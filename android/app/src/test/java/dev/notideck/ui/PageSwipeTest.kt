// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PageSwipeTest {
    private val d = 2f
    private val w = 1000f // 18% = 180 px, 48dp = 96 px -> 180 wins

    private fun decide(dx: Float, dy: Float = 0f, vx: Float = 0f, vy: Float = 0f, page: Int = 1, count: Int = 3, width: Float = w) =
        PageSwipe.decide(dx, dy, vx, vy, width, d, page, count)

    @Test fun leftSwipeGoesNextRightGoesPrevious() {
        assertEquals(1, decide(-200f))
        assertEquals(-1, decide(200f))
    }

    @Test fun belowThresholdDoesNothing() {
        assertEquals(0, decide(-170f))
        assertEquals(0, decide(0f))
    }

    @Test fun narrowGridUsesMinimumDp() {
        assertEquals(0, decide(-90f, width = 300f))
        assertEquals(1, decide(-100f, width = 300f))
    }

    @Test fun mostlyVerticalIsIgnored() {
        assertEquals(0, decide(-200f, dy = 150f))
        assertEquals(1, decide(-200f, dy = 100f))
    }

    @Test fun flingTriggersWithShortTravel() {
        assertEquals(1, decide(-60f, vx = -2500f))
        assertEquals(-1, decide(60f, vx = 2500f))
        assertEquals(0, decide(-60f, vx = -1500f)) // 1500 px/s < 1000 dp/s at 2x
    }

    @Test fun flingNeedsDirectionBiasAndSameSign() {
        assertEquals(0, decide(-60f, vx = -2500f, vy = 2000f))
        assertEquals(0, decide(-60f, vx = 2500f))
        assertEquals(0, decide(-10f, vx = -9000f))
    }

    @Test fun noWrapAtEnds() {
        assertEquals(0, decide(-200f, page = 2, count = 3))
        assertEquals(0, decide(200f, page = 0, count = 3))
        assertEquals(1, decide(-200f, page = 0, count = 3))
        assertEquals(-1, decide(200f, page = 2, count = 3))
    }

    @Test fun singlePageOrFolderIsInert() {
        assertEquals(0, decide(-300f, count = 1))
        assertEquals(0, decide(-300f, count = 0))
        assertFalse(PageSwipe.available(true, 0))
        assertFalse(PageSwipe.available(true, 1))
        assertFalse(PageSwipe.available(false, 3))
        assertTrue(PageSwipe.available(true, 2))
    }

    @Test fun followOffsetIsDampedAndClamped() {
        assertEquals(-25f, PageSwipe.followOffset(-100f, 60f), 1e-5f)
        assertEquals(60f, PageSwipe.followOffset(900f, 60f), 0f)
    }

    @Test fun bandHitTest() {
        val band = 28f * d
        assertTrue(MediaBand.hit(190f, 200f, band))
        assertTrue(MediaBand.hit(200f, 200f, band))
        assertFalse(MediaBand.hit(100f, 200f, band))
        assertFalse(MediaBand.hit(143f, 200f, band))
        assertTrue(MediaBand.hit(144f, 200f, band))
        assertFalse(MediaBand.hit(10f, 0f, band))
    }

    @Test fun fractionMapsAcrossBar() {
        assertEquals(0.5f, MediaBand.fraction(60f, 10f, 100f), 1e-5f)
        assertEquals(0f, MediaBand.fraction(0f, 10f, 100f), 0f)
        assertEquals(1f, MediaBand.fraction(500f, 10f, 100f), 0f)
        assertEquals(0f, MediaBand.fraction(50f, 10f, 0f), 0f)
    }
}

class PressGateTest {
    private val slop = 8f
    private fun ev(avail: Boolean = true, t: Long, dx: Float = 0f, dy: Float = 0f, up: Boolean = false) =
        PressGate.evaluate(avail, t, dx, dy, slop, up)

    @Test fun fastTapFiresOnRelease() {
        assertEquals(PressGate.Result.Pending, ev(t = 30))
        assertEquals(PressGate.Result.Fire, ev(t = 40, up = true))
    }

    @Test fun holdFiresAtEightyMs() {
        assertEquals(PressGate.Result.Pending, ev(t = 79))
        assertEquals(PressGate.Result.Fire, ev(t = 80))
    }

    @Test fun horizontalMoveCancels() {
        assertEquals(PressGate.Result.Cancel, ev(t = 40, dx = 12f, dy = 2f))
        assertEquals(PressGate.Result.Cancel, ev(t = 40, dx = -12f, up = true))
    }

    @Test fun verticalJitterAndSmallMovesDoNotCancel() {
        assertEquals(PressGate.Result.Pending, ev(t = 40, dx = 3f, dy = 30f))
        assertEquals(PressGate.Result.Pending, ev(t = 40, dx = 12f, dy = 30f))
        assertEquals(PressGate.Result.Pending, ev(t = 40, dx = 7f))
        assertEquals(PressGate.Result.Fire, ev(t = 90, dy = 40f))
    }

    @Test fun unavailableIsImmediate() {
        assertEquals(PressGate.Result.Fire, ev(avail = false, t = 0))
        assertEquals(PressGate.Result.Fire, ev(avail = false, t = 10, dx = 50f))
    }
}
