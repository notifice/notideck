// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import dev.notideck.proto.v1.CameraImage
import dev.notideck.proto.v1.CameraStart
import dev.notideck.proto.v1.Lens
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PreviewOwnersTest {
    private val tile = Any()
    private val full = Any()

    @Test fun mostRecentAttachWins() {
        val o = PreviewOwners<String>()
        assertNull(o.top)
        assertTrue(o.isEmpty)
        o.attach(tile, "tile")
        o.attach(full, "full")
        assertEquals("full", o.top)
        assertEquals(2, o.size)
    }

    @Test fun detachingTheTopRestoresThePreviousOwner() {
        val o = PreviewOwners<String>()
        o.attach(tile, "tile")
        o.attach(full, "full")
        assertEquals("tile", o.detach(full))
        assertEquals("tile", o.top)
        assertNull(o.detach(tile))
        assertTrue(o.isEmpty)
    }

    @Test fun detachingAnOlderOwnerKeepsTheTop() {
        val o = PreviewOwners<String>()
        o.attach(tile, "tile")
        o.attach(full, "full")
        assertEquals("full", o.detach(tile))
        assertEquals(1, o.size)
    }

    @Test fun reattachReplacesTheOwnersSurfaceAndMovesItUp() {
        val o = PreviewOwners<String>()
        o.attach(tile, "tile-a")
        o.attach(full, "full")
        o.attach(tile, "tile-b")
        assertEquals("tile-b", o.top)
        assertEquals(2, o.size)
        assertEquals("full", o.detach(tile))
    }

    @Test fun detachUnknownOwnerIsANoOp() {
        val o = PreviewOwners<String>()
        o.attach(tile, "tile")
        assertEquals("tile", o.detach(Any()))
        assertEquals(1, o.size)
    }

    @Test fun ownersCompareByIdentity() {
        val o = PreviewOwners<String>()
        o.attach("owner", "a")
        o.attach(String("owner".toCharArray()), "b")   // equal, not identical
        assertEquals(2, o.size)
    }
}

class PreviewPolicyTest {
    private fun decide(streaming: Boolean = false, attached: Boolean = true, fg: Boolean = true, perm: Boolean = true, cam: Boolean = true) =
        PreviewPolicy.decide(streaming, attached, fg, perm, cam)

    @Test fun streamingWinsOverEverything() {
        assertEquals(PreviewMode.STREAMING, decide(streaming = true))
        assertEquals(PreviewMode.STREAMING, decide(streaming = true, attached = false, fg = false))
    }

    @Test fun previewOnlyNeedsSurfaceForegroundPermissionAndCamera() {
        assertEquals(PreviewMode.PREVIEW_ONLY, decide())
        assertEquals(PreviewMode.OFF, decide(attached = false))
        assertEquals(PreviewMode.OFF, decide(fg = false))
        assertEquals(PreviewMode.OFF, decide(perm = false))
        assertEquals(PreviewMode.OFF, decide(cam = false))
    }

    @Test fun permissionNeededOnlyForAWaitingSurface() {
        assertTrue(PreviewPolicy.permissionNeeded(streaming = false, attached = true, permitted = false, hasCamera = true))
        assertFalse(PreviewPolicy.permissionNeeded(streaming = false, attached = false, permitted = false, hasCamera = true))
        assertFalse(PreviewPolicy.permissionNeeded(streaming = false, attached = true, permitted = true, hasCamera = true))
        assertFalse(PreviewPolicy.permissionNeeded(streaming = true, attached = true, permitted = false, hasCamera = true))
        assertFalse(PreviewPolicy.permissionNeeded(streaming = false, attached = true, permitted = false, hasCamera = false))
    }
}

class PreviewSettingsTest {
    private val start = CameraStart(width = 1920, height = 1080, fps = 60, lens = Lens.LENS_BACK, mirror = true,
        rotation_deg = 90, image = CameraImage(ae_lock = true))

    @Test fun defaultsAreFrontLens720p30NoMirror() {
        val s = PreviewSettings.resolve(null, null)
        assertEquals(Lens.LENS_FRONT, s.lens)
        assertEquals(PreviewDefaults.WIDTH, s.width)
        assertEquals(PreviewDefaults.HEIGHT, s.height)
        assertEquals(PreviewDefaults.FPS, s.fps)
        assertFalse(s.mirror)
        assertNull(s.image)
    }

    @Test fun thisRunWinsOverPersisted() {
        val persisted = PreviewSettings(Lens.LENS_FRONT, 640, 360, 15, mirror = false)
        val s = PreviewSettings.resolve(PreviewSettings.from(start), persisted)
        assertEquals(PreviewSettings(Lens.LENS_BACK, 1920, 1080, 60, true, 90, CameraImage(ae_lock = true)), s)
    }

    @Test fun persistedWhenNothingThisRun() {
        val persisted = PreviewSettings(Lens.LENS_BACK, 640, 360, 15, mirror = true)
        assertEquals(persisted, PreviewSettings.resolve(null, persisted))
    }

    @Test fun invalidEntriesFallThrough() {
        val broken = PreviewSettings(Lens.LENS_BACK, 0, 0, 30, mirror = true)
        val persisted = PreviewSettings(Lens.LENS_BACK, 640, 360, 15, mirror = false)
        assertEquals(persisted, PreviewSettings.resolve(broken, persisted))
        assertEquals(PreviewSettings.DEFAULT, PreviewSettings.resolve(broken, null))
    }

    @Test fun unspecifiedLensAndFpsTakeDefaults() {
        val s = PreviewSettings.resolve(PreviewSettings(Lens.LENS_UNSPECIFIED, 1280, 720, 0, mirror = false), null)
        assertEquals(Lens.LENS_FRONT, s.lens)
        assertEquals(PreviewDefaults.FPS, s.fps)
    }
}

class PreviewGeometryTest {
    private fun close(a: FloatArray, b: FloatArray) = assertArrayEquals(a, b, 1e-5f)

    @Test fun sameAspectShowsTheWholeFrame() {
        close(floatArrayOf(1f, 1f), CameraGeometry.previewScale(1280, 720, 640, 360))
    }

    @Test fun squareViewCropsTheSidesOfA16by9Frame() {
        close(floatArrayOf(9f / 16f, 1f), CameraGeometry.previewScale(1280, 720, 500, 500))
    }

    @Test fun wideViewCropsTopAndBottom() {
        // 32:9 view over a 16:9 frame: half the height stays visible.
        close(floatArrayOf(1f, 0.5f), CameraGeometry.previewScale(1280, 720, 3200, 900))
    }

    @Test fun unknownSizesDoNotCrop() {
        close(floatArrayOf(1f, 1f), CameraGeometry.previewScale(1280, 720, 0, 0))
    }

    @Test fun previewMatrixEqualsEncoderMatrixAtSameAspect() {
        for (rot in listOf(0, 90, 180, 270)) for (mirror in listOf(false, true)) {
            close(
                CameraGeometry.coverMatrix4(rot, mirror, 1280, 720, 1280, 720),
                CameraGeometry.previewMatrix4(rot, mirror, 1280, 720, 1920, 1080)
            )
        }
    }

    @Test fun previewMatrixScalesTheEncoderColumns() {
        val enc = CameraGeometry.coverMatrix4(90, true, 1280, 720, 1280, 720)
        val pv = CameraGeometry.previewMatrix4(90, true, 1280, 720, 720, 720)
        val sx = 720f / 1280f   // square view of a 16:9 frame
        assertEquals(enc[0] * sx, pv[0], 1e-5f)
        assertEquals(enc[1] * sx, pv[1], 1e-5f)
        assertEquals(enc[4], pv[4], 1e-5f)
        assertEquals(enc[5], pv[5], 1e-5f)
        assertEquals(1f, pv[15], 0f)
    }
}
