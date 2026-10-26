// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import android.view.Surface
import kotlinx.coroutines.flow.StateFlow

/**
 * Round 28: local camera preview for the CAMERA tile and the full-screen view.
 *
 * The preview shows exactly what the PC receives (same lens, crop, rotation, mirror, image settings): the GL
 * pass draws each camera frame to the encoder and, while a preview surface is attached, to that surface too.
 * When the PC is not using the camera, attaching a surface opens the camera for preview only (no encoder, no
 * media connection), with the last CameraStart settings or the defaults below; detaching the last surface
 * closes it again. If the PC starts the camera while a preview-only session runs, the controller switches to
 * the streaming pipeline and keeps the attached surface; if the PC stops, it falls back to preview only while
 * a surface is still attached.
 *
 * All methods are safe from the main thread and return quickly; work happens on the controller thread.
 */
interface CameraPreview {
    /**
     * Show the preview on [surface] ([width] x [height] px, the view's size; frames are scaled to fill it,
     * centre-cropped). [owner] identifies the caller (one surface per owner; a new attach replaces the
     * previous surface of the same owner). The most recent attach wins when several owners are attached:
     * only that surface receives frames (the tile hides its preview while the full-screen view is open).
     */
    fun attach(owner: Any, surface: Surface, width: Int, height: Int)

    /** Stop drawing to [owner]'s surface. Call before the surface is destroyed. No-op if not attached. */
    fun detach(owner: Any)

    val state: StateFlow<PreviewState>
}

data class PreviewState(
    /** Frames are being drawn to an attached surface. */
    val active: Boolean = false,
    /** The PC is receiving the camera right now (the rail shows the camera-in-use chip). */
    val streamingToPc: Boolean = false,
    /** Output frame size (what the PC gets), for the full-screen view's letterboxing. 0 = unknown. */
    val frameWidth: Int = 0,
    val frameHeight: Int = 0,
    /** CAMERA permission missing: the UI shows a "Allow camera" button that requests it. */
    val permissionNeeded: Boolean = false,
    /** Camera could not be opened (busy, error); human readable, empty when fine. */
    val error: String = ""
)

/** Preview-only defaults when the PC never sent a CameraStart in this app run. */
object PreviewDefaults {
    const val WIDTH = 1280
    const val HEIGHT = 720
    const val FPS = 30
}
