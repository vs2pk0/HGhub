import Foundation
import Combine
import Security

protocol SessionTokenStorage {
    func read(origin: String) -> String
    func write(_ token: String, origin: String) -> Bool
    func clear()
}

final class KeychainTokens: SessionTokenStorage {
    private let service = (Bundle.main.bundleIdentifier ?? "app.hghub.mobile") + ".session.v2"
    private func query(_ origin: String? = nil) -> [String: Any] {
        var value: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service]
        if let origin { value[kSecAttrAccount as String] = origin }
        return value
    }
    func read(origin: String) -> String {
        var request = query(origin)
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(request as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data else { return "" }
        return String(data: data, encoding: .utf8) ?? ""
    }
    func write(_ token: String, origin: String) -> Bool {
        let attributes: [String: Any] = [kSecValueData as String: Data(token.utf8),
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly]
        let existing = query(origin)
        let updated = SecItemUpdate(existing as CFDictionary, attributes as CFDictionary)
        if updated == errSecSuccess { return true }
        guard updated == errSecItemNotFound else { return false }
        return SecItemAdd(existing.merging(attributes) { _, new in new } as CFDictionary, nil) == errSecSuccess
    }
    func clear() { SecItemDelete(query() as CFDictionary) }
}

struct SessionContext {
    let origin: String
    let token: String
    let revision: UInt64
}

@MainActor
final class AppModel: ObservableObject {
    @Published var base: String
    @Published var token: String
    @Published var account: Account?
    @Published var dark: Bool
    @Published var muted: Bool
    @Published var muteOnStartup: Bool
    /// 为真时优先播 HEVC 高清；这一集没有高清地址就用标准。
    @Published var preferHevc: Bool
    /// 打开后，离开应用回到桌面时才进入系统小窗。
    @Published var pipOnDesktop: Bool
    @Published var favs: [Int]
    @Published var likes: [Int]
    @Published var history: [Watch]
    @Published var searches: [String]
    @Published var toast: String?
    @Published var loginOpen = false
    /// 全屏播放时收起底栏，避免和小横条抢手势。
    @Published var immersive = false

    let defaults: UserDefaults
    private let tokens: SessionTokenStorage
    private var sessionOrigin = ""
    private var sessionRevision: UInt64 = 0
    lazy var api = HgApi(model: self)

    func sessionContext() -> SessionContext {
        SessionContext(origin: base, token: sessionOrigin == base && trustedLine ? token : "", revision: sessionRevision)
    }

    func isCurrent(_ context: SessionContext) -> Bool {
        context.origin == base && context.revision == sessionRevision
    }

    var loggedIn: Bool { !token.isEmpty }
    var trustedLine: Bool { base == "https://huangguoai.com" || base == "https://huangguoai.ai" }

    static let fixed = [
        ("固定线路 1", "https://huangguoai.com"),
        ("固定线路 2", "https://huangguoai.ai"),
    ]

    init(defaults: UserDefaults = .standard, tokens: SessionTokenStorage = KeychainTokens()) {
        self.defaults = defaults
        self.tokens = tokens
        let d = defaults
        let initialBase = d.string(forKey: "hg_base") ?? "https://huangguoai.com"
        base = initialBase
        token = tokens.read(origin: initialBase)
        dark = d.bool(forKey: "hg_dark")
        muteOnStartup = d.object(forKey: "hg_mute_startup") == nil ? true : d.bool(forKey: "hg_mute_startup")
        preferHevc = d.bool(forKey: "hg_hevc")
        pipOnDesktop = d.bool(forKey: "hg_pip")
        muted = d.object(forKey: "hg_muted") == nil ? true : d.bool(forKey: "hg_muted")
        favs = d.array(forKey: "hg_favs") as? [Int] ?? []
        likes = d.array(forKey: "hg_likes") as? [Int] ?? []
        searches = d.stringArray(forKey: "hg_searches") ?? []
        if let data = d.data(forKey: "hg_history"),
           let rows = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] {
            history = rows.map { Watch(id: $0.int("id"), ep: max($0.int("ep"), 1), t: $0.dbl("t"), dur: $0.dbl("dur"), at: $0.dbl("at")) }.filter { $0.id > 0 }
        } else {
            history = []
        }
        account = nil
        if let raw = d.string(forKey: "hg_user"),
           let data = raw.data(using: .utf8),
           let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            account = obj.account()
        }
        sessionOrigin = d.string(forKey: "hg_token_origin") ?? ""
        d.removeObject(forKey: "hg_token") // Remove the previous plaintext format.
        if sessionOrigin != base || !trustedLine || token.isEmpty {
            token = ""
            account = nil
            d.removeObject(forKey: "hg_token")
            d.removeObject(forKey: "hg_user")
            d.removeObject(forKey: "hg_token_origin")
            tokens.clear()
        }
        if muteOnStartup { muted = true }
    }

    func show(_ text: String) { toast = text }

    func setDark(_ on: Bool) { dark = on; defaults.set(on, forKey: "hg_dark") }
    func setMuted(_ on: Bool) { muted = on; defaults.set(on, forKey: "hg_muted") }
    func setMuteOnStartup(_ on: Bool) { muteOnStartup = on; defaults.set(on, forKey: "hg_mute_startup") }
    func setPreferHevc(_ on: Bool) { preferHevc = on; defaults.set(on, forKey: "hg_hevc") }
    func setPipOnDesktop(_ on: Bool) { pipOnDesktop = on; defaults.set(on, forKey: "hg_pip") }

    func setLine(_ url: String) {
        guard let next = normalizedOrigin(url.trimmingCharacters(in: .whitespacesAndNewlines)) else { return }
        guard next != base else { return }
        sessionRevision &+= 1
        sessionOrigin = ""
        tokens.clear()
        base = next
        token = ""
        account = nil
        defaults.set(next, forKey: "hg_base")
        defaults.removeObject(forKey: "hg_token")
        defaults.removeObject(forKey: "hg_user")
        defaults.removeObject(forKey: "hg_token_origin")
    }

    func setSession(token: String, user: Account, context: SessionContext) -> Bool {
        guard isCurrent(context), trustedLine else { return false }
        guard tokens.write(token, origin: context.origin) else { return false }
        sessionRevision &+= 1
        sessionOrigin = context.origin
        self.token = token
        account = user
        defaults.set(sessionOrigin, forKey: "hg_token_origin")
        let obj: [String: Any] = ["id": user.id, "username": user.username, "nickname": user.nickname, "like": user.like, "collect": user.collect]
        if let data = try? JSONSerialization.data(withJSONObject: obj), let text = String(data: data, encoding: .utf8) {
            defaults.set(text, forKey: "hg_user")
        }
        return true
    }

    func logout(context: SessionContext? = nil) {
        if let context, !isCurrent(context) { return }
        sessionRevision &+= 1
        sessionOrigin = ""
        tokens.clear()
        token = ""
        account = nil
        defaults.removeObject(forKey: "hg_token")
        defaults.removeObject(forKey: "hg_user")
        defaults.removeObject(forKey: "hg_token_origin")
    }

    func isFav(_ id: Int) -> Bool { favs.contains(id) }
    func isLiked(_ id: Int) -> Bool { likes.contains(id) }

    @discardableResult
    func toggleFav(_ id: Int) -> Bool {
        let on = !favs.contains(id)
        favs = on ? [id] + favs : favs.filter { $0 != id }
        defaults.set(favs, forKey: "hg_favs")
        return on
    }

    @discardableResult
    func toggleLike(_ id: Int) -> Bool {
        let on = !likes.contains(id)
        likes = on ? [id] + likes : likes.filter { $0 != id }
        defaults.set(likes, forKey: "hg_likes")
        return on
    }

    func rememberSearch(_ q: String) {
        let k = q.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !k.isEmpty else { return }
        searches = Array(([k] + searches.filter { $0 != k }).prefix(12))
        defaults.set(searches, forKey: "hg_searches")
    }

    func clearSearch() {
        searches = []
        defaults.removeObject(forKey: "hg_searches")
    }

    func saveWatch(id: Int, ep: Int, t: Double, dur: Double, keepFurther: Bool = false) {
        guard id > 0 else { return }
        let epN = max(ep, 1)
        if keepFurther, let prev = history.first(where: { $0.id == id }), prev.ep > epN { return }
        let prev = history.first { $0.id == id }
        let time = (prev?.ep == epN && t < 1 && (prev?.t ?? 0) > t) ? (prev?.t ?? t) : max(t, 0)
        let item = Watch(id: id, ep: epN, t: time, dur: max(dur, 0), at: Date().timeIntervalSince1970)
        history = Array(([item] + history.filter { $0.id != id }).prefix(40))
        let rows: [[String: Any]] = history.map { ["id": $0.id, "ep": $0.ep, "t": $0.t, "dur": $0.dur, "at": $0.at] }
        if let data = try? JSONSerialization.data(withJSONObject: rows) {
            defaults.set(data, forKey: "hg_history")
        }
    }

    func clearLocal() {
        favs = []
        likes = []
        history = []
        defaults.removeObject(forKey: "hg_favs")
        defaults.removeObject(forKey: "hg_likes")
        defaults.removeObject(forKey: "hg_history")
    }
}
