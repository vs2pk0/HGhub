package app.hghub.mobile.data

import android.util.Base64
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.math.BigInteger
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.SecureRandom
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPublicKeySpec
import javax.crypto.Cipher
import javax.crypto.KeyAgreement
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * 站点已停用 /api/edge/request。公开读走明文 JSON，其余走 dx150-edge-v1。
 */
internal object EdgeChannel {
    private const val protocol = "dx150-edge-v1"
    private const val maxSequence = 16384
    private val jsonType = "application/json; charset=utf-8".toMediaType()
    private val lock = Any()
    private val sessions = HashMap<String, Session>()

    fun exchange(
        http: OkHttpClient,
        origin: String,
        ua: String,
        method: String,
        path: String,
        body: String,
        contentType: String,
        token: String,
    ): JSONObject {
        val scope = requestScope(method, path)
        if (scope == "public") {
            val plain = publicJson(http, origin, ua, method, path, body, contentType)
            if (plain != null) return plain
        }
        val packetPath = if (scope == "public") canonical(path) else path
        val packet = JSONObject().put(
            "request",
            JSONObject()
                .put("method", method)
                .put("path", packetPath)
                .put("body", body)
                .put("content_type", contentType),
        )
        if (scope != "public" && token.isNotBlank()) packet.put("authorization", "Bearer $token")
        return encrypted(http, origin, ua, scope, packet)
    }

    private fun publicJson(
        http: OkHttpClient,
        origin: String,
        ua: String,
        method: String,
        path: String,
        body: String,
        contentType: String,
    ): JSONObject? {
        val packet = JSONObject()
            .put("method", method)
            .put("path", canonical(path))
            .put("body", body)
            .put("content_type", contentType)
        val posted = post(
            http, "$origin/api/public/request", packet, ua, "$origin/",
            extra = listOf("X-HG-Public-JSON" to "1"),
        )
        val marked = posted.header("x-hg-public-json") == "1"
        if (posted.code == 400 && !marked) {
            val error = runCatching { JSONObject(posted.text).optString("error") }.getOrDefault("")
            if (error == "invalid_request" || error == "unsupported_version") return null
        }
        if (posted.code !in 200..299) throw ApiException(httpError(posted.code, posted.text))
        return parseBusiness(posted.text)
    }

    private fun encrypted(http: OkHttpClient, origin: String, ua: String, scope: String, packet: JSONObject): JSONObject {
        repeat(2) {
            val material = material(http, origin, ua, scope)
            val iv = nonce(material.sequence)
            val aadReq = aad(material.info, "req", material.sequence, material.timestamp)
            val cipherText = seal(material.requestKey, iv, aadReq, packet.toString().toByteArray())
            val body = JSONObject()
                .put("v", 1)
                .put("type", "call")
                .put("kid", material.kid)
                .put("sid", material.sid)
                .put("seq", material.sequence)
                .put("ts", material.timestamp)
                .put("ciphertext", b64url(cipherText))
            val posted = post(http, "$origin/api/$scope/request", body, ua, "$origin/")
            if (posted.code !in 200..299) {
                val error = runCatching { JSONObject(posted.text).optString("error") }.getOrDefault("")
                if (error == "session_expired") {
                    invalidate(origin, scope, material.sid)
                    return@repeat
                }
                throw ApiException("加密接口请求失败（${posted.code}）")
            }
            val result = JSONObject(posted.text)
            if (result.optInt("v") != 1 || result.optString("type") != "result" ||
                result.optString("sid") != material.sid || result.optString("kid") != material.kid ||
                result.optLong("seq") != material.sequence.toLong() || result.optLong("ts") != material.timestamp
            ) {
                throw ApiException("加密接口响应不匹配")
            }
            val plain = open(
                material.responseKey,
                iv,
                aad(material.info, "res", material.sequence, material.timestamp),
                b64urlDec(result.optString("ciphertext")),
            )
            return businessFromInner(JSONObject(String(plain)))
        }
        throw ApiException("加密接口请求失败")
    }

    private fun material(http: OkHttpClient, origin: String, ua: String, scope: String): Material {
        synchronized(lock) {
            val key = "$origin\n$scope"
            var session = sessions[key]
            if (session == null || session.stale()) {
                session = handshake(http, origin, ua, scope)
                sessions[key] = session
            }
            session.sequence += 1
            if (session.sequence > maxSequence) throw ApiException("加密会话已到期")
            return session.material()
        }
    }

    private fun invalidate(origin: String, scope: String, sid: String) {
        synchronized(lock) {
            val key = "$origin\n$scope"
            if (sessions[key]?.sid == sid) sessions.remove(key)
        }
    }

    private fun handshake(http: OkHttpClient, origin: String, ua: String, scope: String): Session {
        val generator = KeyPairGenerator.getInstance("EC")
        generator.initialize(ECGenParameterSpec("secp256r1"), SecureRandom())
        val pair = generator.generateKeyPair()
        val pub = pair.public as java.security.interfaces.ECPublicKey
        val rawPub = byteArrayOf(4) + fixed32(pub.w.affineX) + fixed32(pub.w.affineY)
        val helloBody = JSONObject().put("v", 1).put("type", "hello").put("client_pub", b64url(rawPub))
        val posted = post(http, "$origin/api/$scope/request", helloBody, ua, "$origin/")
        if (posted.code !in 200..299) throw ApiException("加密握手失败")
        val hello = JSONObject(posted.text)
        val kid = hello.optString("kid")
        val sid = hello.optString("sid")
        val site = hello.optString("site")
        val serverTime = hello.optLong("server_time")
        val expiresAt = hello.optLong("expires_at")
        if (hello.optInt("v") != 1 || hello.optString("type") != "hello" ||
            !tokenOk(kid, 1, 64) || !tokenOk(sid, 16, 128) || !siteOk(site) ||
            expiresAt <= serverTime || expiresAt - serverTime > 600
        ) {
            throw ApiException("加密握手响应无效")
        }
        val serverPub = b64urlDec(hello.optString("server_pub"))
        val salt = b64urlDec(hello.optString("salt"))
        if (salt.size != 32 || serverPub.size != 65 || serverPub[0] != 4.toByte()) {
            throw ApiException("加密握手响应无效")
        }
        val serverKey = KeyFactory.getInstance("EC").generatePublic(
            ECPublicKeySpec(
                ECPoint(BigInteger(1, serverPub.copyOfRange(1, 33)), BigInteger(1, serverPub.copyOfRange(33, 65))),
                pub.params,
            ),
        )
        val agreement = KeyAgreement.getInstance("ECDH")
        agreement.init(pair.private)
        agreement.doPhase(serverKey, true)
        val shared = pad32(agreement.generateSecret())
        val info = listOf(protocol, scope, site, kid, sid).joinToString("\n")
        val local = now()
        return Session(
            info = info,
            kid = kid,
            sid = sid,
            clockOffset = serverTime - local,
            expiresAt = local + expiresAt - serverTime,
            requestKey = hkdf(shared, salt, "$info\nreq".toByteArray()),
            responseKey = hkdf(shared, salt, "$info\nres".toByteArray()),
        )
    }

    private fun businessFromInner(inner: JSONObject): JSONObject {
        val status = inner.optInt("status", -1)
        if (status !in 200..599 || inner.optJSONObject("headers") == null) throw ApiException("加密接口响应格式无效")
        val text = if (status == 204 || status == 205 || status == 304) "" else {
            val body = inner.optString("body")
            if (body.isBlank()) "" else String(Base64.decode(body, Base64.DEFAULT))
        }
        if (status !in 200..299) throw ApiException(httpError(status, text))
        return parseBusiness(text)
    }

    private fun post(
        http: OkHttpClient,
        url: String,
        body: JSONObject,
        ua: String,
        referer: String,
        extra: List<Pair<String, String>> = emptyList(),
    ): Posted {
        val builder = Request.Builder()
            .url(url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("User-Agent", ua)
            .header("Referer", referer)
            .post(body.toString().toRequestBody(jsonType))
        extra.forEach { (name, value) -> builder.header(name, value) }
        http.newCall(builder.build()).execute().use { resp ->
            if (resp.isRedirect) throw ApiException("业务接口重定向已拒绝")
            return Posted(resp.code, resp.header("x-hg-public-json"), resp.body?.string().orEmpty())
        }
    }

    private fun parseBusiness(text: String): JSONObject {
        if (text.isBlank()) return JSONObject()
        return runCatching { JSONObject(text) }.getOrElse { throw ApiException("接口返回无法解析") }
    }

    private fun httpError(status: Int, text: String): String {
        if (status == 401 || status == 403) return "请先登录"
        val msg = runCatching { JSONObject(text).optString("msg") }.getOrDefault("")
        if (msg.isNotBlank()) return msg
        return "服务器请求失败（HTTP $status），请稍后重试或切换线路"
    }

    private fun canonical(path: String): String {
        val q = path.indexOf('?')
        if (q < 0) return path
        val pairs = path.substring(q + 1).split('&').filter { it.isNotEmpty() }.map {
            val eq = it.indexOf('=')
            if (eq < 0) it to "" else it.substring(0, eq) to it.substring(eq + 1)
        }.sortedWith(compareBy({ it.first }, { it.second }))
        if (pairs.isEmpty()) return path.substring(0, q)
        return path.substring(0, q) + "?" + pairs.joinToString("&") { "${it.first}=${it.second}" }
    }

    private fun now() = System.currentTimeMillis() / 1000
    private fun nonce(sequence: Int): ByteArray {
        val iv = ByteArray(12)
        iv[8] = (sequence ushr 24).toByte()
        iv[9] = (sequence ushr 16).toByte()
        iv[10] = (sequence ushr 8).toByte()
        iv[11] = sequence.toByte()
        return iv
    }

    private fun aad(info: String, direction: String, sequence: Int, timestamp: Long) =
        "$info\n$direction\n$sequence\n$timestamp".toByteArray()

    private fun hkdf(ikm: ByteArray, salt: ByteArray, info: ByteArray): ByteArray {
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(SecretKeySpec(salt, "HmacSHA256"))
        val prk = mac.doFinal(ikm)
        mac.init(SecretKeySpec(prk, "HmacSHA256"))
        mac.update(info)
        mac.update(1)
        return mac.doFinal()
    }

    private fun seal(key: ByteArray, iv: ByteArray, aad: ByteArray, plain: ByteArray): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(128, iv))
        cipher.updateAAD(aad)
        return cipher.doFinal(plain)
    }

    private fun open(key: ByteArray, iv: ByteArray, aad: ByteArray, cipherText: ByteArray): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(128, iv))
        cipher.updateAAD(aad)
        return cipher.doFinal(cipherText)
    }

    private fun fixed32(value: BigInteger): ByteArray {
        val raw = value.toByteArray()
        val stripped = if (raw.size > 32 && raw[0] == 0.toByte()) raw.copyOfRange(raw.size - 32, raw.size) else raw
        val out = ByteArray(32)
        System.arraycopy(stripped, 0, out, 32 - stripped.size, stripped.size.coerceAtMost(32))
        return out
    }

    private fun pad32(raw: ByteArray): ByteArray {
        if (raw.size == 32) return raw
        if (raw.size > 32) return raw.copyOfRange(raw.size - 32, raw.size)
        return ByteArray(32 - raw.size) + raw
    }

    private fun b64url(bytes: ByteArray) =
        Base64.encodeToString(bytes, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)

    private fun b64urlDec(value: String): ByteArray {
        if (!value.matches(Regex("^[A-Za-z0-9_-]+$"))) throw ApiException("加密响应编码无效")
        val pad = (4 - value.length % 4) % 4
        return Base64.decode(value + "=".repeat(pad), Base64.URL_SAFE or Base64.NO_WRAP)
    }

    private fun tokenOk(value: String, min: Int, max: Int) =
        value.length in min..max && value.all { it.isLetterOrDigit() || it == '_' || it == '-' }

    private fun siteOk(value: String) =
        value.length in 1..256 && value.all { it.isLetterOrDigit() || it == '.' || it == ':' || it == '[' || it == ']' || it == '-' }

    private fun requestScope(method: String, path: String): String {
        var verb = method.uppercase()
        if (verb == "HEAD") verb = "GET"
        val pathname = path.substringBefore('?')
        val segments = pathname.split('/')
        for (row in routeRows) {
            if (row.method != verb) continue
            val parts = row.pattern.split('/')
            if (parts.size != segments.size) continue
            val matched = parts.zip(segments).all { (pattern, segment) ->
                if (pattern.startsWith(":")) {
                    val name = pattern.removePrefix(":")
                    if (name == "id" || name == "cid" || name == "ep") segment.isNotEmpty() && segment.all { it.isDigit() }
                    else segment.isNotEmpty()
                } else {
                    pattern == segment
                }
            }
            if (!matched) continue
            if (row.scope == "public" && path.substringAfter('?', "").split('&').any {
                    val value = it.substringAfter('=', "")
                    it.substringBefore('=') == "sort" && (value.equals("random", true) || value.equals("rand", true))
                }
            ) {
                return "dynamic"
            }
            return row.scope
        }
        throw ApiException("接口未配置加密通道")
    }

    private data class Route(val method: String, val scope: String, val pattern: String, val dynamic: Int)

    private val routeRows: List<Route> by lazy {
        ROUTE_TABLE.map { Route(it[0], it[1], it[2], it[2].split('/').count { part -> part.startsWith(":") }) }
            .sortedWith(compareBy({ it.method }, { it.dynamic }))
    }

    private val ROUTE_TABLE = arrayOf(
        arrayOf("POST", "dynamic", "/api/edge/image"),
        arrayOf("GET", "dynamic", "/api/home"),
        arrayOf("POST", "dynamic", "/api/media/covers"),
        arrayOf("GET", "dynamic", "/api/posts/:id"),
        arrayOf("GET", "dynamic", "/api/posts/:id/comments"),
        arrayOf("POST", "dynamic", "/api/posts/:id/share"),
        arrayOf("GET", "dynamic", "/api/search"),
        arrayOf("GET", "dynamic", "/api/site/address-poster"),
        arrayOf("POST", "dynamic", "/api/site/click"),
        arrayOf("GET", "dynamic", "/api/site/entries"),
        arrayOf("GET", "dynamic", "/api/site/go-home"),
        arrayOf("POST", "dynamic", "/api/site/home-slots"),
        arrayOf("POST", "dynamic", "/api/stats/click"),
        arrayOf("POST", "dynamic", "/api/stats/origin"),
        arrayOf("GET", "dynamic", "/api/users/:id"),
        arrayOf("GET", "dynamic", "/api/videos/:id"),
        arrayOf("GET", "dynamic", "/api/videos/:id/comments"),
        arrayOf("GET", "dynamic", "/api/videos/:id/comments/:cid/replies"),
        arrayOf("GET", "dynamic", "/api/videos/:id/next"),
        arrayOf("POST", "dynamic", "/api/videos/:id/play"),
        arrayOf("POST", "dynamic", "/api/videos/:id/episode-stats"),
        arrayOf("POST", "dynamic", "/api/videos/:id/visit"),
        arrayOf("POST", "dynamic", "/api/videos/recommend"),
        arrayOf("POST", "dynamic", "/api/videos/recommend/ref/:ref"),
        arrayOf("GET", "dynamic", "/api/whoami"),
        arrayOf("POST", "playback", "/api/media/playback/previews"),
        arrayOf("POST", "playback", "/api/media/prerolls/:id/playback"),
        arrayOf("POST", "playback", "/api/posts/:id/playback"),
        arrayOf("POST", "playback", "/api/videos/:id/episodes/:ep/playback"),
        arrayOf("GET", "playback", "/api/videos/:id/play"),
        arrayOf("POST", "playback", "/api/videos/:id/playback"),
        arrayOf("GET", "public", "/api/feed/post/today"),
        arrayOf("GET", "public", "/api/feed/recommend"),
        arrayOf("GET", "public", "/api/feed/today"),
        arrayOf("GET", "public", "/api/home/latest"),
        arrayOf("GET", "public", "/api/home/recommends"),
        arrayOf("GET", "public", "/api/posts"),
        arrayOf("GET", "public", "/api/posts/:id/adjacent"),
        arrayOf("GET", "public", "/api/posts/:id/related"),
        arrayOf("GET", "public", "/api/posts/category/:route"),
        arrayOf("GET", "public", "/api/posts/tag/:tag"),
        arrayOf("GET", "public", "/api/posts/user/:id"),
        arrayOf("GET", "public", "/api/ranks/:type"),
        arrayOf("GET", "public", "/api/search/hot"),
        arrayOf("GET", "public", "/api/search/suggest"),
        arrayOf("GET", "public", "/api/search/tags/hot"),
        arrayOf("GET", "public", "/api/site/categories/post"),
        arrayOf("GET", "public", "/api/site/categories/video"),
        arrayOf("GET", "public", "/api/site/config"),
        arrayOf("GET", "public", "/api/site/links"),
        arrayOf("GET", "public", "/api/site/menus"),
        arrayOf("GET", "public", "/api/site/meta"),
        arrayOf("GET", "public", "/api/site/pages"),
        arrayOf("GET", "public", "/api/site/pages/:slug"),
        arrayOf("GET", "public", "/api/site/sorts/video"),
        arrayOf("GET", "public", "/api/site/transit"),
        arrayOf("GET", "public", "/api/site/watermark"),
        arrayOf("GET", "public", "/api/tags"),
        arrayOf("GET", "public", "/api/topics"),
        arrayOf("GET", "public", "/api/topics/:slug"),
        arrayOf("GET", "public", "/api/users/:id/stats"),
        arrayOf("GET", "public", "/api/videos"),
        arrayOf("GET", "public", "/api/videos/:id/related"),
        arrayOf("GET", "public", "/api/videos/category/:route"),
        arrayOf("GET", "public", "/api/videos/long"),
        arrayOf("GET", "public", "/api/videos/report-reasons"),
        arrayOf("GET", "public", "/api/videos/sort/:route"),
        arrayOf("GET", "public", "/api/videos/tag/:tag"),
        arrayOf("GET", "public", "/api/videos/user/:id"),
        arrayOf("POST", "user", "/api/account/loginByPassword"),
        arrayOf("POST", "user", "/api/account/logout"),
        arrayOf("GET", "user", "/api/account/me"),
        arrayOf("POST", "user", "/api/account/password"),
        arrayOf("PATCH", "user", "/api/account/profile"),
        arrayOf("POST", "user", "/api/account/registerByPassword"),
        arrayOf("POST", "user", "/api/ai-wish"),
        arrayOf("DELETE", "user", "/api/posts/:id/collect"),
        arrayOf("POST", "user", "/api/posts/:id/collect"),
        arrayOf("POST", "user", "/api/posts/:id/comments"),
        arrayOf("POST", "user", "/api/posts/:id/comments/:cid/guest-like"),
        arrayOf("DELETE", "user", "/api/posts/:id/comments/:cid/like"),
        arrayOf("POST", "user", "/api/posts/:id/comments/:cid/like"),
        arrayOf("GET", "user", "/api/posts/:id/comments/liked"),
        arrayOf("POST", "user", "/api/posts/:id/guest-comments"),
        arrayOf("POST", "user", "/api/posts/:id/guest-like"),
        arrayOf("GET", "user", "/api/posts/:id/guest-liked"),
        arrayOf("DELETE", "user", "/api/posts/:id/like"),
        arrayOf("POST", "user", "/api/posts/:id/like"),
        arrayOf("POST", "user", "/api/posts/:id/visit"),
        arrayOf("DELETE", "user", "/api/users/:id/follow"),
        arrayOf("POST", "user", "/api/users/:id/follow"),
        arrayOf("GET", "user", "/api/users/me/collects"),
        arrayOf("POST", "user", "/api/users/me/collects/delete"),
        arrayOf("GET", "user", "/api/users/me/comments"),
        arrayOf("GET", "user", "/api/users/me/follows"),
        arrayOf("GET", "user", "/api/users/me/history"),
        arrayOf("POST", "user", "/api/users/me/history"),
        arrayOf("POST", "user", "/api/users/me/history/delete"),
        arrayOf("POST", "user", "/api/users/me/history/sync"),
        arrayOf("GET", "user", "/api/users/me/likes"),
        arrayOf("POST", "user", "/api/users/me/likes/delete"),
        arrayOf("POST", "user", "/api/users/me/likes/sync"),
        arrayOf("GET", "user", "/api/users/me/messages"),
        arrayOf("DELETE", "user", "/api/users/me/messages/:id"),
        arrayOf("POST", "user", "/api/users/me/messages/:id/read"),
        arrayOf("POST", "user", "/api/users/me/messages/batch-delete"),
        arrayOf("POST", "user", "/api/users/me/messages/read-all"),
        arrayOf("GET", "user", "/api/users/me/messages/unread-count"),
        arrayOf("DELETE", "user", "/api/videos/:id/collect"),
        arrayOf("POST", "user", "/api/videos/:id/collect"),
        arrayOf("POST", "user", "/api/videos/:id/comments"),
        arrayOf("POST", "user", "/api/videos/:id/comments/:cid/guest-like"),
        arrayOf("DELETE", "user", "/api/videos/:id/comments/:cid/like"),
        arrayOf("POST", "user", "/api/videos/:id/comments/:cid/like"),
        arrayOf("GET", "user", "/api/videos/:id/comments/liked"),
        arrayOf("GET", "user", "/api/videos/:id/comments/state"),
        arrayOf("POST", "user", "/api/videos/:id/feedback"),
        arrayOf("POST", "user", "/api/videos/:id/guest-comments"),
        arrayOf("POST", "user", "/api/videos/:id/guest-like"),
        arrayOf("GET", "user", "/api/videos/:id/guest-liked"),
        arrayOf("GET", "user", "/api/videos/:id/interaction-state"),
        arrayOf("DELETE", "user", "/api/videos/:id/like"),
        arrayOf("POST", "user", "/api/videos/:id/like"),
        arrayOf("GET", "user", "/api/videos/:id/rating"),
        arrayOf("POST", "user", "/api/videos/:id/rating"),
        arrayOf("POST", "user", "/api/videos/:id/report"),
        arrayOf("GET", "user", "/api/videos/following"),
    )

    private class Session(
        val info: String,
        val kid: String,
        val sid: String,
        val clockOffset: Long,
        val expiresAt: Long,
        var sequence: Int = 0,
        val requestKey: ByteArray,
        val responseKey: ByteArray,
    ) {
        fun stale() = expiresAt <= now() + 5 || sequence >= maxSequence
        fun material() = Material(info, kid, sid, sequence, now() + clockOffset, requestKey, responseKey)
    }

    private class Material(
        val info: String,
        val kid: String,
        val sid: String,
        val sequence: Int,
        val timestamp: Long,
        val requestKey: ByteArray,
        val responseKey: ByteArray,
    )

    private class Posted(val code: Int, val marker: String?, val text: String) {
        fun header(name: String) = if (name.equals("x-hg-public-json", true)) marker else null
    }
}
