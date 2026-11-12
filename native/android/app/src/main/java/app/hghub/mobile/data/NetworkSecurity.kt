package app.hghub.mobile.data

import okhttp3.Dns
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Response
import java.io.IOException
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress
import java.net.UnknownHostException
import java.util.concurrent.TimeUnit

internal fun trustedOrigin(origin: String) = origin == "https://huangguoai.com" || origin == "https://huangguoai.ai"

internal fun checkedUrl(raw: String): HttpUrl {
    val url = raw.toHttpUrlOrNull() ?: throw IOException("地址无效")
    val host = url.host.trimEnd('.').lowercase()
    if (url.scheme != "https" || url.username.isNotEmpty() || url.password.isNotEmpty() ||
        raw.any { it.isISOControl() } || listOf("localhost", "local", "internal", "home.arpa").any {
            host == it || host.endsWith(".$it")
        }) throw IOException("目标地址不可用")
    // HttpUrl canonicalizes alternate IPv4 spellings before this check.
    if (host.contains(':') || host.all { it.isDigit() || it == '.' }) {
        if (!publicAddress(InetAddress.getByName(host))) throw IOException("禁止访问内网地址")
    }
    return url
}

internal fun checkedOrigin(raw: String): String {
    val url = checkedUrl(raw)
    if (url.encodedPath != "/" || url.query != null || url.fragment != null) throw IOException("线路必须是 HTTPS origin")
    return url.toString().removeSuffix("/")
}

internal fun publicAddress(address: InetAddress): Boolean {
    val bytes = address.address.map { it.toInt() and 255 }
    if (address is Inet4Address) {
        val (a, b, c) = bytes
        if (a == 198 && b in 18..19) return true // User's FakeIP proxy transport.
        return !(a == 0 || a == 10 || a == 127 || a >= 224 ||
            (a == 100 && b in 64..127) || (a == 169 && b == 254) ||
            (a == 172 && b in 16..31) || (a == 192 && b == 168) ||
            (a == 192 && b == 0 && c in listOf(0, 2)) || (a == 192 && b == 88 && c == 99) ||
            (a == 198 && b == 51 && c == 100) || (a == 203 && b == 0 && c == 113))
    }
    if (address !is Inet6Address) return false
    if (bytes.take(10).all { it == 0 } && bytes[10] == 255 && bytes[11] == 255) {
        return publicAddress(InetAddress.getByAddress(address.address.copyOfRange(12, 16)))
    }
    val first = bytes[0] * 256 + bytes[1]
    val second = bytes[2] * 256 + bytes[3]
    return first and 0xe000 == 0x2000 && first != 0x2002 &&
        !(first == 0x2001 && (second < 0x0200 || second == 0x0db8)) &&
        !(first == 0x3fff && second < 0x1000)
}

internal class PublicDns(private val resolver: Dns = Dns.SYSTEM) : Dns {
    override fun lookup(hostname: String): List<InetAddress> {
        val addresses = resolver.lookup(hostname)
        if (addresses.isEmpty() || addresses.any { !publicAddress(it) }) throw UnknownHostException("目标 DNS 包含内网或保留地址")
        // OkHttp connects using this exact result; never resolve a second time after checking.
        return addresses
    }
}

internal class GuestEnvelopeRead

internal class CheckedRedirects : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        var request = chain.request()
        for (hop in 0..5) {
            checkedUrl(request.url.toString())
            val response = chain.proceed(request)
            if (response.code !in listOf(301, 302, 303, 307, 308)) return response
            val guest = request.tag(GuestEnvelopeRead::class.java) != null
            val canFollow = request.header("Authorization") == null &&
                (request.method == "GET" || (guest && response.code in listOf(301, 302, 303)))
            if (!canFollow) return response // Never replay passwords, tokens or mutations.
            val location = response.header("Location")
            response.close()
            if (hop == 5 || location == null) throw IOException("重定向次数超限或缺少地址")
            val next = request.url.resolve(location) ?: throw IOException("重定向地址无效")
            checkedUrl(next.toString())
            // The logical GET is carried in a POST envelope; keep its method/body.
            request = request.newBuilder().url(next).build()
        }
        throw IOException("重定向次数超限")
    }
}

internal val safeHttp: OkHttpClient = OkHttpClient.Builder()
    .connectTimeout(12, TimeUnit.SECONDS)
    .readTimeout(20, TimeUnit.SECONDS)
    .callTimeout(30, TimeUnit.SECONDS)
    .dns(PublicDns())
    .followRedirects(false)
    .followSslRedirects(false)
    .retryOnConnectionFailure(false)
    .addInterceptor(CheckedRedirects())
    .build()
