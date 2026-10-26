// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import dev.notideck.proto.v1.Control
import org.junit.Assert.assertEquals
import org.junit.Test

class StreamFaceTest {
    @Test fun liveWithImage() {
        val f = streamFaceFor(Control(id = "a", state_index = 1), hasImage = true)
        assertEquals(StreamFace(live = true, greyscale = false, showPill = true, plain = false), f)
    }

    @Test fun offlineImageIsGreyNoPill() {
        val f = streamFaceFor(Control(id = "a", state_index = 0), hasImage = true)
        assertEquals(StreamFace(live = false, greyscale = true, showPill = false, plain = false), f)
    }

    @Test fun noImageIsPlain() {
        assertEquals(StreamFace(true, false, true, true), streamFaceFor(Control(id = "a", state_index = 1), hasImage = false))
        assertEquals(StreamFace(false, false, false, true), streamFaceFor(Control(id = "a"), hasImage = false))
    }
}
