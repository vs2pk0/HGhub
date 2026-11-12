import Darwin
import Foundation
import Combine

/// 本地回环代理：给 AVPlayer 改写 m3u8，并在上游请求上带 Referer。
final class MediaProxy: ObservableObject {
    @Published var port: UInt16 = 0
    var referer = "https://huangguoai.com/"
    private var extraHeaders: [String: String] = [:]
    private var listenFD: Int32 = -1
    private let lock = NSLock()
    private let clients = DispatchSemaphore(value: 8)
    private let route = "/v/" + UUID().uuidString
    private let ua = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"

    func start(referer: String) {
        self.referer = referer
        guard listenFD < 0 else { return }
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return }
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = 0
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        let bound = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard bound == 0, listen(fd, 64) == 0 else {
            close(fd)
            return
        }
        var got = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let named = withUnsafeMutablePointer(to: &got) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &len) }
        }
        guard named == 0 else {
            close(fd)
            return
        }
        listenFD = fd
        port = UInt16(bigEndian: got.sin_port)
        Thread.detachNewThread { [weak self] in
            self?.acceptLoop()
        }
    }

    func updateReferer(_ value: String) {
        lock.lock()
        referer = value
        lock.unlock()
    }

    func updatePlaybackHeaders(_ headers: [String: String]) {
        lock.lock()
        extraHeaders = headers.filter { $0.key.lowercased() == "x-cdn-auth" && !$0.value.isEmpty }
        lock.unlock()
    }

    func wrap(_ remote: String) -> URL? {
        guard allow(remote), port > 0, var parts = URLComponents(string: "http://127.0.0.1:\(port)\(route)") else { return nil }
        parts.queryItems = [URLQueryItem(name: "u", value: remote)]
        return parts.url
    }

    private func currentRequest() -> (referer: String, extra: [String: String]) {
        lock.lock()
        defer { lock.unlock() }
        return (referer, extraHeaders)
    }

    private func acceptLoop() {
        while listenFD >= 0 {
            let fd = accept(listenFD, nil, nil)
            if fd < 0 { continue }
            guard clients.wait(timeout: .now()) == .success else { close(fd); continue }
            Thread.detachNewThread { [weak self] in
                guard let self else { close(fd); return }
                defer { close(fd); self.clients.signal() }
                var timeout = timeval(tv_sec: 10, tv_usec: 0)
                setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
                setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
                self.handle(fd)
            }
        }
    }

    private func handle(_ fd: Int32) {
        var header = Data()
        var buf = [UInt8](repeating: 0, count: 2048)
        while header.count < 64 * 1024 {
            let n = buf.withUnsafeMutableBytes { recv(fd, $0.baseAddress, $0.count, 0) }
            if n <= 0 { break }
            header.append(buf, count: n)
            if header.range(of: Data([13, 10, 13, 10])) != nil { break }
        }
        guard let text = String(data: header, encoding: .utf8),
              let line = text.components(separatedBy: "\r\n").first,
              line.hasPrefix("GET ") || line.hasPrefix("HEAD "),
              let target = line.split(separator: " ").dropFirst().first,
              let url = URL(string: "http://127.0.0.1" + target),
              url.path == route,
              let remote = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first(where: { $0.name == "u" })?.value,
              allow(remote), let remoteURL = URL(string: remote) else {
            write(fd, status: 400, type: "text/plain", body: Data("bad request".utf8))
            return
        }
        let current = currentRequest()
        var req = URLRequest(url: remoteURL)
        req.setValue(ua, forHTTPHeaderField: "User-Agent")
        req.setValue(current.referer, forHTTPHeaderField: "Referer")
        if let page = URL(string: current.referer), let host = page.host, let scheme = page.scheme {
            req.setValue("\(scheme)://\(host)", forHTTPHeaderField: "Origin")
        }
        for (name, value) in current.extra {
            req.setValue(value, forHTTPHeaderField: name)
        }
        req.timeoutInterval = 20
        let sem = DispatchSemaphore(value: 0)
        var data: Data?
        var http: HTTPURLResponse?
        Task {
            if let result = try? await safeData(for: req) {
                data = result.0
                http = result.1
            }
            sem.signal()
        }
        sem.wait()
        guard let data, let http, (200..<300).contains(http.statusCode) else {
            write(fd, status: 502, type: "text/plain", body: Data("upstream".utf8))
            return
        }
        let prefix = String(data: data.prefix(16), encoding: .utf8) ?? ""
        if prefix.contains("#EXTM3U") || (http.value(forHTTPHeaderField: "Content-Type") ?? "").contains("mpegurl") {
            let text = String(data: data, encoding: .utf8) ?? ""
            let rewritten = rewrite(text, base: http.url?.absoluteString ?? remote)
            write(fd, status: 200, type: "application/vnd.apple.mpegurl", body: Data(rewritten.utf8))
        } else {
            let type = http.value(forHTTPHeaderField: "Content-Type") ?? "application/octet-stream"
            write(fd, status: 200, type: type, body: data)
        }
    }

    private func rewrite(_ text: String, base: String) -> String {
        let body = text.hasPrefix("\u{feff}") ? String(text.dropFirst()) : text
        var out: [String] = []
        for raw in body.components(separatedBy: "\n") {
            let line = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if line.isEmpty { out.append(""); continue }
            if line.hasPrefix("#") {
                if line.hasPrefix("#EXT-X-CONTENT-STEERING:") { continue }
                out.append(line.contains("URI=\"") ? rewriteURIs(line, base: base) : line)
            } else {
                let abs = absolute(base, line)
                out.append(wrap(abs)?.absoluteString ?? abs)
            }
        }
        return out.joined(separator: "\n") + "\n"
    }

    private func rewriteURIs(_ line: String, base: String) -> String {
        guard let regex = try? NSRegularExpression(pattern: "URI=\"([^\"]*)\"") else { return line }
        let ns = line as NSString
        let mutable = NSMutableString(string: line)
        for match in regex.matches(in: line, range: NSRange(location: 0, length: ns.length)).reversed() {
            let uri = ns.substring(with: match.range(at: 1))
            let abs = absolute(base, uri)
            let local = wrap(abs)?.absoluteString ?? abs
            mutable.replaceCharacters(in: match.range(at: 0), with: "URI=\"\(local)\"")
        }
        return mutable as String
    }

    private func absolute(_ base: String, _ target: String) -> String {
        if target.hasPrefix("http://") || target.hasPrefix("https://") { return target }
        guard let b = URL(string: base) else { return target }
        return URL(string: target, relativeTo: b)?.absoluteString ?? target
    }

    private func write(_ fd: Int32, status: Int, type: String, body: Data) {
        let head = "HTTP/1.1 \(status) OK\r\nContent-Type: \(type)\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n"
        sendAll(fd, Data(head.utf8))
        sendAll(fd, body)
    }

    private func sendAll(_ fd: Int32, _ data: Data) {
        // 切換分頁會讓播放器關閉連線；此時僅讓 send 回報錯誤，避免 SIGPIPE 終止整個 App。
        var noSigPipe: Int32 = 1
        guard setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigPipe, socklen_t(MemoryLayout<Int32>.size)) == 0 else { return }
        data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            var sent = 0
            while sent < raw.count {
                let n = send(fd, base.advanced(by: sent), raw.count - sent, 0)
                if n <= 0 { break }
                sent += n
            }
        }
    }
}
