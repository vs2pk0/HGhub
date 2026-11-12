import Foundation
import Darwin

enum ApiError: LocalizedError {
    case message(String)
    var errorDescription: String? {
        switch self { case .message(let s): return s }
    }
}

final class HgApi {
    let model: AppModel
    private let ua = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"

    init(model: AppModel) { self.model = model }

    func feed(page: Int) async throws -> ([VideoCard], PageInfo) {
        let data = try await get("/api/videos?page=\(page)&size=8")
        return (data.rows("items").map { $0.card() }, data.page())
    }

    func category(_ cate: String, page: Int, size: Int = 20) async throws -> ([VideoCard], PageInfo) {
        let data = try await get("/api/videos/category/\(enc(cate))?page=\(page)&size=\(size)")
        return (data.rows("items").map { $0.card() }, data.page())
    }

    func search(_ q: String, page: Int) async throws -> ([VideoCard], PageInfo) {
        let data = try await get("/api/search?q=\(enc(q))&page=\(page)&size=20")
        return (data.rows("items").map { $0.card() }, data.page())
    }

    func rank(_ kind: String) async throws -> [RankItem] {
        let k = (kind == "recommend" || kind == "potential") ? kind : "hot"
        return try await get("/api/ranks/\(k)").rows("items").map { $0.rank() }
    }

    func posts(page: Int) async throws -> ([PostItem], PageInfo) {
        let data = try await get("/api/posts?page=\(page)&size=20")
        return (data.rows("items").map { $0.post() }, data.page())
    }

    func post(_ id: Int) async throws -> PostItem { try await get("/api/posts/\(id)").post() }
    func video(_ id: Int) async throws -> Detail { try await get("/api/videos/\(id)").detail() }
    func play(_ id: Int, ep: Int, origin: String? = nil) async throws -> PlayInfo {
        try await call("GET", "/api/videos/\(id)/play?ep=\(ep)&page_texts=0", nil, auth: false, origin: origin).play()
    }

    /// 当前站点的播放地址从这里取，旧的 play 接口会返回空的 video_url。
    func playback(_ id: Int, ep: Int) async throws -> PlayInfo {
        try await call("POST", "/api/videos/\(id)/episodes/\(ep)/playback", [:], auth: false).playback()
    }
    func comments(_ id: Int, ep: Int) async throws -> [CommentItem] {
        try await get("/api/videos/\(id)/comments?ep=\(ep)&page=1&size=20").rows("items").map { $0.comment() }
    }

    func sendComment(_ id: Int, ep: Int, text: String) async throws {
        _ = try await call("POST", "/api/videos/\(id)/comments?ep=\(ep)", ["text": text], auth: true)
    }

    func like(_ id: Int, ep: Int, on: Bool) async throws {
        _ = try await call(on ? "POST" : "DELETE", "/api/videos/\(id)/like?ep=\(ep)", nil, auth: true)
    }

    func collect(_ id: Int, on: Bool) async throws {
        _ = try await call(on ? "POST" : "DELETE", "/api/videos/\(id)/collect", nil, auth: true)
    }

    func login(username: String, password: String) async throws -> Account {
        let context = await model.sessionContext()
        let data = try await call("POST", "/api/account/loginByPassword", ["username": username, "password": password], auth: false, context: context)
        return try await save(data, context: context)
    }

    func register(username: String, password: String, email: String) async throws -> Account {
        let context = await model.sessionContext()
        var body: [String: Any] = ["username": username, "password": password, "confirm_password": password]
        if !email.isEmpty { body["email"] = email }
        let data = try await call("POST", "/api/account/registerByPassword", body, auth: false, context: context)
        return try await save(data, context: context)
    }

    func logout() async {
        let context = await model.sessionContext()
        _ = try? await call("POST", "/api/account/logout", nil, auth: true, context: context)
        await model.logout(context: context)
    }

    func mirrors() async throws -> [String] {
        let page = "https://huangguoai.pages.dev/"
        let html = try await text(page)
        let src = scriptSrc(html) ?? "publish.js"
        let jsURL = src.hasPrefix("http") ? src : page + src.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let js = try await text(jsURL)
        let domains = jsArray(js, "var urls")
        let subs = jsArray(js, "var subdomains")
        if domains.isEmpty { throw ApiError.message("没有找到镜像域名") }
        var out: [String] = []
        for domain in domains {
            let host = domain.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            let names = subs.isEmpty ? [host] : subs.map { "\($0).\(host)" }
            for name in names {
                let url = "https://\(name)"
                if let origin = normalizedOrigin(url), !out.contains(origin) { out.append(origin) }
                if out.count >= 40 { return out }
            }
        }
        if out.isEmpty { throw ApiError.message("没有可用镜像") }
        return out
    }

    func bytes(_ url: String) async throws -> Data {
        guard allow(url) else { throw ApiError.message("地址不可用") }
        let origin = await model.base
        var req = URLRequest(url: URL(string: url)!)
        req.setValue(ua, forHTTPHeaderField: "User-Agent")
        req.setValue(origin + "/", forHTTPHeaderField: "Referer")
        req.timeoutInterval = 20
        let (data, resp) = try await safeData(for: req)
        guard let http = resp as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw ApiError.message("封面加载失败")
        }
        return data
    }

    private func get(_ path: String) async throws -> [String: Any] {
        try await call("GET", path, nil, auth: false)
    }

    private func call(_ method: String, _ path: String, _ body: [String: Any]?, auth: Bool, origin override: String? = nil, context: SessionContext? = nil) async throws -> [String: Any] {
        let captured: SessionContext
        if let context { captured = context } else { captured = await model.sessionContext() }
        guard let origin = normalizedOrigin(override ?? captured.origin) else { throw ApiError.message("线路地址无效") }
        let token = captured.token
        let accountRequest = auth || path == "/api/account/loginByPassword" || path == "/api/account/registerByPassword"
        if accountRequest && (!trustedOrigin(origin) || origin != captured.origin) { throw ApiError.message("镜像线路仅供游客浏览，登录请切换到固定线路") }
        if auth && token.isEmpty { throw ApiError.message("请先登录") }
        var encoded = ""
        var ctype = ""
        if let body {
            let raw = try JSONSerialization.data(withJSONObject: body)
            encoded = raw.base64EncodedString()
            ctype = "application/json"
        }
        let envelope: [String: Any] = ["method": method, "path": path, "body": encoded, "content_type": ctype]
        var req = URLRequest(url: URL(string: origin + "/api/edge/request")!)
        req.httpMethod = "POST"
        req.httpBody = try JSONSerialization.data(withJSONObject: envelope)
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        req.setValue(ua, forHTTPHeaderField: "User-Agent")
        req.setValue(origin + "/", forHTTPHeaderField: "Referer")
        if auth && !token.isEmpty { req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        req.timeoutInterval = 20
        let (data, resp) = try await safeData(for: req, guestEnvelope: method == "GET" && body == nil && !auth)
        let code = (resp as? HTTPURLResponse)?.statusCode ?? 0
        if code == 401 || code == 403 { throw ApiError.message("请先登录") }
        if !(200..<300).contains(code) { throw ApiError.message("服务器请求失败（HTTP \(code)），请稍后重试或切换线路") }
        if data.isEmpty { return [:] }
        guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw ApiError.message("接口返回无法解析")
        }
        if (obj["status"] as? NSNumber)?.intValue == 1 || (obj["status"] as? Int) == 1 {
            return obj["data"] as? [String: Any] ?? [:]
        }
        let msg = (obj["msg"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "接口返回失败"
        throw ApiError.message(msg)
    }

    private func save(_ data: [String: Any], context: SessionContext) async throws -> Account {
        let token = data.str("token")
        guard !token.isEmpty, let user = data.child("user")?.account() else {
            throw ApiError.message("登录返回不完整")
        }
        guard await model.setSession(token: token, user: user, context: context) else {
            throw ApiError.message("登录期间线路或会话已变化，请重新登录")
        }
        return user
    }

    private func text(_ url: String) async throws -> String {
        guard allow(url), let u = URL(string: url) else { throw ApiError.message("地址不可用") }
        var req = URLRequest(url: u)
        req.setValue(ua, forHTTPHeaderField: "User-Agent")
        req.timeoutInterval = 15
        let (data, resp) = try await safeData(for: req)
        guard let http = resp as? HTTPURLResponse, (200..<300).contains(http.statusCode),
              let text = String(data: data, encoding: .utf8) else {
            throw ApiError.message("镜像清单不可用")
        }
        return text
    }

    private func enc(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? s
    }
}

func trustedOrigin(_ origin: String) -> Bool {
    origin == "https://huangguoai.com" || origin == "https://huangguoai.ai"
}

func normalizedOrigin(_ raw: String) -> String? {
    guard allow(raw), let url = URL(string: raw),
          url.path.isEmpty || url.path == "/", url.query == nil, url.fragment == nil,
          let parts = URLComponents(url: url, resolvingAgainstBaseURL: false), let host = parts.host else { return nil }
    var origin = URLComponents()
    origin.scheme = "https"
    origin.host = host.lowercased()
    origin.port = parts.port == 443 ? nil : parts.port
    return origin.string
}

func publicIP(_ host: String) -> Bool? {
    let host = host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
    var v4 = in_addr()
    if inet_aton(host, &v4) == 1 {
        let b = withUnsafeBytes(of: &v4) { Array($0) }
        let (a, second, c) = (Int(b[0]), Int(b[1]), Int(b[2]))
        if a == 198 && (18...19).contains(second) { return true }
        return !(a == 0 || a == 10 || a == 127 || a >= 224 ||
            (a == 100 && (64...127).contains(second)) || (a == 169 && second == 254) ||
            (a == 172 && (16...31).contains(second)) || (a == 192 && second == 168) ||
            (a == 192 && second == 0 && (c == 0 || c == 2)) || (a == 192 && second == 88 && c == 99) ||
            (a == 198 && second == 51 && c == 100) || (a == 203 && second == 0 && c == 113))
    }
    var v6 = in6_addr()
    if inet_pton(AF_INET6, host, &v6) == 1 {
        let b = withUnsafeBytes(of: &v6) { Array($0) }
        if b.prefix(10).allSatisfy({ $0 == 0 }) && b[10] == 255 && b[11] == 255 {
            return publicIP(b.suffix(4).map { String($0) }.joined(separator: "."))
        }
        let first = Int(b[0]) * 256 + Int(b[1]), second = Int(b[2]) * 256 + Int(b[3])
        return first & 0xe000 == 0x2000 && first != 0x2002 &&
            !(first == 0x2001 && (second < 0x0200 || second == 0x0db8)) &&
            !(first == 0x3fff && second < 0x1000)
    }
    return nil
}

func allow(_ raw: String) -> Bool {
    guard !raw.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
          let url = URL(string: raw), url.scheme?.lowercased() == "https", url.user == nil, url.password == nil,
          let host = url.host?.lowercased(), !host.isEmpty else { return false }
    let name = host.trimmingCharacters(in: CharacterSet(charactersIn: "."))
    if ["localhost", "local", "internal", "home.arpa"].contains(where: { name == $0 || name.hasSuffix("." + $0) }) { return false }
    if let allowed = publicIP(host) { return allowed }
    return !host.contains(":") && !host.contains("%")
}

func checkDNS(_ url: URL) async throws {
    guard let host = url.host else { throw ApiError.message("目标没有主机名") }
    if let allowed = publicIP(host) {
        if !allowed { throw ApiError.message("禁止访问内网地址") }
        return
    }
    try await Task.detached {
        var result: UnsafeMutablePointer<addrinfo>?
        guard getaddrinfo(host, nil, nil, &result) == 0, let first = result else { throw ApiError.message("DNS 解析失败") }
        defer { freeaddrinfo(first) }
        var current: UnsafeMutablePointer<addrinfo>? = first
        while let entry = current {
            let address = entry.pointee
            var name = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            guard getnameinfo(address.ai_addr, address.ai_addrlen, &name, socklen_t(name.count), nil, 0, NI_NUMERICHOST) == 0,
                  publicIP(String(cString: name)) == true else { throw ApiError.message("DNS 包含内网或保留地址") }
            current = address.ai_next
        }
    }.value
}

final class NoAutomaticRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

let safeSession: URLSession = {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.httpShouldSetCookies = false
    configuration.httpCookieStorage = nil
    configuration.urlCredentialStorage = nil
    configuration.urlCache = nil
    configuration.timeoutIntervalForResource = 30
    return URLSession(configuration: configuration, delegate: NoAutomaticRedirects(), delegateQueue: nil)
}()

func safeData(for request: URLRequest, guestEnvelope: Bool = false, session: URLSession = safeSession) async throws -> (Data, HTTPURLResponse) {
    var next = request
    for hop in 0...5 {
        try Task.checkCancellation()
        guard let url = next.url, allow(url.absoluteString) else { throw ApiError.message("目标地址不可用") }
        try await checkDNS(url)
        let (data, response) = try await session.data(for: next)
        guard let http = response as? HTTPURLResponse else { throw ApiError.message("响应无效") }
        if ![301, 302, 303, 307, 308].contains(http.statusCode) { return (data, http) }
        let canFollow = next.value(forHTTPHeaderField: "Authorization") == nil &&
            (next.httpMethod == "GET" || (guestEnvelope && [301, 302, 303].contains(http.statusCode)))
        guard canFollow, hop < 5, let location = http.value(forHTTPHeaderField: "Location"),
              let target = URL(string: location, relativeTo: url)?.absoluteURL, allow(target.absoluteString) else {
            throw ApiError.message("不安全的业务重定向已拒绝")
        }
        // Keep the logical GET's POST envelope intact.
        next.url = target
    }
    throw ApiError.message("重定向次数超限")
}

private func scriptSrc(_ html: String) -> String? {
    var rest = html
    while let range = rest.range(of: "src=\"") {
        let after = rest[range.upperBound...]
        guard let end = after.firstIndex(of: "\"") else { return nil }
        let candidate = String(after[..<end])
        if candidate.contains(".js") { return candidate }
        rest = String(after[end...])
    }
    return nil
}

private func jsArray(_ src: String, _ decl: String) -> [String] {
    guard let at = src.range(of: decl) else { return [] }
    let rest = src[at.lowerBound...]
    guard let open = rest.firstIndex(of: "["), let close = rest[open...].firstIndex(of: "]") else { return [] }
    let body = rest[rest.index(after: open)..<close]
    return body.split(separator: ",").map { item in
        item.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
    }.filter { !$0.isEmpty && !$0.hasPrefix("//") }
}
