"""Compile the production API/store/proxy and exercise security without upstream traffic (macOS)."""
from pathlib import Path
import subprocess
import tempfile
import platform

root = Path(__file__).resolve().parents[1]
harness = r'''
final class MemoryTokens: SessionTokenStorage {
    var values: [String: String] = [:]
    func read(origin: String) -> String { values[origin] ?? "" }
    func write(_ token: String, origin: String) -> Bool { values[origin] = token; return true }
    func clear() { values.removeAll() }
}

final class FixtureProtocol: URLProtocol {
    static var requests: [URLRequest] = []
    static var responses: [(Int, String?)] = []
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        var captured = request
        if captured.httpBody == nil, let stream = captured.httpBodyStream {
            stream.open()
            defer { stream.close() }
            var body = Data(), buffer = [UInt8](repeating: 0, count: 1024)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }
                body.append(buffer, count: count)
            }
            captured.httpBody = body
        }
        Self.requests.append(captured)
        let (status, location) = Self.responses.removeFirst()
        let headers = location.map { ["Location": $0] } ?? [:]
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data("ok".utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

@main struct SecurityTests {
    @MainActor static func main() async throws {
        for raw in ["http://example.com", "https://user:pass@example.com", "https://localhost.",
            "https://x.localhost", "https://x.internal", "https://home.arpa", "https://127.1",
            "https://2130706433", "https://[::1]", "https://[::ffff:127.0.0.1]", "https://100.64.0.1"] {
            precondition(!allow(raw), "Allowed unsafe URL: \(raw)")
        }
        precondition(allow("https://198.18.0.1/video") && allow("https://[2606:4700:4700::1111]/"))
        precondition(normalizedOrigin("https://EXAMPLE.com:443/") == "https://example.com")
        precondition(normalizedOrigin("https://example.com/path") == nil)
        precondition(normalizedOrigin("https://example.com/?query") == nil)
        precondition(!trustedOrigin("https://huangguoai.com.evil.test"))

        let suite = "hghub-security-" + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        defaults.set("legacy-token", forKey: "hg_token")
        let tokens = MemoryTokens()
        let model = AppModel(defaults: defaults, tokens: tokens)
        precondition(!model.loggedIn, "Unbound legacy credentials must require login")
        let user = Account(id: 1, username: "test", nickname: "test", like: 0, collect: 0)
        let initial = model.sessionContext()
        model.setLine("https://huangguoai.ai")
        model.setLine("https://huangguoai.com")
        precondition(!model.setSession(token: "stale", user: user, context: initial), "A→B→A must reject late login")
        let current = model.sessionContext()
        precondition(model.setSession(token: "current", user: user, context: current))
        model.logout(context: initial)
        precondition(model.token == "current", "Old logout must not erase a new session")
        precondition(defaults.string(forKey: "hg_token") == nil, "No plaintext token in preferences")
        let restored = AppModel(defaults: defaults, tokens: tokens)
        precondition(restored.sessionContext().token == "current")
        restored.setLine("https://mirror.example.com")
        precondition(restored.sessionContext().token.isEmpty && !restored.loggedIn)

        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [FixtureProtocol.self]
        let session = URLSession(configuration: configuration, delegate: NoAutomaticRedirects(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        var request = URLRequest(url: URL(string: "https://8.8.8.8/edge")!)
        request.httpMethod = "POST"
        request.httpBody = Data("private-body".utf8)
        for code in [301, 302, 303, 307, 308] {
            FixtureProtocol.responses = [(code, "/leak")]
            let before = FixtureProtocol.requests.count
            do { _ = try await safeData(for: request, session: session); preconditionFailure("Account POST followed a redirect") }
            catch {}
            precondition(FixtureProtocol.requests.count == before + 1)
            precondition(FixtureProtocol.requests.last!.httpBody == request.httpBody)
        }
        FixtureProtocol.responses = [(302, "/next"), (200, nil)]
        let before = FixtureProtocol.requests.count
        let (_, response) = try await safeData(for: request, guestEnvelope: true, session: session)
        precondition(response.statusCode == 200 && FixtureProtocol.requests.count == before + 2)
        precondition(FixtureProtocol.requests.suffix(2).allSatisfy { $0.httpMethod == "POST" && $0.httpBody == request.httpBody })
        request.httpMethod = "GET"
        request.httpBody = nil
        request.setValue("Bearer private-token", forHTTPHeaderField: "Authorization")
        FixtureProtocol.responses = [(307, "/leak")]
        do { _ = try await safeData(for: request, session: session); preconditionFailure("Bearer token followed a redirect") }
        catch {}
        request.setValue(nil, forHTTPHeaderField: "Authorization")
        FixtureProtocol.responses = [(302, "https://127.0.0.1/leak")]
        do { _ = try await safeData(for: request, session: session); preconditionFailure("Followed private redirect") }
        catch {}

        let proxy = MediaProxy()
        precondition(proxy.wrap("https://8.8.8.8/video") == nil)
        proxy.start(referer: "https://huangguoai.com/")
        let wrapped = proxy.wrap("https://8.8.8.8/video")!
        precondition(wrapped.host == "127.0.0.1" && wrapped.path.hasPrefix("/v/") && wrapped.path.count > 30)
        precondition(proxy.wrap("https://127.0.0.1/private") == nil)
        print("PASS: URL policy, session races/persistence, account/guest redirects, private targets and proxy capability")
    }
}
'''
with tempfile.TemporaryDirectory() as tmp:
    source, binary = Path(tmp) / "main.swift", Path(tmp) / "security-tests"
    source.write_text("import Combine\n" + "\n".join((root / "App" / name).read_text() for name in
                      ["Models.swift", "Api.swift", "Store.swift", "Proxy.swift"]) + harness)
    subprocess.run(["xcrun", "swiftc", "-swift-version", "5", "-target", f"{platform.machine()}-apple-macosx13.0",
                    "-parse-as-library", str(source), "-o", str(binary)], check=True)
    subprocess.run([str(binary)], check=True, timeout=60)
