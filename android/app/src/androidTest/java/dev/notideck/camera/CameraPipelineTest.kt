// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.notideck.net.ClientIdentity
import dev.notideck.net.PinTrustManager
import dev.notideck.net.Tls
import dev.notideck.proto.v1.CameraControl
import dev.notideck.proto.v1.CameraStart
import dev.notideck.proto.v1.CameraState
import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.Lens
import dev.notideck.proto.v1.MediaAttached
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.DataInputStream
import java.io.DataOutputStream
import java.net.InetAddress
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLServerSocket
import javax.net.ssl.SSLSocket

/** Runs the real camera + encoder against a local TLS "host" that speaks the media side of ADR 0005. */
@RunWith(AndroidJUnit4::class)
class CameraPipelineTest {
    @Before fun grantCamera() {
        InstrumentationRegistry.getInstrumentation().uiAutomation
            .apply {
                grantRuntimePermission(ctx.packageName, android.Manifest.permission.CAMERA)
                grantRuntimePermission(ctx.packageName, android.Manifest.permission.POST_NOTIFICATIONS)
            }
    }

    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private val clientId = ClientIdentity()
    private val serverId = ClientIdentity("notideck-test-server")
    private var server: SSLServerSocket? = null

    class Rx(val config: Boolean, val key: Boolean, val pts: Long, val data: ByteArray)

    private class FakeHost(val server: SSLServerSocket) {
        val attachedSession = LinkedBlockingQueue<String>()
        val packets = LinkedBlockingQueue<Rx>()
        val connections = java.util.concurrent.atomic.AtomicInteger()

        init {
            Thread {
                try {
                    while (!server.isClosed) {
                        val s = server.accept() as SSLSocket
                        connections.incrementAndGet()
                        Thread { serve(s) }.also { it.isDaemon = true; it.start() }
                    }
                } catch (_: Exception) {}
            }.also { it.isDaemon = true; it.start() }
        }

        private fun serve(s: SSLSocket) {
            try {
                s.startHandshake()
                val din = DataInputStream(s.inputStream)
                val dout = DataOutputStream(s.outputStream)
                val len = din.readInt()
                val buf = ByteArray(len); din.readFully(buf)
                attachedSession.add(Envelope.ADAPTER.decode(buf).media_attach!!.session_id)
                val reply = Envelope.ADAPTER.encode(Envelope(media_attached = MediaAttached(ok = true)))
                dout.writeInt(reply.size); dout.write(reply); dout.flush()
                while (true) {
                    val h = ByteArray(12); din.readFully(h)
                    var v = 0L
                    for (i in 0 until 8) v = (v shl 8) or (h[i].toLong() and 0xFF)
                    val n = ((h[8].toInt() and 0xFF) shl 24) or ((h[9].toInt() and 0xFF) shl 16) or
                        ((h[10].toInt() and 0xFF) shl 8) or (h[11].toInt() and 0xFF)
                    val data = ByteArray(n); din.readFully(data)
                    packets.add(Rx(v < 0, v and (1L shl 62) != 0L, v and ((1L shl 62) - 1), data))
                }
            } catch (_: Exception) {
            } finally { try { s.close() } catch (_: Exception) {} }
        }
    }

    private fun startHost(): FakeHost {
        val tls = Tls.context(serverId.keyManager(), PinTrustManager(null))
        val ss = tls.serverSocketFactory.createServerSocket(0, 4, InetAddress.getByName("127.0.0.1")) as SSLServerSocket
        ss.enabledProtocols = arrayOf("TLSv1.3")
        ss.needClientAuth = true
        server = ss
        return FakeHost(ss)
    }

    @After fun tearDown() { try { server?.close() } catch (_: Exception) {} }

    private fun nalTypes(data: ByteArray): List<Int> {
        val out = ArrayList<Int>()
        var i = 0
        while (i + 3 < data.size) {
            if (data[i].toInt() == 0 && data[i + 1].toInt() == 0 &&
                (data[i + 2].toInt() == 1 || (data[i + 2].toInt() == 0 && data[i + 3].toInt() == 1))) {
                val start = if (data[i + 2].toInt() == 1) i + 3 else i + 4
                if (start < data.size) out += data[start].toInt() and 0x1F
                i = start
            } else i++
        }
        return out
    }

    @Test
    fun configThenKeyFrameArrive() {
        val host = startHost()
        val caps = CameraCapabilities(ctx)
        assertTrue("emulator should expose a camera", caps.hasCamera)
        assertTrue("mode list not empty", caps.modes.isNotEmpty())
        val choice = CameraModes.pickClosest(1280, 720, 30, Lens.LENS_BACK, caps.modes)!!
        val streaming = java.util.concurrent.CountDownLatch(1)
        val pipeline = CameraPipeline(
            ctx, caps, PipelineConfig(choice.lens, choice.width, choice.height, choice.fps, 2_000_000, false),
            "session-1",
            { Tls.connect("127.0.0.1", host.server.localPort, null, clientId, 2000).socket },
            object : PipelineListener {
                override fun onStreaming() { streaming.countDown() }
                override fun onFailure(message: String) { throw AssertionError("pipeline failed: $message") }
            }
        )
        try {
            pipeline.start()
            assertEquals("session-1", host.attachedSession.poll(5, TimeUnit.SECONDS))
            val first = host.packets.poll(8, TimeUnit.SECONDS)!!
            assertTrue("first packet is CONFIG", first.config)
            assertTrue("SPS+PPS in config: ${nalTypes(first.data)}", nalTypes(first.data).containsAll(listOf(7, 8)))
            var key: Rx? = null
            val deadline = System.currentTimeMillis() + 8000
            while (key == null && System.currentTimeMillis() < deadline) {
                val p = host.packets.poll(1, TimeUnit.SECONDS) ?: continue
                if (p.key) key = p
            }
            assertTrue("KEY_FRAME packet arrived", key != null)
            assertTrue("IDR NAL in key frame", nalTypes(key!!.data).contains(5))
            assertTrue("streaming reported", streaming.await(3, TimeUnit.SECONDS))
            // delta frames keep coming
            var deltas = 0
            val end = System.currentTimeMillis() + 3000
            while (System.currentTimeMillis() < end && deltas < 5) {
                val p = host.packets.poll(500, TimeUnit.MILLISECONDS) ?: continue
                if (!p.config && !p.key) deltas++
            }
            assertTrue("delta frames flow ($deltas)", deltas >= 3 || host.packets.isNotEmpty())
            // sync frame request yields another key frame
            pipeline.requestKeyFrame()
            var again = false
            val end2 = System.currentTimeMillis() + 4000
            while (!again && System.currentTimeMillis() < end2) {
                val p = host.packets.poll(500, TimeUnit.MILLISECONDS) ?: continue
                if (p.key) again = true
            }
            assertTrue("keyframe on request", again)
        } finally {
            pipeline.stop()
        }
    }

    @Test
    fun controllerRunsFullLifecycleWithLensSwitch() {
        val host = startHost()
        val sent = CopyOnWriteArrayList<Envelope>()
        val caps = CameraCapabilities(ctx)
        val c = CameraController(
            ctx, { sent += it },
            { Tls.connect("127.0.0.1", host.server.localPort, null, clientId, 2000).socket },
            caps, startService = false
        )
        fun states() = sent.mapNotNull { it.camera_status?.state }
        fun waitFor(what: String, ms: Long = 10_000, pred: () -> Boolean) {
            val end = System.currentTimeMillis() + ms
            while (!pred()) {
                if (System.currentTimeMillis() > end) throw AssertionError("timeout: $what, states=${states()}")
                Thread.sleep(50)
            }
        }
        try {
            c.onSessionStart("session-2")
            waitFor("initial STOPPED") { states().contains(CameraState.CAMERA_STATE_STOPPED) }
            val st0 = sent.first { it.camera_status != null }.camera_status!!
            assertTrue(st0.supported_modes.isNotEmpty())
            c.onCameraStart(CameraStart(width = 1280, height = 720, fps = 30, bitrate_bps = 2_000_000, lens = Lens.LENS_BACK))
            waitFor("STREAMING") { states().contains(CameraState.CAMERA_STATE_STREAMING) }
            assertEquals(CameraState.CAMERA_STATE_STARTING, states().first { it != CameraState.CAMERA_STATE_STOPPED })
            val active = sent.last { it.camera_status?.state == CameraState.CAMERA_STATE_STREAMING }.camera_status!!.active!!
            assertEquals(Lens.LENS_BACK, active.lens)
            assertTrue(c.ui.value.streaming)
            assertTrue(host.packets.poll(5, TimeUnit.SECONDS)!!.config)

            if (caps.modes.any { it.lens == Lens.LENS_FRONT && it.width == active.width && it.height == active.height }) {
                val before = sent.size
                c.onCameraControl(CameraControl(lens = Lens.LENS_FRONT))
                waitFor("lens switch status") {
                    sent.drop(before).any { it.camera_status?.active?.lens == Lens.LENS_FRONT }
                }
            }
            c.onCameraControl(CameraControl(zoom_ratio = 1.0f, exposure_comp = 0, focus_x = 0.5f, focus_y = 0.5f, request_keyframe = true))
            Thread.sleep(500)
            c.onCameraStop()
            waitFor("STOPPED after stop") { states().last() == CameraState.CAMERA_STATE_STOPPED && !c.ui.value.streaming }
            assertTrue(states().none { it == CameraState.CAMERA_STATE_ERROR })
        } finally {
            c.onSessionEnd()
            c.release()
        }
    }

    @Test
    fun foregroundServiceNotificationAppearsAndGoesAway() {
        val host = startHost()
        val sent = CopyOnWriteArrayList<Envelope>()
        val c = CameraController(
            ctx, { sent += it },
            { Tls.connect("127.0.0.1", host.server.localPort, null, clientId, 2000).socket },
            CameraCapabilities(ctx), startService = true
        )
        CameraService.appVisible = true
        val nm = ctx.getSystemService(android.content.Context.NOTIFICATION_SERVICE) as android.app.NotificationManager
        fun shown() = nm.activeNotifications.any { it.id == CameraService.NOTIFICATION_ID }
        fun waitFor(what: String, ms: Long = 10_000, pred: () -> Boolean) {
            val end = System.currentTimeMillis() + ms
            while (!pred()) {
                if (System.currentTimeMillis() > end) throw AssertionError("timeout: $what, ${sent.mapNotNull { it.camera_status?.state }}")
                Thread.sleep(50)
            }
        }
        try {
            c.onSessionStart("session-3")
            c.onCameraStart(CameraStart(width = 640, height = 360, fps = 30, bitrate_bps = 1_000_000, lens = Lens.LENS_BACK))
            waitFor("STREAMING") { sent.any { it.camera_status?.state == CameraState.CAMERA_STATE_STREAMING } }
            waitFor("notification") { shown() }
            val n = nm.activeNotifications.first { it.id == CameraService.NOTIFICATION_ID }
            assertEquals("Notideck is using the camera", n.notification.extras.getString(android.app.Notification.EXTRA_TITLE))
            c.onCameraStop()
            waitFor("notification removed") { !shown() }
        } finally {
            c.onSessionEnd(); c.release()
        }
    }
}
