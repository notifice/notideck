// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

/**
 * Round 28: the preview surface currently on top, shared between the controller (any thread) and whichever
 * [GlFrameRenderer] is alive. It outlives pipelines, so a preview-only -> streaming switch keeps the surface.
 *
 * The GL thread polls [target] on every frame. [set] additionally makes a live renderer apply the change at
 * once and waits briefly for it, so a surface that is about to be destroyed is no longer drawn to.
 */
class PreviewOutput(private val listener: Listener) {
    interface Listener {
        /** The renderer drew its first frame to [target] (true) or stopped drawing to it (false). GL thread. */
        fun onPreviewActive(target: PreviewTarget, active: Boolean)
        /** Drawing to [target] failed; the renderer dropped it and keeps encoding. GL thread. */
        fun onPreviewError(target: PreviewTarget, message: String)
    }

    @Volatile var target: PreviewTarget? = null
        private set

    private val lock = Any()
    private var renderer: GlFrameRenderer? = null

    fun set(t: PreviewTarget?) {
        val r = synchronized(lock) { target = t; renderer }
        r?.syncPreview()
    }

    internal fun bind(r: GlFrameRenderer) = synchronized(lock) { renderer = r }
    internal fun unbind(r: GlFrameRenderer) = synchronized(lock) { if (renderer === r) renderer = null }

    internal fun active(t: PreviewTarget, on: Boolean) = listener.onPreviewActive(t, on)
    internal fun error(t: PreviewTarget, message: String) = listener.onPreviewError(t, message)
}
