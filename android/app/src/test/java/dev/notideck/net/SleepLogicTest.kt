// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.DeckSleep
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SleepLogicTest {
    private var t = 1_000L
    private val timer = SleepTimer { t }
    private val cfg = SleepConfig(enabled = true, idleS = 60, wakeOnCameraMic = true, showClock = true)

    @Test fun idleIsClamped() {
        assertEquals(120, SleepLogic.clampIdleS(0))
        assertEquals(15, SleepLogic.clampIdleS(1))
        assertEquals(15, SleepLogic.clampIdleS(15))
        assertEquals(300, SleepLogic.clampIdleS(300))
        assertEquals(3600, SleepLogic.clampIdleS(99_999))
    }

    @Test fun configFromProtoClamps() {
        val c = SleepConfig.from(DeckSleep(enabled = true, idle_s = 5, wake_on_camera_mic = true, show_clock = false))
        assertEquals(SleepConfig(true, 15, true, false), c)
    }

    @Test fun sleepsAfterIdle() {
        assertEquals(60_000L, timer.remainingMs(cfg, false))
        t += 59_999
        assertFalse(timer.shouldSleep(cfg, false))
        t += 1
        assertTrue(timer.shouldSleep(cfg, false))
    }

    @Test fun touchRestartsTimer() {
        t += 50_000
        timer.touch()
        assertEquals(60_000L, timer.remainingMs(cfg, false))
        t += 60_000
        assertTrue(timer.shouldSleep(cfg, false))
    }

    @Test fun neverSleepsWhenInhibitedDisabledOrUnknown() {
        t += 1_000_000
        assertNull(timer.remainingMs(cfg, true))
        assertNull(timer.remainingMs(cfg.copy(enabled = false), false))
        assertNull(timer.remainingMs(null, false))
        assertFalse(timer.shouldSleep(cfg, true))
    }

    @Test fun clockStaysInsideMargin() {
        assertEquals(Pair(20, 20), SleepLogic.clockOffset(0f, 0f, 1000, 500, 100, 40, 20))
        assertEquals(Pair(880, 440), SleepLogic.clockOffset(1f, 1f, 1000, 500, 100, 40, 20))
        assertEquals(Pair(20, 20), SleepLogic.clockOffset(0.9f, 0.9f, 100, 40, 100, 40, 20))
    }
}
