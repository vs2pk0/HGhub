package app.hghub.mobile.data

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

data class SessionContext(val origin: String, val token: String, val revision: Long)

class AppStore(ctx: Context) {
    private val prefs = ctx.getSharedPreferences("hghub_native_v1", Context.MODE_PRIVATE)
    private val vault = TokenVault(ctx)

    var base by mutableStateOf(prefs.getString("base", DEFAULT_BASE) ?: DEFAULT_BASE)
        private set
    var token by mutableStateOf(vault.read(base))
        private set
    var account by mutableStateOf(accountFrom(prefs.getString("user", null)))
        private set
    var dark by mutableStateOf(prefs.getBoolean("dark", false))
        private set
    var muted by mutableStateOf(prefs.getBoolean("muted", true))
        private set
    var muteOnStartup by mutableStateOf(prefs.getBoolean("muteOnStartup", true))
        private set
    var preferHevc by mutableStateOf(prefs.getBoolean("preferHevc", false))
        private set
    /** 打开后，按 Home 回到桌面时进入系统小窗，不在应用内缩小。 */
    var pipOnDesktop by mutableStateOf(prefs.getBoolean("pip", false))
        private set
    var canDesktopPip by mutableStateOf(false)
    var inPip by mutableStateOf(false)
    var pipWidth by mutableStateOf(9)
    var pipHeight by mutableStateOf(16)
    var immersive by mutableStateOf(false)
    var favs by mutableStateOf(intsFrom(prefs.getString("favs", null)))
        private set
    var likes by mutableStateOf(intsFrom(prefs.getString("likes", null)))
        private set
    var history by mutableStateOf(watchesFrom(prefs.getString("history", null)))
        private set
    var searches by mutableStateOf(stringsFrom(prefs.getString("searches", null)))
        private set
    var toast by mutableStateOf<String?>(null)
    private var sessionOrigin = prefs.getString("token_origin", "").orEmpty()
    private var sessionRevision = 0L

    // Called on the main dispatcher so origin, token and revision are one snapshot.
    fun sessionContext() = SessionContext(base, token.takeIf { sessionOrigin == base && trustedLine }.orEmpty(), sessionRevision)
    fun isCurrent(context: SessionContext) = context.origin == base && context.revision == sessionRevision

    val loggedIn: Boolean get() = token.isNotBlank()
    val trustedLine: Boolean
        get() = base == "https://huangguoai.com" || base == "https://huangguoai.ai"

    init {
        prefs.edit().remove("token").apply() // Remove the previous plaintext format.
        if (sessionOrigin != base || !trustedLine || token.isEmpty()) {
            token = ""
            account = null
            prefs.edit().remove("token").remove("user").remove("token_origin").apply()
            vault.clear()
        }
        if (muteOnStartup && !muted) {
            muted = true
            prefs.edit().putBoolean("muted", true).apply()
        }
    }

    fun show(msg: String) { toast = msg }

    fun applyDark(on: Boolean) {
        dark = on
        prefs.edit().putBoolean("dark", on).apply()
    }

    fun applyMuted(on: Boolean) {
        muted = on
        prefs.edit().putBoolean("muted", on).apply()
    }

    fun applyPreferHevc(on: Boolean) {
        preferHevc = on
        prefs.edit().putBoolean("preferHevc", on).apply()
    }

    fun applyPipOnDesktop(on: Boolean) {
        pipOnDesktop = on
        prefs.edit().putBoolean("pip", on).apply()
    }

    fun applyMuteOnStartup(on: Boolean) {
        muteOnStartup = on
        prefs.edit().putBoolean("muteOnStartup", on).apply()
    }

    fun setLine(url: String) {
        val next = runCatching { checkedOrigin(url.trim()) }.getOrNull() ?: return
        if (next == base) return
        sessionRevision++
        sessionOrigin = ""
        vault.clear()
        base = next
        token = ""
        account = null
        prefs.edit().putString("base", next).remove("token").remove("user").remove("token_origin").apply()
    }

    fun setSession(token: String, user: Account, context: SessionContext): Boolean {
        if (!isCurrent(context) || !trustedLine) return false
        vault.write(token, context.origin)
        sessionRevision++
        sessionOrigin = context.origin
        this.token = token
        account = user
        prefs.edit().putString("token_origin", sessionOrigin).putString("user", user.toJson().toString()).apply()
        return true
    }

    fun logout(context: SessionContext? = null) {
        if (context != null && !isCurrent(context)) return
        sessionRevision++
        sessionOrigin = ""
        vault.clear()
        token = ""
        account = null
        prefs.edit().remove("token").remove("user").remove("token_origin").apply()
    }

    fun isFav(id: Int) = id in favs
    fun isLiked(id: Int) = id in likes

    fun toggleFav(id: Int): Boolean {
        val on = id !in favs
        favs = if (on) listOf(id) + favs else favs.filter { it != id }
        prefs.edit().putString("favs", favs.intsJson()).apply()
        return on
    }

    fun toggleLike(id: Int): Boolean {
        val on = id !in likes
        likes = if (on) listOf(id) + likes else likes.filter { it != id }
        prefs.edit().putString("likes", likes.intsJson()).apply()
        return on
    }

    fun rememberSearch(q: String) {
        val k = q.trim()
        if (k.isEmpty()) return
        searches = (listOf(k) + searches.filter { it != k }).take(12)
        prefs.edit().putString("searches", searches.stringsJson()).apply()
    }

    fun clearSearch() {
        searches = emptyList()
        prefs.edit().remove("searches").apply()
    }

    fun saveWatch(id: Int, ep: Int, t: Double, dur: Double, keepFurther: Boolean = false) {
        if (id <= 0) return
        val epN = ep.coerceAtLeast(1)
        val prev = history.firstOrNull { it.id == id }
        // 首页自动从第 1 集播过，不要把已经看到的更后集数冲掉。
        if (keepFurther && prev != null && prev.ep > epN) return
        val time = if (prev != null && prev.ep == epN && t < 1.0 && prev.t > t) prev.t else t.coerceAtLeast(0.0)
        val item = Watch(id, epN, time, dur.coerceAtLeast(0.0), System.currentTimeMillis())
        history = listOf(item) + history.filter { it.id != id }.take(40)
        prefs.edit().putString("history", history.toJson()).apply()
    }

    fun clearLocal() {
        favs = emptyList()
        likes = emptyList()
        history = emptyList()
        prefs.edit().remove("favs").remove("likes").remove("history").apply()
    }

    companion object {
        const val DEFAULT_BASE = "https://huangguoai.com"
        val FIXED = listOf(
            "固定线路 1" to "https://huangguoai.com",
            "固定线路 2" to "https://huangguoai.ai",
        )
    }
}
