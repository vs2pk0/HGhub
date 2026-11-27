package app.hghub.mobile.data

import android.util.Base64
import okhttp3.OkHttpClient
import okhttp3.Request
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.URI
import java.util.concurrent.TimeUnit

class ApiException(message: String) : Exception(message)

class HgApi(private val store: AppStore) {
    private val http = OkHttpClient.Builder()
        .connectTimeout(12, TimeUnit.SECONDS)
        .readTimeout(20, TimeUnit.SECONDS)
        .followRedirects(true)
        .followSslRedirects(true)
        .build()

    private fun guard(raw: String) {
        val uri = runCatching { URI(raw) }.getOrElse { throw ApiException("地址无效") }
        if (!uri.scheme.equals("https", true)) throw ApiException("只允许 HTTPS")
        val host = uri.host?.lowercase().orEmpty()
        if (host.isBlank() || host == "localhost" || host.endsWith(".local") || isPrivate(host)) {
            throw ApiException("目标地址不可用")
        }
    }

    suspend fun feed(page: Int, size: Int = 10): Pair<List<VideoCard>, PageInfo> =
        get("/api/videos?page=$page&size=$size").let { it.items().map { o -> o.toCard() } to it.toPage() }

    suspend fun category(cate: String, page: Int, size: Int = 20): Pair<List<VideoCard>, PageInfo> =
        get("/api/videos/category/${enc(cate)}?page=$page&size=$size").let { it.items().map { o -> o.toCard() } to it.toPage() }

    suspend fun search(q: String, page: Int, size: Int = 20): Pair<List<VideoCard>, PageInfo> =
        get("/api/search?q=${enc(q)}&page=$page&size=$size").let { it.items().map { o -> o.toCard() } to it.toPage() }

    suspend fun rank(kind: String): List<RankItem> {
        val k = if (kind == "recommend" || kind == "potential") kind else "hot"
        return get("/api/ranks/$k").items().map { it.toRank() }
    }

    suspend fun posts(page: Int, size: Int = 20): Pair<List<Post>, PageInfo> =
        get("/api/posts?page=$page&size=$size").let { it.items().map { o -> o.toPost() } to it.toPage() }

    suspend fun post(id: Int): Post = get("/api/posts/$id").toPost()

    suspend fun video(id: Int): Detail = get("/api/videos/$id").toDetail()

    suspend fun play(id: Int, ep: Int, origin: String? = null): PlayInfo =
        get("/api/videos/$id/play?ep=$ep&page_texts=0", origin).toPlay()

    /** 当前站点的播放地址从这里取，旧的 play 接口会返回空的 video_url。 */
    suspend fun playback(id: Int, ep: Int): PlayInfo = withContext(Dispatchers.IO) {
        call("POST", "/api/videos/$id/episodes/$ep/playback", JSONObject(), auth = false).toPlayback()
    }

    suspend fun comments(id: Int, ep: Int, page: Int): Pair<List<Comment>, PageInfo> =
        get("/api/videos/$id/comments?ep=$ep&page=$page&size=20").let { it.items().map { o -> o.toComment() } to it.toPage() }

    suspend fun sendComment(id: Int, ep: Int, text: String) = withContext(Dispatchers.IO) {
        val body = JSONObject().put("text", text)
        call("POST", "/api/videos/$id/comments?ep=$ep", body, auth = true)
        Unit
    }

    suspend fun like(id: Int, ep: Int, on: Boolean) = withContext(Dispatchers.IO) {
        call(if (on) "POST" else "DELETE", "/api/videos/$id/like?ep=$ep", null, auth = true)
        Unit
    }

    suspend fun collect(id: Int, on: Boolean) = withContext(Dispatchers.IO) {
        call(if (on) "POST" else "DELETE", "/api/videos/$id/collect", null, auth = true)
        Unit
    }

    suspend fun login(username: String, password: String): Account = withContext(Dispatchers.IO) {
        ensureTrusted()
        val data = call(
            "POST",
            "/api/account/loginByPassword",
            JSONObject().put("username", username).put("password", password),
            auth = false,
        )
        saveSession(data)
    }

    suspend fun register(username: String, password: String, email: String): Account = withContext(Dispatchers.IO) {
        ensureTrusted()
        val body = JSONObject()
            .put("username", username)
            .put("password", password)
            .put("confirm_password", password)
        if (email.isNotBlank()) body.put("email", email)
        val data = call("POST", "/api/account/registerByPassword", body, auth = false)
        saveSession(data)
    }

    suspend fun logout() {
        withContext(Dispatchers.IO) {
            runCatching { call("POST", "/api/account/logout", null, auth = true) }
        }
        store.logout()
    }

    /** 地址发布页上的镜像线路。失败时由界面提示，不自动切换。 */
    suspend fun mirrors(): List<String> = withContext(Dispatchers.IO) {
        val page = "https://huangguoai.pages.dev/"
        guard(page)
        val html = text(page) ?: throw ApiException("地址发布页不可用")
        val src = scriptSrc(html) ?: "publish.js"
        val jsUrl = if (src.startsWith("http")) src else page.trimEnd('/') + "/" + src.trimStart('/')
        guard(jsUrl)
        val js = text(jsUrl) ?: throw ApiException("镜像清单不可用")
        val domains = jsArray(js, "var urls")
        val subs = jsArray(js, "var subdomains")
        if (domains.isEmpty()) throw ApiException("没有找到镜像域名")
        val out = LinkedHashSet<String>()
        for (domain in domains) {
            val host = domain.trim().trimEnd('/')
            if (host.isEmpty()) continue
            val names = if (subs.isEmpty()) listOf(host) else subs.map { "$it.$host" }
            for (name in names) {
                val url = "https://$name"
                if (runCatching { guard(url) }.isSuccess) out.add(url)
                if (out.size >= 40) return@withContext out.toList()
            }
        }
        if (out.isEmpty()) throw ApiException("没有可用镜像")
        out.toList()
    }

    fun bytes(url: String): ByteArray? {
        guard(url)
        val req = Request.Builder()
            .url(url)
            .header("User-Agent", UA)
            .header("Referer", store.base.trimEnd('/') + "/")
            .header("Accept", "*/*")
            .build()
        http.newCall(req).execute().use { resp ->
            if (!resp.isSuccessful) return null
            return resp.body?.bytes()
        }
    }

    private fun saveSession(data: JSONObject): Account {
        val token = data.str("token")
        if (token.isBlank()) throw ApiException("登录返回缺少 token")
        val user = data.obj("user")?.toAccount() ?: throw ApiException("登录返回缺少用户")
        store.setSession(token, user)
        return user
    }

    private fun ensureTrusted() {
        if (!store.trustedLine) throw ApiException("镜像线路仅供游客浏览，登录请切换到固定线路")
    }

    private suspend fun get(path: String, origin: String? = null): JSONObject =
        withContext(Dispatchers.IO) { call("GET", path, null, false, origin) }

    private fun call(method: String, path: String, body: JSONObject?, auth: Boolean, originOverride: String? = null): JSONObject {
        val origin = (originOverride ?: store.base).trim().trimEnd('/')
        guard(origin)
        if (auth && store.token.isBlank()) throw ApiException("请先登录")
        val encoded = if (body == null) "" else Base64.encodeToString(body.toString().toByteArray(), Base64.NO_WRAP)
        val value = EdgeChannel.exchange(
            http,
            origin,
            UA,
            method,
            path,
            encoded,
            if (body == null) "" else "application/json",
            if (auth) store.token else "",
        )
        if (value.length() == 0) return JSONObject()
        if (value.optInt("status", 0) == 1) {
            return value.optJSONObject("data") ?: JSONObject()
        }
        val msg = value.optString("msg").ifBlank { "接口返回失败" }
        throw ApiException(msg)
    }

    private fun text(url: String): String? {
        val req = Request.Builder().url(url).header("User-Agent", UA).header("Accept", "text/html,*/*").build()
        http.newCall(req).execute().use { resp ->
            if (!resp.isSuccessful) return null
            return resp.body?.string()
        }
    }

    private fun enc(s: String): String = java.net.URLEncoder.encode(s, "UTF-8")

    companion object {
        const val UA =
            "Mozilla/5.0 (Linux; Android 14; Mobile) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36"
    }
}

private fun isPrivate(host: String): Boolean {
    val parts = host.split('.')
    if (parts.size != 4 || parts.any { it.toIntOrNull() == null }) return false
    val n = parts.map { it.toInt() }
    if (n.any { it !in 0..255 }) return false
    return n[0] == 10 || n[0] == 127 || (n[0] == 192 && n[1] == 168) ||
        (n[0] == 172 && n[1] in 16..31) || (n[0] == 169 && n[1] == 254) || n[0] == 0
}

private fun scriptSrc(html: String): String? {
    var rest = html
    while (true) {
        val i = rest.indexOf("src=\"")
        if (i < 0) return null
        val after = rest.substring(i + 5)
        val end = after.indexOf('"')
        if (end < 0) return null
        val candidate = after.substring(0, end)
        if (candidate.contains(".js")) return candidate
        rest = after.substring(end)
    }
}

private fun jsArray(src: String, decl: String): List<String> {
    val at = src.indexOf(decl)
    if (at < 0) return emptyList()
    val rest = src.substring(at)
    val open = rest.indexOf('[')
    val close = rest.indexOf(']', open + 1)
    if (open < 0 || close < 0) return emptyList()
    return rest.substring(open + 1, close)
        .split(',')
        .map { it.trim().trim('\'', '"').trim() }
        .filter { it.isNotEmpty() && !it.startsWith("//") }
}
