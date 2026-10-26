// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import dev.notideck.camera.PreviewState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CameraTileTest {
    @Test fun idleShowsHintAndNoSurface() {
        val f = cameraTileFace(previewOn = false, fullScreenOpen = false, st = PreviewState())
        assertFalse(f.surface)
        assertTrue(f.showHint)
        assertFalse(f.pcPill)
    }

    @Test fun previewOnAttachesSurface() {
        val f = cameraTileFace(true, false, PreviewState(active = true))
        assertTrue(f.surface)
        assertFalse(f.showHint)
        assertFalse(f.needsPermission)
        assertEquals("", f.error)
    }

    @Test fun fullScreenOpenDropsInlinePreview() {
        val f = cameraTileFace(true, true, PreviewState(active = true))
        assertFalse(f.surface)
        assertTrue(f.showHint)
    }

    @Test fun permissionBeatsError() {
        val f = cameraTileFace(true, false, PreviewState(permissionNeeded = true, error = "busy"))
        assertTrue(f.needsPermission)
        assertEquals("", f.error)
        assertTrue(f.surface)
    }

    @Test fun errorShownOnlyWhileOn() {
        assertEquals("busy", cameraTileFace(true, false, PreviewState(error = "busy")).error)
        assertEquals("", cameraTileFace(false, false, PreviewState(error = "busy")).error)
        assertFalse(cameraTileFace(false, false, PreviewState(permissionNeeded = true)).needsPermission)
    }

    @Test fun pcPillFollowsStreaming() {
        assertTrue(cameraTileFace(false, false, PreviewState(streamingToPc = true)).pcPill)
        assertTrue(cameraTileFace(true, true, PreviewState(streamingToPc = true)).pcPill)
    }

    @Test fun letterboxWideFrameInShortContainer() {
        // 16:9 frame in 800x400 (2:1): height limits, bars left and right
        val f = fitLetterbox(800f, 400f, 1280, 720)
        assertEquals(400f * 16f / 9f, f.width, 0.01f)
        assertEquals(400f, f.height, 0.01f)
    }

    @Test fun letterboxWidthLimited() {
        val f = fitLetterbox(600f, 600f, 1920, 1080)
        assertEquals(600f, f.width, 0.01f)
        assertEquals(337.5f, f.height, 0.01f)
    }

    @Test fun letterboxPortraitFrame() {
        val f = fitLetterbox(1000f, 500f, 720, 1280)
        assertEquals(500f * 720f / 1280f, f.width, 0.01f)
        assertEquals(500f, f.height, 0.01f)
    }

    @Test fun letterboxUnknownFrameFallsBackTo16by9() {
        val f = fitLetterbox(1600f, 1600f, 0, 0)
        assertEquals(1600f, f.width, 0.01f)
        assertEquals(900f, f.height, 0.01f)
    }

    @Test fun letterboxEmptyContainer() {
        assertEquals(Fit(0f, 0f), fitLetterbox(0f, 100f, 16, 9))
    }
}
