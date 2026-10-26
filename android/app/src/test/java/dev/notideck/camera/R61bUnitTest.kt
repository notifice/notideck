// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import dev.notideck.net.HealthPolicy
import dev.notideck.proto.v1.DeviceHealth
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.math.abs

class R61bUnitTest {
    private fun map(m: FloatArray, u: Float, v: Float) = (m[0] * u + m[1] * v) to (m[2] * u + m[3] * v)

    @Test fun coverMatrixCropsInsteadOfStretching() {
        val sizes = listOf(1280 to 720, 1920 to 1080, 640 to 480, 720 to 1280)
        for ((bw, bh) in sizes) for (rot in listOf(0, 90, 180, 270)) for (mirror in listOf(false, true)) {
            val m = CameraGeometry.coverMatrix(rot, mirror, bw, bh, bw, bh)
            // The whole output frame lies inside the buffer (no sampling outside the image).
            for (u in listOf(-0.5f, 0.5f)) for (v in listOf(-0.5f, 0.5f)) {
                val (x, y) = map(m, u, v)
                assertTrue("$bw x $bh rot=$rot mirror=$mirror: ($x,$y)", abs(x) <= 0.5001f && abs(y) <= 0.5001f)
            }
            // No stretching: a unit step in output pixels covers equal buffer pixels in both axes.
            val (hx, hy) = map(m, 1f / bw, 0f)
            val (vx, vy) = map(m, 0f, 1f / bh)
            val hLen = Math.hypot((hx * bw).toDouble(), (hy * bh).toDouble())
            val vLen = Math.hypot((vx * bw).toDouble(), (vy * bh).toDouble())
            assertEquals("rot=$rot", hLen, vLen, 1e-4)
            // Orthogonal axes (pure rotation/mirror + uniform scale).
            assertEquals(0.0, (hx * bw) * (vx * bw) + (hy * bh) * (vy * bh).toDouble(), 1e-3)
        }
    }

    @Test fun rotate90OfLandscapeBufferCropsToTheMiddleBand() {
        val m = CameraGeometry.coverMatrix(90, false, 1280, 720, 1280, 720)
        // 16:9 output of a 9:16 rotated image: the frame shows the middle (9/16)^2 of the buffer width
        val (x, y) = map(m, 0f, 0.5f)       // top-centre of the output
        assertEquals(0.5625 * 0.5 * 0.5625, abs(x).toDouble(), 1e-4)
        assertEquals(0f, y, 1e-6f)
        val (fx, fy) = map(m, 0.5f, 0f)     // right-centre reaches the buffer edge
        assertEquals(0f, fx, 1e-6f)
        assertEquals(0.5f, abs(fy), 1e-5f)
    }

    @Test fun identityForRotation0And180OnMatchingAspect() {
        val m0 = CameraGeometry.coverMatrix(0, false, 1280, 720, 1280, 720)
        assertEquals(1f, m0[0], 1e-6f); assertEquals(1f, m0[3], 1e-6f); assertEquals(0f, m0[1], 1e-6f)
        val m180 = CameraGeometry.coverMatrix(180, false, 1280, 720, 1280, 720)
        assertEquals(-1f, m180[0], 1e-6f); assertEquals(-1f, m180[3], 1e-6f)
        val mm = CameraGeometry.coverMatrix(0, true, 1280, 720, 1280, 720)
        assertEquals(-1f, mm[0], 1e-6f); assertEquals(1f, mm[3], 1e-6f)
    }

    @Test fun matrix4IsColumnMajor() {
        val m = CameraGeometry.coverMatrix(90, true, 1280, 720, 1280, 720)
        val g = CameraGeometry.coverMatrix4(90, true, 1280, 720, 1280, 720)
        assertEquals(m[0], g[0], 0f); assertEquals(m[1], g[4], 0f)
        assertEquals(m[2], g[1], 0f); assertEquals(m[3], g[5], 0f)
        assertEquals(1f, g[15], 0f)
    }

    @Test fun thermalStepUpNeedsFiveCoolMinutes() {
        var now = 0L
        val t = ThermalPolicy.StepUpTracker { now }
        assertFalse(t.update(39f, 0))
        now = 4 * 60_000L; assertFalse(t.update(39f, 0))
        now = 4 * 60_000L + 1000; assertFalse(t.update(40.5f, 0))   // warm again: timer restarts
        now += 4 * 60_000L; assertFalse(t.update(39f, 0))
        now += 4 * 60_000L; assertFalse(t.update(39f, 0))
        now += 61_000L; assertTrue(t.update(39f, 0))
        t.reset()
        now += 1000; assertFalse(t.update(39f, 0))
        now += 10 * 60_000L; assertFalse("status severe blocks", t.update(39f, 3))
    }

    @Test fun healthSendsOnSignificantChangeOnly() {
        val a = DeviceHealth(battery_percent = 80, battery_temp_c = 30f, charging = false, thermal_throttled = false)
        assertTrue(HealthPolicy.significant(null, a))
        assertFalse(HealthPolicy.significant(a, a.copy(battery_temp_c = 30.4f)))
        assertTrue(HealthPolicy.significant(a, a.copy(battery_temp_c = 31f)))
        assertTrue(HealthPolicy.significant(a, a.copy(battery_percent = 79)))
        assertTrue(HealthPolicy.significant(a, a.copy(charging = true)))
        assertTrue(HealthPolicy.significant(a, a.copy(thermal_throttled = true)))
    }
}
