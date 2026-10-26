// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import android.content.Context
import android.hardware.display.DisplayManager
import android.view.Display
import android.view.OrientationEventListener
import android.view.Surface
import android.view.WindowManager

/**
 * Physical device rotation from the accelerometer, in `Display.getRotation()` degrees (0/90/180/270),
 * so it works while the activity is backgrounded or the screen is off. Callbacks run on the main looper.
 */
class DeviceOrientation(private val context: Context, private val onChange: (Int) -> Unit) {
    @Volatile var degrees: Int = initial()
        private set

    private val listener = object : OrientationEventListener(context) {
        override fun onOrientationChanged(orientation: Int) {
            val next = CameraGeometry.snapDeviceRotation(orientation, degrees)
            if (next != degrees) { degrees = next; onChange(next) }
        }
    }

    fun enable() { if (listener.canDetectOrientation()) listener.enable() }
    fun disable() { listener.disable() }

    /** Display rotation while the screen is on (activity resumed or not), else landscape (app is landscape-first). */
    private fun initial(): Int = try {
        @Suppress("DEPRECATION")
        val d = (context.getSystemService(Context.WINDOW_SERVICE) as WindowManager).defaultDisplay
        val on = (context.getSystemService(Context.DISPLAY_SERVICE) as DisplayManager)
            .getDisplay(Display.DEFAULT_DISPLAY)?.state == Display.STATE_ON
        if (!on) 90 else when (d.rotation) {
            Surface.ROTATION_90 -> 90; Surface.ROTATION_180 -> 180; Surface.ROTATION_270 -> 270; else -> 0
        }
    } catch (_: Throwable) { 90 }
}
