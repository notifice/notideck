// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import dev.notideck.ui.FaderGeometry.Zone
import org.junit.Test
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue

class FaderGeometryTest {
    @Test fun thumbStaysInsideTrackAtExtremes() {
        val len = 300f; val thumb = 24f
        assertEquals(12f, FaderGeometry.thumbCenter(0f, len, thumb), 0f)
        assertEquals(288f, FaderGeometry.thumbCenter(1f, len, thumb), 0f)
        assertEquals(12f, FaderGeometry.thumbCenter(-3f, len, thumb), 0f)
        assertEquals(288f, FaderGeometry.thumbCenter(9f, len, thumb), 0f)
    }

    @Test fun valueAtInvertsThumbCenter() {
        for (v in listOf(0f, 0.25f, 0.5f, 1f)) {
            val pos = FaderGeometry.thumbCenter(v, 200f, 24f)
            assertEquals(v, FaderGeometry.valueAt(pos, 200f, 24f), 1e-5f)
        }
        assertEquals(0f, FaderGeometry.valueAt(-50f, 200f, 24f), 0f)
        assertEquals(1f, FaderGeometry.valueAt(999f, 200f, 24f), 0f)
        assertEquals(0f, FaderGeometry.valueAt(10f, 20f, 24f), 0f)
    }

    @Test fun ledCountClamped() {
        assertEquals(FaderGeometry.MIN_LEDS, FaderGeometry.ledCount(10f, 5f, 2f))
        assertEquals(FaderGeometry.MAX_LEDS, FaderGeometry.ledCount(5000f, 5f, 2f))
        assertEquals(20, FaderGeometry.ledCount(140f, 5f, 2f))
    }

    @Test fun ledLit() {
        assertEquals(0, FaderGeometry.ledLit(0f, 20))
        assertEquals(1, FaderGeometry.ledLit(0.02f, 20))
        assertEquals(20, FaderGeometry.ledLit(1f, 20))
        assertEquals(10, FaderGeometry.ledLit(0.5f, 20))
        assertTrue(FaderGeometry.ledLit(0.3f, 20) < FaderGeometry.ledLit(0.6f, 20))
    }

    @Test fun zones() {
        assertEquals(Zone.GREEN, FaderGeometry.zone(0, 20))
        assertEquals(Zone.AMBER, FaderGeometry.zone(15, 20))
        assertEquals(Zone.RED, FaderGeometry.zone(19, 20))
    }
}
