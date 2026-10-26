// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class CameraImageMapperTest {
    private val M = CameraImageMapper

    @Test fun focusPermilleRoundTripsThroughDiopters() {
        val min = 10f
        assertEquals(0f, M.focusDiopters(0, min), 1e-6f)
        assertEquals(10f, M.focusDiopters(1000, min), 1e-6f)
        assertEquals(2.5f, M.focusDiopters(250, min), 1e-6f)
        for (p in listOf(0, 1, 333, 500, 999, 1000)) assertEquals(p, M.focusPermille(M.focusDiopters(p, min), min))
        assertEquals(1000, M.focusDiopters(5000, min).let { M.focusPermille(it, min) })
        assertEquals(0, M.focusPermille(-1f, min))
        assertEquals(1000, M.focusPermille(99f, min))
        assertEquals(0, M.focusPermille(3f, 0f))
        assertEquals(0, M.focusPermille(Float.NaN, min))
    }

    @Test fun focusModesFallBackToContinuous() {
        assertEquals(3, M.effectiveFocus(3, focusManual = true, focusLock = true))
        assertEquals(1, M.effectiveFocus(3, focusManual = false, focusLock = true))
        assertEquals(2, M.effectiveFocus(2, focusManual = false, focusLock = true))
        assertEquals(1, M.effectiveFocus(2, focusManual = true, focusLock = false))
        assertEquals(1, M.effectiveFocus(0, true, true))
        assertEquals(M.AF_OFF, M.afMode(3, true))
        assertEquals(M.AF_AUTO, M.afMode(2, false))
        assertEquals(M.AF_CONTINUOUS_VIDEO, M.afMode(1, true))
        assertNull(M.afMode(1, false))
    }

    @Test fun exposureIsCappedAtFrameDuration() {
        // 1/30 s frame: a 100 ms request is cut to 33.3 ms
        val m = M.manualExposure(800, 100_000, 100, 3200, 50, 1_000_000, 30, 0, 0)
        assertEquals(1_000_000_000L / 30, m.exposureNs)
        assertEquals(1_000_000_000L / 30, m.frameDurationNs)
        assertEquals(800, m.iso)
        val short = M.manualExposure(800, 8_000, 100, 3200, 50, 1_000_000, 30, 0, 0)
        assertEquals(8_000_000L, short.exposureNs)
    }

    @Test fun isoAndExposureAreClamped() {
        val hi = M.manualExposure(99_999, 1_000_000_000, 100, 3200, 100, 200_000, 4, 0, 0)
        assertEquals(3200, hi.iso)
        assertEquals(200_000_000L, hi.exposureNs)
        val lo = M.manualExposure(5, 1, 100, 3200, 100, 200_000, 30, 0, 0)
        assertEquals(100, lo.iso)
        assertEquals(100_000L, lo.exposureNs)
    }

    @Test fun zeroKeepsTheCurrentValueOrASafeMiddle() {
        val cur = M.manualExposure(0, 0, 100, 3200, 50, 1_000_000, 30, 640, 12_000)
        assertEquals(640, cur.iso)
        assertEquals(12_000_000L, cur.exposureNs)
        val none = M.manualExposure(0, 0, 100, 3200, 50, 1_000_000, 30, 0, 0)
        assertEquals(M.DEFAULT_ISO, none.iso)
        assertEquals(16_666_000L, none.exposureNs)   // half a frame, in whole microseconds
        // a stale readout outside the range is clamped too
        assertEquals(3200, M.manualExposure(0, 0, 100, 3200, 50, 1_000_000, 30, 9000, 0).iso)
    }

    @Test fun whiteBalanceMapsAndFallsBack() {
        val avail = intArrayOf(0, 1, 2, 5)
        assertEquals(5, M.awbMode(5, avail))
        assertEquals(M.AWB_AUTO, M.awbMode(8, avail))   // shade unavailable
        assertEquals(M.AWB_AUTO, M.awbMode(0, avail))   // unspecified
        assertEquals(M.AWB_AUTO, M.awbMode(5, intArrayOf()))
        assertEquals(listOf(1, 2, 5), M.whiteBalances(avail))   // OFF skipped
    }

    @Test fun antibandingMapsAndFallsBack() {
        val avail = intArrayOf(M.AB_OFF, M.AB_50HZ, M.AB_AUTO)
        assertEquals(M.AB_50HZ, M.antibandingMode(3, avail))
        assertEquals(M.AB_AUTO, M.antibandingMode(4, avail))   // 60 Hz unavailable
        assertEquals(M.AB_AUTO, M.antibandingMode(0, avail))
        assertEquals(M.AB_OFF, M.antibandingMode(2, avail))
        assertNull(M.antibandingMode(1, intArrayOf(M.AB_60HZ)))
        assertEquals(listOf(1, 2, 3), M.antibandings(avail))
    }

    @Test fun evIsClamped() {
        assertEquals(6, M.clampEv(20, -6, 6))
        assertEquals(-6, M.clampEv(-20, -6, 6))
        assertEquals(0, M.clampEv(3, 0, 0))
        assertEquals(0, M.clampEv(3, 5, -5))
    }
}
