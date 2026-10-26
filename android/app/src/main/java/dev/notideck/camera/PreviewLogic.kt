// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import android.content.Context
import android.view.Surface
import androidx.core.content.edit
import dev.notideck.proto.v1.CameraImage
import dev.notideck.proto.v1.CameraStart
import dev.notideck.proto.v1.Lens

/** One attached preview surface. Identity matters: a re-attach (new size) is a new target. */
class PreviewTarget(val surface: Surface, val width: Int, val height: Int)

/**
 * Round 28 preview owners: one entry per owner, most recent attach on top. Only the top entry is drawn;
 * when it detaches, the next still-attached owner becomes active again. Owners compare by identity.
 * Not thread-safe; the controller guards it.
 */
class PreviewOwners<T : Any> {
    private val stack = ArrayList<Pair<Any, T>>()

    /** The value that should be drawn now, or null when nobody is attached. */
    val top: T? get() = stack.lastOrNull()?.second
    val isEmpty: Boolean get() = stack.isEmpty()
    val size: Int get() = stack.size

    /** Replaces [owner]'s previous value (if any) and moves the owner to the top. Returns the new top. */
    fun attach(owner: Any, value: T): T {
        stack.removeAll { it.first === owner }
        stack.add(owner to value)
        return value
    }

    /** Removes [owner]; no-op when it is not attached. Returns the new top. */
    fun detach(owner: Any): T? {
        stack.removeAll { it.first === owner }
        return top
    }
}

/** What the camera should be doing, from the controller's point of view. */
enum class PreviewMode { OFF, PREVIEW_ONLY, STREAMING }

object PreviewPolicy {
    /**
     * [streaming]: the PC's pipeline runs (it always feeds an attached surface too). Otherwise a preview-only
     * session runs while a surface is attached, the app is in the foreground and the camera can be opened.
     */
    fun decide(streaming: Boolean, attached: Boolean, foreground: Boolean, permitted: Boolean, hasCamera: Boolean): PreviewMode =
        when {
            streaming -> PreviewMode.STREAMING
            attached && foreground && permitted && hasCamera -> PreviewMode.PREVIEW_ONLY
            else -> PreviewMode.OFF
        }

    /** The UI must offer "Allow camera": a surface waits for a camera only the permission keeps closed. */
    fun permissionNeeded(streaming: Boolean, attached: Boolean, permitted: Boolean, hasCamera: Boolean): Boolean =
        !streaming && attached && hasCamera && !permitted
}

/**
 * Settings for a preview-only session. [image] and [rotationDeg] only come from a CameraStart of this run
 * (and later CameraControl image changes); the persisted copy keeps lens, size, fps and mirror.
 */
data class PreviewSettings(
    val lens: Lens,
    val width: Int,
    val height: Int,
    val fps: Int,
    val mirror: Boolean,
    val rotationDeg: Int = 0,
    val image: CameraImage? = null
) {
    companion object {
        val DEFAULT = PreviewSettings(Lens.LENS_FRONT, PreviewDefaults.WIDTH, PreviewDefaults.HEIGHT, PreviewDefaults.FPS, mirror = false)

        fun from(m: CameraStart) = PreviewSettings(
            lens = m.lens, width = m.width, height = m.height, fps = m.fps, mirror = m.mirror,
            rotationDeg = m.rotation_deg, image = m.image
        )

        /** Last CameraStart of this run, else the persisted one, else [DEFAULT]. Invalid sizes fall through. */
        fun resolve(thisRun: PreviewSettings?, persisted: PreviewSettings?): PreviewSettings {
            val s = listOfNotNull(thisRun, persisted).firstOrNull { it.width > 0 && it.height > 0 } ?: return DEFAULT
            return s.copy(
                lens = if (s.lens == Lens.LENS_UNSPECIFIED) DEFAULT.lens else s.lens,
                fps = if (s.fps > 0) s.fps else DEFAULT.fps
            )
        }
    }
}

/** SharedPreferences copy of the last CameraStart's lens, size, fps and mirror. */
class PreviewSettingsStore(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences("camera_preview", Context.MODE_PRIVATE)

    fun save(s: PreviewSettings) {
        prefs.edit {
            putInt("lens", s.lens.value).putInt("width", s.width).putInt("height", s.height)
                .putInt("fps", s.fps).putBoolean("mirror", s.mirror)
        }
    }

    fun load(): PreviewSettings? {
        if (!prefs.contains("width")) return null
        return PreviewSettings(
            lens = Lens.fromValue(prefs.getInt("lens", Lens.LENS_FRONT.value)) ?: Lens.LENS_FRONT,
            width = prefs.getInt("width", 0), height = prefs.getInt("height", 0),
            fps = prefs.getInt("fps", 0), mirror = prefs.getBoolean("mirror", false)
        )
    }
}
