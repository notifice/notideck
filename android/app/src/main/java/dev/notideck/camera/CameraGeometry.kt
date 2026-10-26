// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

/** Orientation and tap-to-focus maths, pure so it can be unit tested. */
object CameraGeometry {
    /** Clockwise degrees to turn the sensor image so it is upright for a display rotated [displayDeg] (0/90/180/270). */
    fun uprightRotation(sensorOrientation: Int, displayDeg: Int, front: Boolean): Int {
        val r = if (front) sensorOrientation + displayDeg else sensorOrientation - displayDeg
        return ((r % 360) + 360) % 360
    }

    /**
     * Snaps an [OrientationEventListener] angle to a `Display.getRotation()` quadrant in degrees.
     * The listener reports the device's clockwise turn from natural while the display rotation counts the
     * other way (landscape-left reads 270 and is ROTATION_90), so display = (360 - angle). Switches away from
     * [current] only once within 30 degrees of the new quadrant's centre; -1 (unknown) keeps [current].
     */
    fun snapDeviceRotation(angle: Int, current: Int): Int {
        if (angle < 0 || angle > 359) return current
        val d = (360 - angle) % 360
        val centre = ((d + 45) / 90 * 90) % 360
        val diff = Math.abs(d - centre).let { minOf(it, 360 - it) }
        return if (centre != current && diff <= 30) centre else current
    }

    /**
     * The usual SurfaceTexture transform is a y-flip (column major: m0 > 0, m5 < 0, no off-diagonal terms).
     * If [m] instead turns the image (rotation or 180°), replace it with the plain y-flip so the sensor
     * buffer reaches the shader in sensor orientation, which [uprightRotation] assumes. Returns true if replaced.
     */
    fun stripTransformRotation(m: FloatArray): Boolean {
        val plain = m[0] > 0.5f && m[5] < -0.5f && Math.abs(m[1]) < 0.5f && Math.abs(m[4]) < 0.5f
        if (plain) return false
        m.fill(0f)
        m[0] = 1f; m[5] = -1f; m[10] = 1f; m[13] = 1f; m[15] = 1f
        return true
    }

    /**
     * Maps a point in the final (rotated, optionally mirrored) image, 0..1 with y down, to normalised
     * sensor coordinates (0..1, y down). [rotationDeg] is the clockwise turn applied to the sensor image.
     */
    fun outputToSensor(x: Float, y: Float, rotationDeg: Int, mirror: Boolean): Pair<Float, Float> {
        val ox = if (mirror) 1f - x else x
        return when (((rotationDeg % 360) + 360) % 360) {
            90 -> y to (1f - ox)
            180 -> (1f - ox) to (1f - y)
            270 -> (1f - y) to ox
            else -> ox to y
        }
    }

    /** Zoom crop (left, top, right, bottom) centred in an active array of [w] x [h]. */
    fun cropForZoom(w: Int, h: Int, ratio: Float): IntArray {
        val r = ratio.coerceAtLeast(1f)
        val cw = (w / r).toInt()
        val ch = (h / r).toInt()
        val l = (w - cw) / 2
        val t = (h - ch) / 2
        return intArrayOf(l, t, l + cw, t + ch)
    }

    /**
     * 2x2 matrix [m00, m01, m10, m11] (row major) mapping an output texture coordinate offset from the
     * centre (-0.5..0.5 on both axes) to a buffer texture coordinate offset from the centre, for a
     * camera buffer of [bufW] x [bufH] turned clockwise by [rotationDeg] and drawn into [outW] x [outH]
     * with "cover" scaling: the image is scaled uniformly to fill the frame and the overflow is cropped,
     * never stretched. For 90/270 the buffer's width and height trade places before fitting.
     */
    fun coverMatrix(rotationDeg: Int, mirror: Boolean, bufW: Int, bufH: Int, outW: Int, outH: Int): FloatArray {
        val rot = ((rotationDeg % 360) + 360) % 360
        val b = bufW.toFloat() / bufH            // buffer aspect (height = 1)
        val a = outW.toFloat() / outH            // output aspect (height = 1)
        // Image size once turned, in units where the buffer height is 1.
        val iw = if (rot == 90 || rot == 270) 1f else b
        val ih = if (rot == 90 || rot == 270) b else 1f
        val s = maxOf(a / iw, 1f / ih)           // cover: smallest uniform scale that fills the frame
        val sy = 1f / s
        val ux = a / s                           // output x offset (-0.5..0.5) in image units
        val x = if (mirror) -ux else ux
        val rad = Math.toRadians(rot.toDouble())
        val c = Math.round(Math.cos(rad)).toFloat()
        val sn = Math.round(Math.sin(rad)).toFloat()
        // q = Rot(theta) * (x * u, sy * v); then buffer normalised: (q.x / b, q.y)
        val m00 = c * x / b
        val m01 = -sn * sy / b
        val m10 = sn * x
        val m11 = c * sy
        return floatArrayOf(m00, m01, m10, m11)
    }

    /** [coverMatrix] expanded to a column-major 4x4 for the shader's uRot uniform. */
    fun coverMatrix4(rotationDeg: Int, mirror: Boolean, bufW: Int, bufH: Int, outW: Int, outH: Int, out: FloatArray = FloatArray(16)): FloatArray {
        val m = coverMatrix(rotationDeg, mirror, bufW, bufH, outW, outH)
        java.util.Arrays.fill(out, 0f)
        out[0] = m[0]; out[4] = m[1]
        out[1] = m[2]; out[5] = m[3]
        out[10] = 1f; out[15] = 1f
        return out
    }

    /**
     * Round 28 preview: the part of the [frameW] x [frameH] output frame a [viewW] x [viewH] view shows when
     * the frame is centre-cropped to fill it. Returns (sx, sy), the fraction of the frame's width and height
     * that stays visible; one of them is always 1.
     */
    fun previewScale(frameW: Int, frameH: Int, viewW: Int, viewH: Int): FloatArray {
        if (frameW <= 0 || frameH <= 0 || viewW <= 0 || viewH <= 0) return floatArrayOf(1f, 1f)
        val frame = frameW.toFloat() / frameH
        val view = viewW.toFloat() / viewH
        return if (view > frame) floatArrayOf(1f, frame / view) else floatArrayOf(view / frame, 1f)
    }

    /**
     * [coverMatrix4] for a preview view: exactly the output frame (same rotation, mirror and cover crop as
     * the encoder gets), then centre-cropped once more to the view's aspect.
     */
    fun previewMatrix4(
        rotationDeg: Int, mirror: Boolean, frameW: Int, frameH: Int, viewW: Int, viewH: Int, out: FloatArray = FloatArray(16)
    ): FloatArray {
        val m = coverMatrix(rotationDeg, mirror, frameW, frameH, frameW, frameH)
        val s = previewScale(frameW, frameH, viewW, viewH)
        java.util.Arrays.fill(out, 0f)
        out[0] = m[0] * s[0]; out[4] = m[1] * s[1]
        out[1] = m[2] * s[0]; out[5] = m[3] * s[1]
        out[10] = 1f; out[15] = 1f
        return out
    }
}
