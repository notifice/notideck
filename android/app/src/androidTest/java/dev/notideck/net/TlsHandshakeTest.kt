// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.notideck.pairing.PairingCrypto
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import javax.net.ssl.SSLServerSocket
import javax.net.ssl.SSLSocket

/**
 * Proves Conscrypt can sign a TLS 1.3 mutual-auth handshake with non-exportable AndroidKeyStore
 * P-256 keys (the ADR 0004 risk), on both the client and a throwaway local server.
 */
@RunWith(AndroidJUnit4::class)
class TlsHandshakeTest {
    private val client = ClientIdentity()
    private val server = ClientIdentity("notideck-test-server")

    private fun startServer(): Triple<SSLServerSocket, PinTrustManager, Thread> {
        val trust = PinTrustManager(null)
        val ctx = Tls.context(server.keyManager(), trust)
        val ss = ctx.serverSocketFactory.createServerSocket(0, 1, java.net.InetAddress.getByName("127.0.0.1")) as SSLServerSocket
        ss.enabledProtocols = arrayOf("TLSv1.3")
        ss.needClientAuth = true
        val t = Thread {
            try {
                (ss.accept() as SSLSocket).use { s ->
                    s.startHandshake()
                    val b = s.inputStream.read()
                    s.outputStream.write(b); s.outputStream.flush()
                }
            } catch (_: Exception) {}
        }.also { it.isDaemon = true; it.start() }
        return Triple(ss, trust, t)
    }

    @Test
    fun tls13MutualAuthWithKeystoreKeys() {
        val (ss, serverTrust, t) = startServer()
        val link = Tls.connect("127.0.0.1", ss.localPort, server.fingerprintHex(), client, 3000)
        link.socket.use { s ->
            assertEquals("TLSv1.3", s.session.protocol)
            assertEquals(server.fingerprintHex(), link.serverFp)
            s.outputStream.write(42); s.outputStream.flush()
            assertEquals(42, s.inputStream.read())
        }
        t.join(2000)
        assertEquals(client.fingerprintHex(), serverTrust.seenFp)
        assertEquals(64, client.fingerprintHex().length)
        assertTrue(PairingCrypto.isFingerprintHex(client.fingerprintHex()))
        ss.close()
    }

    @Test
    fun wrongPinIsRejected() {
        val (ss, _, _) = startServer()
        try {
            Tls.connect("127.0.0.1", ss.localPort, "00".repeat(32), client, 3000)
            fail("handshake must fail on a pin mismatch")
        } catch (e: java.io.IOException) {
            // expected
        } finally {
            ss.close()
        }
    }

    @Test
    fun identityIsStable() {
        assertEquals(client.fingerprintHex(), ClientIdentity().fingerprintHex())
    }
}
