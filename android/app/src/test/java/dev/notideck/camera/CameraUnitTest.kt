// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import dev.notideck.proto.v1.CameraMode
import dev.notideck.proto.v1.Lens
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MediaHeaderTest {
    private fun bytes(vararg v: Int) = ByteArray(v.size) { v[it].toByte() }

    @Test fun keyFrameVector() {
        // pts 123456789 us = 0x075BCD15, KEY_FRAME = bit 62, len 5. Matches nd_proto::media::MediaHeader::encode.
        assertArrayEquals(
            bytes(0x40, 0x00, 0x00, 0x00, 0x07, 0x5B, 0xCD, 0x15, 0x00, 0x00, 0x00, 0x05),
            MediaPacket.header(123_456_789L, config = false, keyFrame = true, len = 5)
        )
    }

    @Test fun configVector() {
        assertArrayEquals(
            bytes(0x80, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x00, 0x01, 0x00),
            MediaPacket.header(0, config = true, keyFrame = false, len = 256)
        )
    }

    @Test fun deltaFrameAndPtsMask() {
        assertArrayEquals(
            bytes(0, 0, 0, 0, 0, 0x01, 0x00, 0x00, 0, 0, 0, 1),
            MediaPacket.header(65536L, false, false, 1)
        )
        // PTS bits above 62 are masked off so they cannot set flags.
        val h = MediaPacket.header(-1L, false, false, 0)
        assertEquals(0x3F, h[0].toInt() and 0xFF)
    }
}

class ModeSelectionTest {
    private fun m(lens: Lens, w: Int, h: Int, vararg fps: Int) = CameraMode(lens, w, h, fps.toList())

    @Test fun selectFiltersAndTrims() {
        val sizes = listOf(
            SizeCaps(4000, 3000, listOf(30)),          // too big
            SizeCaps(1080, 1920, listOf(30)),          // portrait
            SizeCaps(1920, 1080, listOf(30, 24)),
            SizeCaps(1920, 1080, listOf(60)),          // duplicate size merges
            SizeCaps(1280, 720, listOf(30)),
            SizeCaps(1280, 960, listOf(30)),
            SizeCaps(1024, 768, listOf(30)),
            SizeCaps(960, 540, listOf(30)),
            SizeCaps(800, 600, listOf(30)),
            SizeCaps(800, 480, listOf(30)),
            SizeCaps(720, 480, listOf(30)),
            SizeCaps(640, 480, listOf(30)),
            SizeCaps(640, 360, listOf(30)),
            SizeCaps(352, 288, listOf(30)),
            SizeCaps(320, 240, listOf(30)),
            SizeCaps(176, 144, listOf(30)),
            SizeCaps(1600, 1200, emptyList())          // no fps
        )
        val out = CameraModes.select(sizes)
        assertEquals(12, out.size)
        assertEquals(out.map { it.width to it.height }.distinct().size, out.size)
        assertTrue(out.any { it.width == 1920 && it.height == 1080 && it.fps == listOf(60, 30, 24) })
        assertTrue(out.any { it.width == 1280 && it.height == 720 })
        assertTrue(out.any { it.width == 640 && it.height == 360 })
        assertTrue(out.none { it.height > it.width || it.width > 1920 })
        assertEquals(out.sortedByDescending { it.width * it.height }, out)
    }

    @Test fun anchorsSurviveTrimming() {
        val many = (0 until 30).map { SizeCaps(1900 - it * 20, 1060 - it * 10, listOf(30)) } +
            SizeCaps(1280, 720, listOf(30)) + SizeCaps(640, 360, listOf(30))
        val out = CameraModes.select(many)
        assertEquals(12, out.size)
        assertTrue(out.any { it.width == 1280 && it.height == 720 })
        assertTrue(out.any { it.width == 640 && it.height == 360 })
    }

    @Test fun protoCarriesLens() {
        val p = CameraModes.toProto(Lens.LENS_FRONT, listOf(SizeCaps(1280, 720, listOf(30, 15))))
        assertEquals(listOf(m(Lens.LENS_FRONT, 1280, 720, 30, 15)), p)
    }

    private val modes = listOf(
        m(Lens.LENS_BACK, 1920, 1080, 30, 24),
        m(Lens.LENS_BACK, 1280, 720, 30, 24, 15),
        m(Lens.LENS_BACK, 640, 360, 30),
        m(Lens.LENS_FRONT, 1280, 720, 30),
        m(Lens.LENS_FRONT, 640, 480, 30)
    )

    @Test fun exactModeIsKept() {
        assertEquals(CameraModes.Choice(Lens.LENS_BACK, 1280, 720, 24), CameraModes.pickClosest(1280, 720, 24, Lens.LENS_BACK, modes))
    }

    @Test fun closestSizeAndFps() {
        // 1600x900 is nearest to 1920x1080 in log-area (ratio 1.35) vs 1280x720 (1.56)
        assertEquals(CameraModes.Choice(Lens.LENS_BACK, 1920, 1080, 30), CameraModes.pickClosest(1600, 900, 29, Lens.LENS_BACK, modes))
        // 60 fps asked, best is 30
        assertEquals(30, CameraModes.pickClosest(1280, 720, 60, Lens.LENS_BACK, modes)!!.fps)
        // tie between 24 and ... prefers lower
        assertEquals(24, CameraModes.pickClosest(1280, 720, 20, Lens.LENS_BACK, modes)!!.fps)
    }

    @Test fun aspectMattersAndLensFallback() {
        assertEquals(640 to 480, CameraModes.pickClosest(640, 480, 30, Lens.LENS_FRONT, modes)!!.let { it.width to it.height })
        // external lens missing: falls back to whatever exists
        assertNotNull(CameraModes.pickClosest(1280, 720, 30, Lens.LENS_EXTERNAL, modes))
        assertEquals(Lens.LENS_BACK, CameraModes.pickClosest(0, 0, 0, Lens.LENS_UNSPECIFIED, modes)!!.lens)
        assertEquals(null, CameraModes.pickClosest(1280, 720, 30, Lens.LENS_BACK, emptyList()))
    }

    @Test fun throttleCapsAt720p24() {
        val t = CameraModes.throttled(CameraModes.Choice(Lens.LENS_BACK, 1920, 1080, 30), modes)
        assertEquals(CameraModes.Choice(Lens.LENS_BACK, 1280, 720, 24), t)
        val small = CameraModes.throttled(CameraModes.Choice(Lens.LENS_BACK, 640, 360, 30), modes)
        assertEquals(640, small.width)
        assertTrue(small.fps <= 30)
        // front lens has only 30 fps: keeps size cap at least
        val f = CameraModes.throttled(CameraModes.Choice(Lens.LENS_FRONT, 1920, 1080, 30), modes)
        assertTrue(f.width * f.height <= 1280 * 720)
    }
}

class ThermalPolicyTest {
    @Test fun thresholds() {
        val d = ThermalPolicy::decide
        assertEquals(ThermalPolicy.Action.None, d(38f, 0, false))
        assertEquals(ThermalPolicy.Action.Throttle, d(42f, 0, false))
        assertEquals(ThermalPolicy.Action.None, d(43f, 0, true))
        assertEquals(ThermalPolicy.Action.Throttle, d(30f, 3, false))
        assertEquals(ThermalPolicy.Action.Stop, d(46f, 0, true))
        assertEquals(ThermalPolicy.Action.Stop, d(30f, 4, false))
    }
}

class GeometryTest {
    @Test fun uprightRotation() {
        assertEquals(0, CameraGeometry.uprightRotation(90, 90, false))
        assertEquals(180, CameraGeometry.uprightRotation(90, 270, false))
        assertEquals(180, CameraGeometry.uprightRotation(270, 90, false))
        assertEquals(180, CameraGeometry.uprightRotation(270, 270, true) % 360 + 0)
    }

    @Test fun focusMapping() {
        assertEquals(0.25f to 0.75f, CameraGeometry.outputToSensor(0.25f, 0.75f, 0, false))
        assertEquals(0.75f to 0.75f, CameraGeometry.outputToSensor(0.25f, 0.75f, 0, true))
        assertEquals(0.75f to 0.25f, CameraGeometry.outputToSensor(0.25f, 0.75f, 180, false))
    }

    @Test fun zoomCrop() {
        assertArrayEquals(intArrayOf(500, 250, 1500, 750), CameraGeometry.cropForZoom(2000, 1000, 2f))
        assertArrayEquals(intArrayOf(0, 0, 2000, 1000), CameraGeometry.cropForZoom(2000, 1000, 0.5f))
    }
}

class BackpressureTest {
    private fun frame(ptsMs: Long, key: Boolean = false, size: Int = 100) =
        MediaPacket(ptsMs * 1000, false, key, ByteArray(size))

    @Test fun passesWhenKeepingUp() {
        val q = MediaBackpressureQueue(maxQueuedUs = 300_000)
        for (i in 0 until 5) assertEquals(MediaBackpressureQueue.Offer.Queued, q.offer(frame(i * 33L, key = i == 0)))
        assertEquals(5, q.size)
        assertEquals(0, q.dropped)
    }

    @Test fun dropsDeltasAfterBackupAndAsksSyncOnce() {
        val q = MediaBackpressureQueue(maxQueuedUs = 300_000)
        q.offer(frame(0, key = true))
        var t = 33L
        while (t <= 330) { q.offer(frame(t)); t += 33 }   // span 330 ms > 300
        val first = q.offer(frame(t))
        assertEquals(MediaBackpressureQueue.Offer.Dropped(true), first)
        val sizeAtBackup = q.size
        // further deltas dropped silently, queue does not grow
        for (i in 1..50) assertEquals(MediaBackpressureQueue.Offer.Dropped(false), q.offer(frame(t + i * 33L)))
        assertEquals(sizeAtBackup, q.size)
    }

    @Test fun keyFrameReplacesStaleQueueButKeepsConfig() {
        val q = MediaBackpressureQueue(maxQueuedUs = 100_000)
        q.offer(MediaPacket(0, true, false, ByteArray(20)))
        q.offer(frame(0, key = true))
        for (i in 1..6) q.offer(frame(i * 33L))
        assertTrue(q.offer(frame(300)) is MediaBackpressureQueue.Offer.Dropped)
        assertEquals(MediaBackpressureQueue.Offer.Queued, q.offer(frame(333, key = true)))
        assertEquals(2, q.size) // config + new key
        val a = q.take()!!
        assertTrue(a.config)
        assertTrue(q.take()!!.keyFrame)
        // deltas flow again
        assertEquals(MediaBackpressureQueue.Offer.Queued, q.offer(frame(366)))
    }

    @Test fun byteLimitAlsoTriggers() {
        val q = MediaBackpressureQueue(maxQueuedUs = 10_000_000, maxBytes = 1000)
        q.offer(frame(0, key = true, size = 800))
        assertEquals(MediaBackpressureQueue.Offer.Dropped(true), q.offer(frame(33, size = 400)))
    }

    @Test fun closeUnblocksTake() {
        val q = MediaBackpressureQueue()
        var result: MediaPacket? = frame(0)
        val t = Thread { result = q.take() }
        t.start(); Thread.sleep(50); q.close(); t.join(1000)
        assertFalse(t.isAlive)
        assertEquals(null, result)
    }
}
