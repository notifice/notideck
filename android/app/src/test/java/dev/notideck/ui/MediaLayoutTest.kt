// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class MediaLayoutTest {
    @Test fun layoutChoice() {
        assertEquals(MediaLayout.Mini, mediaLayoutFor(1, 1, 100f, 100f))
        assertEquals(MediaLayout.Mini, mediaLayoutFor(0, 0, 100f, 100f))
        assertEquals(MediaLayout.Wide, mediaLayoutFor(3, 1, 300f, 100f))
        assertEquals(MediaLayout.Wide, mediaLayoutFor(3, 2, 400f, 150f))
        assertEquals(MediaLayout.Tall, mediaLayoutFor(2, 2, 200f, 200f))
        assertEquals(MediaLayout.Tall, mediaLayoutFor(1, 2, 100f, 200f))
    }
}
