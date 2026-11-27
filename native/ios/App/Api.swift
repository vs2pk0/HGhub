import CryptoKit
import Foundation

enum ApiError: LocalizedError {
    case message(String)
    var errorDescription: String? {
        switch self { case .message(let s): return s }
    }
}

final class HgApi {
    let model: AppModel
    private let ua = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"
    private let edge = EdgeVault()

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
        try await ensureTrusted()
        let data = try await call("POST", "/api/account/loginByPassword", ["username": username, "password": password], auth: false)
        return try await save(data)
    }

    func register(username: String, password: String, email: String) async throws -> Account {
        try await ensureTrusted()
        var body: [String: Any] = ["username": username, "password": password, "confirm_password": password]
        if !email.isEmpty { body["email"] = email }
        let data = try await call("POST", "/api/account/registerByPassword", body, auth: false)
        return try await save(data)
    }

    func logout() async {
        _ = try? await call("POST", "/api/account/logout", nil, auth: true)
        await model.logout()
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
                if allow(url) { out.append(url) }
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
        let (data, resp) = try await URLSession.shared.data(for: req)
        guard let http = resp as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw ApiError.message("封面加载失败")
        }
        return data
    }

    private func get(_ path: String) async throws -> [String: Any] {
        try await call("GET", path, nil, auth: false)
    }

    private func call(_ method: String, _ path: String, _ body: [String: Any]?, auth: Bool, origin override: String? = nil) async throws -> [String: Any] {
        let current = await model.base
        let origin = (override ?? current).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let token = await model.token
        guard allow(origin) else { throw ApiError.message("线路地址无效") }
        if auth && token.isEmpty { throw ApiError.message("请先登录") }
        var encoded = ""
        var ctype = ""
        if let body {
            let raw = try JSONSerialization.data(withJSONObject: body)
            encoded = raw.base64EncodedString()
            ctype = "application/json"
        }
        let scope = try requestScope(method, path)
        let data = try await transport(origin: origin, scope: scope, method: method, path: path, body: encoded, contentType: ctype, token: auth ? token : "")
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

    private func save(_ data: [String: Any]) async throws -> Account {
        let token = data.str("token")
        guard !token.isEmpty, let user = data.child("user")?.account() else {
            throw ApiError.message("登录返回不完整")
        }
        await model.setSession(token: token, user: user)
        return user
    }

    private func ensureTrusted() async throws {
        let ok = await model.trustedLine
        if !ok { throw ApiError.message("镜像线路仅供游客浏览，登录请切换到固定线路") }
    }

    private func text(_ url: String) async throws -> String {
        guard allow(url), let u = URL(string: url) else { throw ApiError.message("地址不可用") }
        var req = URLRequest(url: u)
        req.setValue(ua, forHTTPHeaderField: "User-Agent")
        req.timeoutInterval = 15
        let (data, resp) = try await URLSession.shared.data(for: req)
        guard let http = resp as? HTTPURLResponse, (200..<300).contains(http.statusCode),
              let text = String(data: data, encoding: .utf8) else {
            throw ApiError.message("镜像清单不可用")
        }
        return text
    }

    private func enc(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? s
    }

    /// 旧信封已停用。公开读走明文 JSON，其余走加密通道，返回业务 JSON 正文。
    private func transport(origin: String, scope: String, method: String, path: String, body: String, contentType: String, token: String) async throws -> Data {
        if scope == "public" {
            if let data = try await EdgeHTTP.publicJSON(origin: origin, ua: ua, method: method, path: path, body: body, contentType: contentType) {
                return data
            }
        }
        let packetPath = scope == "public" ? canonicalPath(path) : path
        return try await edge.call(origin: origin, ua: ua, scope: scope, method: method, path: packetPath, body: body, contentType: contentType, token: scope == "public" ? "" : token)
    }
}

func allow(_ raw: String) -> Bool {
    guard let url = URL(string: raw), url.scheme?.lowercased() == "https", let host = url.host?.lowercased(), !host.isEmpty else { return false }
    if host == "localhost" || host.hasSuffix(".local") { return false }
    let parts = host.split(separator: ".").compactMap { Int($0) }
    guard parts.count == 4 else { return true }
    if parts[0] == 10 || parts[0] == 127 || parts[0] == 0 { return false }
    if parts[0] == 192 && parts[1] == 168 { return false }
    if parts[0] == 172 && (16...31).contains(parts[1]) { return false }
    if parts[0] == 169 && parts[1] == 254 { return false }
    return true
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

private func requestScope(_ method: String, _ path: String) throws -> String {
    var verb = method.uppercased()
    if verb == "HEAD" { verb = "GET" }
    let pathname = path.split(separator: "?", maxSplits: 1).first.map(String.init) ?? path
    let segments = pathname.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
    for row in edgeRoutes where row.method == verb {
        let parts = row.pattern.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        guard parts.count == segments.count else { continue }
        let matched = zip(parts, segments).allSatisfy { pair in
            let pattern = pair.0
            let segment = pair.1
            if pattern.hasPrefix(":") {
                let name = String(pattern.dropFirst())
                if name == "id" || name == "cid" || name == "ep" {
                    return !segment.isEmpty && segment.all(\.isNumber)
                }
                return !segment.isEmpty
            }
            return pattern == segment
        }
        guard matched else { continue }
        if row.scope == "public", let query = path.split(separator: "?", maxSplits: 1).dropFirst().first {
            let random = query.split(separator: "&").contains { item in
                let pair = item.split(separator: "=", maxSplits: 1)
                return pair.first == "sort" && (pair.dropFirst().first?.caseInsensitiveCompare("random") == .orderedSame || pair.dropFirst().first?.caseInsensitiveCompare("rand") == .orderedSame)
            }
            if random { return "dynamic" }
        }
        return row.scope
    }
    throw ApiError.message("接口未配置加密通道")
}

private func canonicalPath(_ path: String) -> String {
    guard let q = path.firstIndex(of: "?") else { return path }
    let pathname = String(path[..<q])
    let pairs = path[path.index(after: q)...].split(separator: "&").filter { !$0.isEmpty }.map { item -> (String, String) in
        let parts = item.split(separator: "=", maxSplits: 1).map(String.init)
        return (parts.first ?? "", parts.dropFirst().first ?? "")
    }.sorted { $0.0 == $1.0 ? $0.1 < $1.1 : $0.0 < $1.0 }
    if pairs.isEmpty { return pathname }
    return pathname + "?" + pairs.map { "\($0.0)=\($0.1)" }.joined(separator: "&")
}

private struct EdgeRoute {
    let method: String
    let scope: String
    let pattern: String
    let dynamic: Int
}

private let edgeRoutes: [EdgeRoute] = {
    let table: [(String, String, String)] = [
        ("POST", "dynamic", "/api/edge/image"),
        ("GET", "dynamic", "/api/home"),
        ("POST", "dynamic", "/api/media/covers"),
        ("GET", "dynamic", "/api/posts/:id"),
        ("GET", "dynamic", "/api/posts/:id/comments"),
        ("POST", "dynamic", "/api/posts/:id/share"),
        ("GET", "dynamic", "/api/search"),
        ("GET", "dynamic", "/api/site/address-poster"),
        ("POST", "dynamic", "/api/site/click"),
        ("GET", "dynamic", "/api/site/entries"),
        ("GET", "dynamic", "/api/site/go-home"),
        ("POST", "dynamic", "/api/site/home-slots"),
        ("POST", "dynamic", "/api/stats/click"),
        ("POST", "dynamic", "/api/stats/origin"),
        ("GET", "dynamic", "/api/users/:id"),
        ("GET", "dynamic", "/api/videos/:id"),
        ("GET", "dynamic", "/api/videos/:id/comments"),
        ("GET", "dynamic", "/api/videos/:id/comments/:cid/replies"),
        ("GET", "dynamic", "/api/videos/:id/next"),
        ("POST", "dynamic", "/api/videos/:id/play"),
        ("POST", "dynamic", "/api/videos/:id/episode-stats"),
        ("POST", "dynamic", "/api/videos/:id/visit"),
        ("POST", "dynamic", "/api/videos/recommend"),
        ("POST", "dynamic", "/api/videos/recommend/ref/:ref"),
        ("GET", "dynamic", "/api/whoami"),
        ("POST", "playback", "/api/media/playback/previews"),
        ("POST", "playback", "/api/media/prerolls/:id/playback"),
        ("POST", "playback", "/api/posts/:id/playback"),
        ("POST", "playback", "/api/videos/:id/episodes/:ep/playback"),
        ("GET", "playback", "/api/videos/:id/play"),
        ("POST", "playback", "/api/videos/:id/playback"),
        ("GET", "public", "/api/feed/post/today"),
        ("GET", "public", "/api/feed/recommend"),
        ("GET", "public", "/api/feed/today"),
        ("GET", "public", "/api/home/latest"),
        ("GET", "public", "/api/home/recommends"),
        ("GET", "public", "/api/posts"),
        ("GET", "public", "/api/posts/:id/adjacent"),
        ("GET", "public", "/api/posts/:id/related"),
        ("GET", "public", "/api/posts/category/:route"),
        ("GET", "public", "/api/posts/tag/:tag"),
        ("GET", "public", "/api/posts/user/:id"),
        ("GET", "public", "/api/ranks/:type"),
        ("GET", "public", "/api/search/hot"),
        ("GET", "public", "/api/search/suggest"),
        ("GET", "public", "/api/search/tags/hot"),
        ("GET", "public", "/api/site/categories/post"),
        ("GET", "public", "/api/site/categories/video"),
        ("GET", "public", "/api/site/config"),
        ("GET", "public", "/api/site/links"),
        ("GET", "public", "/api/site/menus"),
        ("GET", "public", "/api/site/meta"),
        ("GET", "public", "/api/site/pages"),
        ("GET", "public", "/api/site/pages/:slug"),
        ("GET", "public", "/api/site/sorts/video"),
        ("GET", "public", "/api/site/transit"),
        ("GET", "public", "/api/site/watermark"),
        ("GET", "public", "/api/tags"),
        ("GET", "public", "/api/topics"),
        ("GET", "public", "/api/topics/:slug"),
        ("GET", "public", "/api/users/:id/stats"),
        ("GET", "public", "/api/videos"),
        ("GET", "public", "/api/videos/:id/related"),
        ("GET", "public", "/api/videos/category/:route"),
        ("GET", "public", "/api/videos/long"),
        ("GET", "public", "/api/videos/report-reasons"),
        ("GET", "public", "/api/videos/sort/:route"),
        ("GET", "public", "/api/videos/tag/:tag"),
        ("GET", "public", "/api/videos/user/:id"),
        ("POST", "user", "/api/account/loginByPassword"),
        ("POST", "user", "/api/account/logout"),
        ("GET", "user", "/api/account/me"),
        ("POST", "user", "/api/account/password"),
        ("PATCH", "user", "/api/account/profile"),
        ("POST", "user", "/api/account/registerByPassword"),
        ("POST", "user", "/api/ai-wish"),
        ("DELETE", "user", "/api/posts/:id/collect"),
        ("POST", "user", "/api/posts/:id/collect"),
        ("POST", "user", "/api/posts/:id/comments"),
        ("POST", "user", "/api/posts/:id/comments/:cid/guest-like"),
        ("DELETE", "user", "/api/posts/:id/comments/:cid/like"),
        ("POST", "user", "/api/posts/:id/comments/:cid/like"),
        ("GET", "user", "/api/posts/:id/comments/liked"),
        ("POST", "user", "/api/posts/:id/guest-comments"),
        ("POST", "user", "/api/posts/:id/guest-like"),
        ("GET", "user", "/api/posts/:id/guest-liked"),
        ("DELETE", "user", "/api/posts/:id/like"),
        ("POST", "user", "/api/posts/:id/like"),
        ("POST", "user", "/api/posts/:id/visit"),
        ("DELETE", "user", "/api/users/:id/follow"),
        ("POST", "user", "/api/users/:id/follow"),
        ("GET", "user", "/api/users/me/collects"),
        ("POST", "user", "/api/users/me/collects/delete"),
        ("GET", "user", "/api/users/me/comments"),
        ("GET", "user", "/api/users/me/follows"),
        ("GET", "user", "/api/users/me/history"),
        ("POST", "user", "/api/users/me/history"),
        ("POST", "user", "/api/users/me/history/delete"),
        ("POST", "user", "/api/users/me/history/sync"),
        ("GET", "user", "/api/users/me/likes"),
        ("POST", "user", "/api/users/me/likes/delete"),
        ("POST", "user", "/api/users/me/likes/sync"),
        ("GET", "user", "/api/users/me/messages"),
        ("DELETE", "user", "/api/users/me/messages/:id"),
        ("POST", "user", "/api/users/me/messages/:id/read"),
        ("POST", "user", "/api/users/me/messages/batch-delete"),
        ("POST", "user", "/api/users/me/messages/read-all"),
        ("GET", "user", "/api/users/me/messages/unread-count"),
        ("DELETE", "user", "/api/videos/:id/collect"),
        ("POST", "user", "/api/videos/:id/collect"),
        ("POST", "user", "/api/videos/:id/comments"),
        ("POST", "user", "/api/videos/:id/comments/:cid/guest-like"),
        ("DELETE", "user", "/api/videos/:id/comments/:cid/like"),
        ("POST", "user", "/api/videos/:id/comments/:cid/like"),
        ("GET", "user", "/api/videos/:id/comments/liked"),
        ("GET", "user", "/api/videos/:id/comments/state"),
        ("POST", "user", "/api/videos/:id/feedback"),
        ("POST", "user", "/api/videos/:id/guest-comments"),
        ("POST", "user", "/api/videos/:id/guest-like"),
        ("GET", "user", "/api/videos/:id/guest-liked"),
        ("GET", "user", "/api/videos/:id/interaction-state"),
        ("DELETE", "user", "/api/videos/:id/like"),
        ("POST", "user", "/api/videos/:id/like"),
        ("GET", "user", "/api/videos/:id/rating"),
        ("POST", "user", "/api/videos/:id/rating"),
        ("POST", "user", "/api/videos/:id/report"),
        ("GET", "user", "/api/videos/following"),
    ]
    return table.map { EdgeRoute(method: $0.0, scope: $0.1, pattern: $0.2, dynamic: $0.2.split(separator: "/").filter { $0.hasPrefix(":") }.count) }
        .sorted { lhs, rhs in
            if lhs.method == rhs.method { return lhs.dynamic < rhs.dynamic }
            return lhs.method < rhs.method
        }
}()

private enum EdgeHTTP {
    static func publicJSON(origin: String, ua: String, method: String, path: String, body: String, contentType: String) async throws -> Data? {
        let packet: [String: Any] = ["method": method, "path": canonicalPath(path), "body": body, "content_type": contentType]
        let posted = try await post(url: origin + "/api/public/request", json: packet, ua: ua, referer: origin + "/", extra: ["X-HG-Public-JSON": "1"])
        if posted.code == 400 && posted.marker != "1" {
            let error = (try? JSONSerialization.jsonObject(with: posted.data) as? [String: Any])?["error"] as? String ?? ""
            if error == "invalid_request" || error == "unsupported_version" { return nil }
        }
        if !(200..<300).contains(posted.code) { throw ApiError.message(httpError(posted.code, posted.data)) }
        return posted.data
    }

    static func post(url: String, json: [String: Any], ua: String, referer: String, extra: [String: String] = [:]) async throws -> (code: Int, marker: String?, data: Data) {
        var req = URLRequest(url: URL(string: url)!)
        req.httpMethod = "POST"
        req.httpBody = try JSONSerialization.data(withJSONObject: json)
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        req.setValue(ua, forHTTPHeaderField: "User-Agent")
        req.setValue(referer, forHTTPHeaderField: "Referer")
        extra.forEach { req.setValue($0.value, forHTTPHeaderField: $0.key) }
        req.timeoutInterval = 20
        let (data, resp) = try await URLSession.shared.data(for: req)
        let http = resp as? HTTPURLResponse
        let code = http?.statusCode ?? 0
        if (300..<400).contains(code) { throw ApiError.message("业务接口重定向已拒绝") }
        return (code, http?.value(forHTTPHeaderField: "x-hg-public-json"), data)
    }

    static func httpError(_ status: Int, _ data: Data) -> String {
        if status == 401 || status == 403 { return "请先登录" }
        if let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let msg = obj["msg"] as? String, !msg.isEmpty {
            return msg
        }
        return "服务器请求失败（HTTP \(status)），请稍后重试或切换线路"
    }
}

private struct EdgeMaterial {
    let info: String
    let kid: String
    let sid: String
    let sequence: UInt32
    let timestamp: Int64
    let requestKey: SymmetricKey
    let responseKey: SymmetricKey
}

private struct EdgeSession {
    var info: String
    var kid: String
    var sid: String
    var clockOffset: Int64
    var expiresAt: Int64
    var sequence: UInt32 = 0
    var requestKey: SymmetricKey
    var responseKey: SymmetricKey

    func stale() -> Bool {
        expiresAt <= Int64(Date().timeIntervalSince1970) + 5 || sequence >= 16384
    }

    func material() -> EdgeMaterial {
        EdgeMaterial(info: info, kid: kid, sid: sid, sequence: sequence, timestamp: Int64(Date().timeIntervalSince1970) + clockOffset, requestKey: requestKey, responseKey: responseKey)
    }
}

private actor EdgeVault {
    private var sessions: [String: EdgeSession] = [:]

    func call(origin: String, ua: String, scope: String, method: String, path: String, body: String, contentType: String, token: String) async throws -> Data {
        var packet: [String: Any] = [
            "request": ["method": method, "path": path, "body": body, "content_type": contentType],
        ]
        if !token.isEmpty { packet["authorization"] = "Bearer \(token)" }
        let plain = try JSONSerialization.data(withJSONObject: packet)
        for _ in 0..<2 {
            let material = try await material(origin: origin, ua: ua, scope: scope)
            let iv = nonce(material.sequence)
            let cipher = try seal(material.requestKey, iv, aad(material.info, "req", material.sequence, material.timestamp), plain)
            let envelope: [String: Any] = [
                "v": 1, "type": "call", "kid": material.kid, "sid": material.sid,
                "seq": material.sequence, "ts": material.timestamp, "ciphertext": b64url(cipher),
            ]
            let posted = try await EdgeHTTP.post(url: origin + "/api/\(scope)/request", json: envelope, ua: ua, referer: origin + "/")
            if !(200..<300).contains(posted.code) {
                let error = (try? JSONSerialization.jsonObject(with: posted.data) as? [String: Any])?["error"] as? String ?? ""
                if error == "session_expired" {
                    invalidate(origin: origin, scope: scope, sid: material.sid)
                    continue
                }
                throw ApiError.message("加密接口请求失败（\(posted.code)）")
            }
            guard let result = try JSONSerialization.jsonObject(with: posted.data) as? [String: Any],
                  (result["v"] as? NSNumber)?.intValue == 1,
                  result["type"] as? String == "result",
                  result["sid"] as? String == material.sid,
                  result["kid"] as? String == material.kid,
                  (result["seq"] as? NSNumber)?.uint32Value == material.sequence,
                  (result["ts"] as? NSNumber)?.int64Value == material.timestamp,
                  let encoded = result["ciphertext"] as? String else {
                throw ApiError.message("加密接口响应不匹配")
            }
            let opened = try open(material.responseKey, iv, aad(material.info, "res", material.sequence, material.timestamp), b64urlDecode(encoded))
            return try businessData(opened)
        }
        throw ApiError.message("加密接口请求失败")
    }

    private func material(origin: String, ua: String, scope: String) async throws -> EdgeMaterial {
        let key = origin + "\n" + scope
        var current: EdgeSession
        if let existing = sessions[key], !existing.stale() {
            current = existing
        } else {
            current = try await handshake(origin: origin, ua: ua, scope: scope)
        }
        current.sequence += 1
        if current.sequence > 16384 { throw ApiError.message("加密会话已到期") }
        sessions[key] = current
        return current.material()
    }

    private func invalidate(origin: String, scope: String, sid: String) {
        let key = origin + "\n" + scope
        if sessions[key]?.sid == sid { sessions[key] = nil }
    }

    private func handshake(origin: String, ua: String, scope: String) async throws -> EdgeSession {
        let privateKey = P256.KeyAgreement.PrivateKey()
        let hello: [String: Any] = ["v": 1, "type": "hello", "client_pub": b64url(privateKey.publicKey.x963Representation)]
        let posted = try await EdgeHTTP.post(url: origin + "/api/\(scope)/request", json: hello, ua: ua, referer: origin + "/")
        guard (200..<300).contains(posted.code),
              let obj = try JSONSerialization.jsonObject(with: posted.data) as? [String: Any] else {
            throw ApiError.message("加密握手失败")
        }
        let kid = obj["kid"] as? String ?? ""
        let sid = obj["sid"] as? String ?? ""
        let site = obj["site"] as? String ?? ""
        let serverTime = (obj["server_time"] as? NSNumber)?.int64Value ?? 0
        let expiresAt = (obj["expires_at"] as? NSNumber)?.int64Value ?? 0
        guard (obj["v"] as? NSNumber)?.intValue == 1, obj["type"] as? String == "hello",
              tokenOk(kid, 1, 64), tokenOk(sid, 16, 128), siteOk(site),
              expiresAt > serverTime, expiresAt - serverTime <= 600,
              let serverPub = obj["server_pub"] as? String, let saltText = obj["salt"] as? String else {
            throw ApiError.message("加密握手响应无效")
        }
        let serverBytes = try b64urlDecode(serverPub)
        let salt = try b64urlDecode(saltText)
        guard salt.count == 32, serverBytes.count == 65, serverBytes.first == 4 else {
            throw ApiError.message("加密握手响应无效")
        }
        let serverKey = try P256.KeyAgreement.PublicKey(x963Representation: serverBytes)
        let shared = try privateKey.sharedSecretFromKeyAgreement(with: serverKey)
        let info = [protocolName, scope, site, kid, sid].joined(separator: "\n")
        let local = Int64(Date().timeIntervalSince1970)
        return EdgeSession(
            info: info, kid: kid, sid: sid, clockOffset: serverTime - local,
            expiresAt: local + expiresAt - serverTime,
            requestKey: shared.hkdfDerivedSymmetricKey(using: SHA256.self, salt: salt, sharedInfo: Data((info + "\nreq").utf8), outputByteCount: 32),
            responseKey: shared.hkdfDerivedSymmetricKey(using: SHA256.self, salt: salt, sharedInfo: Data((info + "\nres").utf8), outputByteCount: 32)
        )
    }
}

private let protocolName = "dx150-edge-v1"

private func nonce(_ sequence: UInt32) -> Data {
    var iv = Data(count: 12)
    iv[8] = UInt8((sequence >> 24) & 0xff)
    iv[9] = UInt8((sequence >> 16) & 0xff)
    iv[10] = UInt8((sequence >> 8) & 0xff)
    iv[11] = UInt8(sequence & 0xff)
    return iv
}

private func aad(_ info: String, _ direction: String, _ sequence: UInt32, _ timestamp: Int64) -> Data {
    Data("\(info)\n\(direction)\n\(sequence)\n\(timestamp)".utf8)
}

private func seal(_ key: SymmetricKey, _ iv: Data, _ aad: Data, _ plain: Data) throws -> Data {
    let box = try AES.GCM.seal(plain, using: key, nonce: AES.GCM.Nonce(data: iv), authenticating: aad)
    return box.ciphertext + box.tag
}

private func open(_ key: SymmetricKey, _ iv: Data, _ aad: Data, _ cipherText: Data) throws -> Data {
    guard cipherText.count > 16 else { throw ApiError.message("加密响应无法解密") }
    let box = try AES.GCM.SealedBox(nonce: AES.GCM.Nonce(data: iv), ciphertext: cipherText.prefix(cipherText.count - 16), tag: cipherText.suffix(16))
    return try AES.GCM.open(box, using: key, authenticating: aad)
}

private func businessData(_ plain: Data) throws -> Data {
    guard let inner = try JSONSerialization.jsonObject(with: plain) as? [String: Any],
          let status = (inner["status"] as? NSNumber)?.intValue, (200...599).contains(status),
          inner["headers"] is [String: Any] else {
        throw ApiError.message("加密接口响应格式无效")
    }
    let text: Data
    if status == 204 || status == 205 || status == 304 {
        text = Data()
    } else if let body = inner["body"] as? String, !body.isEmpty {
        guard let decoded = Data(base64Encoded: body) else { throw ApiError.message("接口正文编码无效") }
        text = decoded
    } else {
        text = Data()
    }
    if !(200..<300).contains(status) { throw ApiError.message(EdgeHTTP.httpError(status, text)) }
    return text
}

private func b64url(_ data: Data) -> String {
    data.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
}

private func b64urlDecode(_ value: String) throws -> Data {
    guard value.range(of: #"^[A-Za-z0-9_-]+$"#, options: .regularExpression) != nil else {
        throw ApiError.message("加密响应编码无效")
    }
    var text = value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
    let pad = (4 - text.count % 4) % 4
    text += String(repeating: "=", count: pad)
    guard let data = Data(base64Encoded: text) else { throw ApiError.message("加密响应编码无效") }
    return data
}

private func tokenOk(_ value: String, _ min: Int, _ max: Int) -> Bool {
    (min...max).contains(value.count) && value.allSatisfy { $0.isLetter || $0.isNumber || $0 == "_" || $0 == "-" }
}

private func siteOk(_ value: String) -> Bool {
    (1...256).contains(value.count) && value.allSatisfy { $0.isLetter || $0.isNumber || $0 == "." || $0 == ":" || $0 == "[" || $0 == "]" || $0 == "-" }
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
