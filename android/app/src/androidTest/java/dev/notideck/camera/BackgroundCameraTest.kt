// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.notideck.AppRuntime
import dev.notideck.MainActivity
import dev.notideck.net.ClientIdentity
import dev.notideck.net.ConnectionPlan
import dev.notideck.net.PinTrustManager
import dev.notideck.net.Tls
import dev.notideck.proto.v1.CameraStart
import dev.notideck.proto.v1.CameraState
import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.Lens
import dev.notideck.proto.v1.MediaAttached
import dev.notideck.proto.v1.Welcome
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.DataInputStream
import java.io.DataOutputStream
import java.net.InetAddress
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicInteger
import javax.net.ssl.SSLServerSocket
import javax.net.ssl.SSLSocket

/**
 * Whole app on the device against a fake host on 127.0.0.1:47800: the session foreground service,
 * CameraStart while the app is in the background, the deferred "open the app" path and DeviceHealth.
 */
@RunWith(AndroidJUnit4::class)
class BackgroundCameraTest {
    private val inst = InstrumentationRegistry.getInstrumentation()
    private val ctx = inst.targetContext
    private val serverId = ClientIdentity("notideck-test-server")

    private val received = CopyOnWriteArrayList<Envelope>()
    private val mediaPackets = AtomicInteger()
    private val conns = CopyOnWriteArrayList<SSLSocket>()
    @Volatile private var control: DataOutputStream? = null

    private fun send(e: Envelope) {
        val out = control!!
        val b = Envelope.ADAPTER.encode(e)
        synchronized(out) { out.writeInt(b.size); out.write(b); out.flush() }
    }

    private fun host(): SSLServerSocket {
        val tls = Tls.context(serverId.keyManager(), PinTrustManager(null))
        val ss = tls.serverSocketFactory.createServerSocket(ConnectionPlan.USB_PORT, 4, InetAddress.getByName("127.0.0.1")) as SSLServerSocket
        ss.enabledProtocols = arrayOf("TLSv1.3")
        ss.needClientAuth = true
        Thread {
            try {
                while (!ss.isClosed) {
                    val s = ss.accept() as SSLSocket
                    conns += s
                    Thread { serve(s) }.also { it.isDaemon = true; it.start() }
                }
            } catch (_: Exception) {}
        }.also { it.isDaemon = true; it.start() }
        return ss
    }

    private fun serve(s: SSLSocket) {
        try {
            s.startHandshake()
            val din = DataInputStream(s.inputStream)
            val dout = DataOutputStream(s.outputStream)
            fun read(): Envelope { val n = din.readInt(); val b = ByteArray(n); din.readFully(b); return Envelope.ADAPTER.decode(b) }
            val first = read()
            if (first.media_attach != null) {
                val r = Envelope.ADAPTER.encode(Envelope(media_attached = MediaAttached(ok = true)))
                dout.writeInt(r.size); dout.write(r); dout.flush()
                while (true) {
                    val h = ByteArray(12); din.readFully(h)
                    val n = ((h[8].toInt() and 0xFF) shl 24) or ((h[9].toInt() and 0xFF) shl 16) or
                        ((h[10].toInt() and 0xFF) shl 8) or (h[11].toInt() and 0xFF)
                    din.readFully(ByteArray(n)); mediaPackets.incrementAndGet()
                }
            }
            received += first
            control = dout
            send(Envelope(seq = 1, welcome = Welcome(session_id = "bg-1", host_name = "Test PC", proto_version = 2, host_id = "host-bg")))
            while (true) received += read()
        } catch (_: Exception) {
        } finally { try { s.close() } catch (_: Exception) {} }
    }

    private fun waitFor(what: String, ms: Long = 20_000, pred: () -> Boolean) {
        val end = System.currentTimeMillis() + ms
        while (!pred()) {
            if (System.currentTimeMillis() > end) {
                throw AssertionError("timeout: $what; status=${received.mapNotNull { it.camera_status?.let { s -> s.state to s.error } }}")
            }
            Thread.sleep(100)
        }
    }

    private fun states() = received.mapNotNull { it.camera_status?.state }
    private fun notif(id: Int) =
        (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).activeNotifications.firstOrNull { it.id == id }
    private fun title(id: Int) = notif(id)?.notification?.extras?.getString(android.app.Notification.EXTRA_TITLE)
    private fun shell(cmd: String) = inst.uiAutomation.executeShellCommand(cmd).close()

    @Test
    fun cameraStartsFromTheBackgroundAndDeferredStartResumes() {
        inst.uiAutomation.grantRuntimePermission(ctx.packageName, android.Manifest.permission.CAMERA)
        inst.uiAutomation.grantRuntimePermission(ctx.packageName, android.Manifest.permission.POST_NOTIFICATIONS)
        shell("input keyevent KEYCODE_HOME")
        Thread.sleep(800)
        val ss = host()
        try {
            // Phase A: connected while the app is not visible -> no session service, CameraStart is deferred.
            CameraService.appVisible = false
            inst.runOnMainSync { AppRuntime.get(ctx.applicationContext as android.app.Application).start() }
            waitFor("welcome") { control != null }
            Thread.sleep(1500)
            assertFalse("no service while hidden", CameraService.sessionUp)
            send(Envelope(camera_start = CameraStart(width = 640, height = 360, fps = 30, bitrate_bps = 1_000_000, lens = Lens.LENS_BACK)))
            waitFor("deferred error") { received.any { it.camera_status?.error == CameraController.OPEN_APP_MESSAGE } }
            waitFor("tap-to-open notification") { notif(CameraController.REQUEST_NOTIFICATION_ID) != null }

            // Phase B: the user opens the app: session service starts, the pending camera runs.
            ctx.startActivity(Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            waitFor("session service") { CameraService.sessionUp && CameraService.holdsCamera }
            waitFor("resumed STREAMING") { states().contains(CameraState.CAMERA_STATE_STREAMING) }
            waitFor("health") { received.any { it.health != null } }
            val h = received.first { it.health != null }.health!!
            assertTrue("battery ${h.battery_percent}", h.battery_percent in 0..100)
            waitFor("notification cleared") { notif(CameraController.REQUEST_NOTIFICATION_ID) == null }

            // Phase C: back to the background; stop, then start again with the app hidden.
            send(Envelope(camera_stop = dev.notideck.proto.v1.CameraStop()))
            waitFor("stopped") { states().last() == CameraState.CAMERA_STATE_STOPPED }
            waitFor("idle notification") { title(CameraService.NOTIFICATION_ID) == "Notideck connected to Test PC" }
            shell("input keyevent KEYCODE_HOME")
            waitFor("app hidden") { !CameraService.appVisible }
            val packetsBefore = mediaPackets.get()
            val streamingBefore = states().count { it == CameraState.CAMERA_STATE_STREAMING }
            send(Envelope(camera_start = CameraStart(width = 640, height = 360, fps = 30, bitrate_bps = 1_000_000, lens = Lens.LENS_BACK)))
            waitFor("background STREAMING") { states().count { it == CameraState.CAMERA_STATE_STREAMING } > streamingBefore }
            waitFor("background packets") { mediaPackets.get() > packetsBefore + 5 }
            assertEquals("Notideck is using the camera", title(CameraService.NOTIFICATION_ID))
            assertTrue(CameraService.sessionUp)
            send(Envelope(camera_stop = dev.notideck.proto.v1.CameraStop()))
            waitFor("stopped again") { states().last() == CameraState.CAMERA_STATE_STOPPED }
        } finally {
            ss.close(); conns.forEach { try { it.close() } catch (_: Exception) {} }
        }
        // Host gone: the session ends and the service stops.
        waitFor("service stops with the session", 20_000) { !CameraService.sessionUp && notif(CameraService.NOTIFICATION_ID) == null }
    }
}
