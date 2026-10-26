// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.notideck.pairing.MemoryHostStore
import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.Goodbye
import dev.notideck.proto.v1.GoodbyeReason
import dev.notideck.proto.v1.Welcome
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.DataInputStream
import java.io.DataOutputStream
import java.net.InetAddress
import javax.net.ssl.SSLServerSocket
import javax.net.ssl.SSLSocket

/**
 * Full stack on the device: a fake host listens on 127.0.0.1:47800 (what `adb reverse` looks like
 * to the app), speaks mutual TLS 1.3 and the v2 framing, and the real ConnectionManager walks
 * the USB first-contact path (TOFU) and the REVOKED path.
 */
@RunWith(AndroidJUnit4::class)
class ConnectionManagerTest {
    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private val clientId = ClientIdentity()
    private val serverId = ClientIdentity("notideck-test-server")

    private fun fakeHost(afterWelcome: (DataOutputStream) -> Unit, hello: MutableList<dev.notideck.proto.v1.Hello>): SSLServerSocket {
        val ctxTls = Tls.context(serverId.keyManager(), PinTrustManager(null))
        val ss = ctxTls.serverSocketFactory.createServerSocket(
            ConnectionPlan.USB_PORT, 4, InetAddress.getByName("127.0.0.1")
        ) as SSLServerSocket
        ss.enabledProtocols = arrayOf("TLSv1.3")
        ss.needClientAuth = true
        Thread {
            try {
                while (!ss.isClosed) {
                    val s = ss.accept() as SSLSocket
                    try {
                        s.startHandshake()
                        val din = DataInputStream(s.inputStream)
                        val dout = DataOutputStream(s.outputStream)
                        val len = din.readInt()
                        val buf = ByteArray(len); din.readFully(buf)
                        hello += Envelope.ADAPTER.decode(buf).hello!!
                        val w = Envelope(seq = 1, welcome = Welcome(session_id = "s1", host_name = "Test PC", proto_version = 2, host_id = "host-1"))
                        Envelope.ADAPTER.encode(w).let { dout.writeInt(it.size); dout.write(it); dout.flush() }
                        afterWelcome(dout)
                        while (din.read() >= 0) { /* drain until the phone hangs up */ }
                    } catch (_: Exception) {
                    } finally { try { s.close() } catch (_: Exception) {} }
                }
            } catch (_: Exception) {}
        }.also { it.isDaemon = true; it.start() }
        return ss
    }

    private suspend fun awaitStatus(client: SessionClient, pred: (ConnectionStatus) -> Boolean): ConnectionStatus =
        withTimeout(15_000) { client.connectionStatus.first(pred) }

    @Test
    fun usbFirstContactIsTofuAndHelloIsV2() = runBlocking {
        val hellos = mutableListOf<dev.notideck.proto.v1.Hello>()
        val ss = fakeHost({}, hellos)
        val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
        val store = MemoryHostStore()
        val client = SessionClient(ctx)
        val mgr = ConnectionManager(client, store, clientId, NoDiscovery(), scope = scope, deviceName = "Test Phone")
        try {
            mgr.start()
            val st = awaitStatus(client) { it is ConnectionStatus.Connected } as ConnectionStatus.Connected
            assertEquals(Transport.USB, st.transport)
            assertEquals("Test PC", st.hostName)
            val host = store.get("host-1")
            assertNotNull(host)
            assertEquals(serverId.fingerprintHex(), host!!.fp)
            assertEquals(2, hellos.single().proto_version)
            assertEquals(clientId.fingerprintHex(), hellos.single().cert_fp)
            assertEquals("Test Phone", hellos.single().device_name)
        } finally {
            mgr.stop(); scope.cancel(); ss.close()
        }
    }

    @Test
    fun revokedForgetsTheHost() = runBlocking {
        val hellos = mutableListOf<dev.notideck.proto.v1.Hello>()
        val ss = fakeHost({ out ->
            Thread.sleep(300)
            Envelope(seq = 2, goodbye = Goodbye(reason = GoodbyeReason.GOODBYE_REASON_REVOKED)).let {
                val b = Envelope.ADAPTER.encode(it); out.writeInt(b.size); out.write(b); out.flush()
            }
        }, hellos)
        val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
        val store = MemoryHostStore()
        val client = SessionClient(ctx)
        val mgr = ConnectionManager(client, store, clientId, NoDiscovery(), scope = scope, deviceName = "Test Phone")
        try {
            mgr.start()
            val ev = withTimeout(15_000) { mgr.events.first() }
            assertTrue(ev is ManagerEvent.HostRemoved)
            assertNull(store.get("host-1"))
            val st = awaitStatus(client) { it is ConnectionStatus.Notice } as ConnectionStatus.Notice
            assertEquals("This PC removed the phone", st.message)
        } finally {
            mgr.stop(); scope.cancel(); ss.close()
        }
    }
}
