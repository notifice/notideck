// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class LevelSmootherTest {
    @Test
    fun instantAttack() {
        val s = LevelSmoother()
        assertEquals(0.8f, s.step(0.8f, 16), 0f)
        assertEquals(0.9f, s.step(0.9f, 16), 0f)
    }

    @Test
    fun exponentialRelease() {
        val s = LevelSmoother()
        s.step(1f, 0)
        val a = s.step(0f, 100)
        assertEquals(0.368f, a, 0.01f)
        val b = s.step(0f, 100)
        assertTrue(b < a && b > 0f)
    }

    @Test
    fun nearZeroWithin300msAndSnapsToZero() {
        val s = LevelSmoother()
        s.step(1f, 0)
        var t = 0
        var v = 1f
        while (v > 0f && t < 2000) { v = s.step(0f, 16); t += 16 }
        assertEquals(0f, v, 0f)
        assertTrue(t < 800)
        val s2 = LevelSmoother()
        s2.step(1f, 0)
        assertTrue(s2.step(0f, 300) < 0.06f)
    }

    @Test
    fun holdsWhenTargetSteadyAndClamps() {
        val s = LevelSmoother()
        s.step(0.5f, 0)
        assertEquals(0.5f, s.step(0.5f, 500), 0f)
        assertEquals(1f, s.step(3f, 16), 0f)
        assertEquals(0f, LevelSmoother().step(Float.NaN, 16), 0f)
    }
}
