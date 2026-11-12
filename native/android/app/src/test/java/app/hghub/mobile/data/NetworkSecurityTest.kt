package app.hghub.mobile.data

import okhttp3.Dns
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.tls.HandshakeCertificates
import okhttp3.tls.HeldCertificate
import org.junit.Assert.*
import org.junit.Test
import java.io.IOException
import java.net.InetAddress

class NetworkSecurityTest {
    private fun fixedDns(vararg addresses: InetAddress) = object : Dns {
        override fun lookup(hostname: String) = addresses.toList()
    }

    @Test fun unsafeTargetsAndOriginsAreRejected() {
        for (url in listOf("http://example.com", "https://user:pass@example.com", "https://localhost.",
            "https://x.localhost", "https://x.internal", "https://home.arpa", "https://127.1",
            "https://2130706433", "https://[::1]", "https://[::ffff:127.0.0.1]", "https://100.64.0.1")) {
            assertThrows(url, IOException::class.java) { checkedUrl(url) }
        }
        for (url in listOf("https://example.com/path", "https://example.com/?q=x", "https://example.com/#x")) {
            assertThrows(url, IOException::class.java) { checkedOrigin(url) }
        }
        assertEquals("https://example.com", checkedOrigin("https://EXAMPLE.com:443/"))
        assertTrue(trustedOrigin("https://huangguoai.com"))
        assertFalse(trustedOrigin("https://huangguoai.com.evil.test"))
    }

    @Test fun dnsRejectsMixedAnswersAndKeepsFakeIp() {
        val public = InetAddress.getByName("8.8.8.8")
        val private = InetAddress.getByName("192.168.1.1")
        val fake = InetAddress.getByName("198.18.0.1")
        assertThrows(IOException::class.java) { PublicDns(fixedDns(public, private)).lookup("example.test") }
        assertEquals(listOf(public, fake), PublicDns(fixedDns(public, fake)).lookup("example.test"))
        for (ip in listOf("2001:db8::1", "64:ff9b::a00:1", "2002:a00:1::1", "fc00::1")) {
            assertFalse(ip, publicAddress(InetAddress.getByName(ip)))
        }
    }

    private fun withServer(test: (MockWebServer, okhttp3.OkHttpClient) -> Unit) {
        val certificate = HeldCertificate.Builder().addSubjectAlternativeName("example.test").build()
        val serverTls = HandshakeCertificates.Builder().heldCertificate(certificate).build()
        val clientTls = HandshakeCertificates.Builder().addTrustedCertificate(certificate.certificate).build()
        MockWebServer().use { server ->
            server.useHttps(serverTls.sslSocketFactory(), false)
            server.start()
            val client = safeHttp.newBuilder()
                .sslSocketFactory(clientTls.sslSocketFactory(), clientTls.trustManager)
                .dns(fixedDns(InetAddress.getByName("127.0.0.1"))) // Test transport only.
                .build()
            test(server, client)
            client.connectionPool.evictAll()
            client.dispatcher.executorService.shutdown()
        }
    }

    @Test fun accountBodiesAndBearerTokensNeverFollowRedirects() = withServer { server, client ->
        for (code in listOf(301, 302, 303, 307, 308)) {
            server.enqueue(MockResponse().setResponseCode(code).addHeader("Location", "/leak"))
            val request = Request.Builder().url(server.url("/account").newBuilder().host("example.test").build())
                .post("private-body".toRequestBody()).build()
            client.newCall(request).execute().use { assertEquals(code, it.code) }
            val received = server.takeRequest()
            assertEquals("/account", received.path)
            assertEquals("private-body", received.body.readUtf8())
        }
        server.enqueue(MockResponse().setResponseCode(307).addHeader("Location", "/leak"))
        val authenticated = Request.Builder().url(server.url("/account").newBuilder().host("example.test").build())
            .header("Authorization", "Bearer private-token").build()
        client.newCall(authenticated).execute().use { assertEquals(307, it.code) }
        assertEquals("/account", server.takeRequest().path)
        assertEquals(6, server.requestCount)
    }

    @Test fun guestEnvelopeKeepsItsPostBodyAndRejectsPrivateRedirects() = withServer { server, client ->
        val url = server.url("/edge").newBuilder().host("example.test").build()
        server.enqueue(MockResponse().setResponseCode(302).addHeader("Location", "/next"))
        server.enqueue(MockResponse().setBody("ok"))
        val request = Request.Builder().url(url).post("guest-envelope".toRequestBody())
            .tag(GuestEnvelopeRead::class.java, GuestEnvelopeRead()).build()
        client.newCall(request).execute().use { assertEquals("ok", it.body!!.string()) }
        repeat(2) {
            val received = server.takeRequest()
            assertEquals("POST", received.method)
            assertEquals("guest-envelope", received.body.readUtf8())
        }
        server.enqueue(MockResponse().setResponseCode(302).addHeader("Location", "https://127.0.0.1/leak"))
        assertThrows(IOException::class.java) { client.newCall(request).execute().close() }
        assertEquals(3, server.requestCount)
    }
}
