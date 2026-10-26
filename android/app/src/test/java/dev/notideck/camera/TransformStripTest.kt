// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TransformStripTest {
    private val flip = floatArrayOf(1f, 0f, 0f, 0f, 0f, -1f, 0f, 0f, 0f, 0f, 1f, 0f, 0f, 1f, 0f, 1f)

    @Test fun plainFlipIsKept() {
        val m = floatArrayOf(0.98f, 0f, 0f, 0f, 0f, -0.98f, 0f, 0f, 0f, 0f, 1f, 0f, 0.01f, 0.99f, 0f, 1f)
        val before = m.copyOf()
        assertFalse(CameraGeometry.stripTransformRotation(m))
        assertArrayEquals(before, m, 0f)
    }

    @Test fun xperiaRotationIsStripped() {
        // Logged on the Xperia XZ (Android 8): 90° turn folded into the transform.
        val m = floatArrayOf(0f, -1f, 0f, 0f, 1f, 0f, 0f, 0f, 0f, 0f, 1f, 0f, 0f, 1f, 0f, 1f)
        assertTrue(CameraGeometry.stripTransformRotation(m))
        assertArrayEquals(flip, m, 0f)
    }

    @Test fun halfTurnIsStripped() {
        val m = floatArrayOf(-1f, 0f, 0f, 0f, 0f, 1f, 0f, 0f, 0f, 0f, 1f, 0f, 1f, 0f, 0f, 1f)
        assertTrue(CameraGeometry.stripTransformRotation(m))
        assertArrayEquals(flip, m, 0f)
    }
}
