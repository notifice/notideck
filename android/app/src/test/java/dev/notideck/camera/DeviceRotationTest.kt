// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import org.junit.Assert.assertEquals
import org.junit.Test

class DeviceRotationTest {
    private fun snap(a: Int, cur: Int) = CameraGeometry.snapDeviceRotation(a, cur)

    @Test fun quadrantCentres() {
        // listener angle is the opposite direction of display rotation: 270 -> ROTATION_90
        assertEquals(0, snap(0, 90))
        assertEquals(90, snap(270, 0))
        assertEquals(180, snap(180, 0))
        assertEquals(270, snap(90, 0))
    }

    @Test fun unknownKeepsCurrent() {
        assertEquals(90, snap(-1, 90))
        assertEquals(180, snap(-1, 180))
    }

    @Test fun hysteresisHoldsNearBoundary() {
        // display angle 45 is on the 0/90 boundary: stay on whichever we had
        assertEquals(0, snap(315, 0))
        assertEquals(90, snap(315, 90))
        // 31 degrees from the new centre is still held, 30 switches
        assertEquals(0, snap(360 - 59, 0))
        assertEquals(90, snap(360 - 60, 0))
    }

    @Test fun wrapAroundAndFarSwitch() {
        assertEquals(0, snap(350, 270))   // display 10
        assertEquals(0, snap(10, 90))     // display 350
        assertEquals(270, snap(80, 0))    // display 280
        assertEquals(90, snap(270, 270))
    }
}
