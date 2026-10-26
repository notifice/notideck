// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import kotlin.math.roundToInt

/**
 * Pure mapping/clamping for the webcam image settings (SPEC round 15). Plain Ints and Floats only,
 * so it runs in JVM unit tests. The numeric constants are the Camera2 ones, spelled out here.
 */
object CameraImageMapper {
    // CaptureRequest.CONTROL_AF_MODE_*
    const val AF_OFF = 0
    const val AF_AUTO = 1
    const val AF_CONTINUOUS_VIDEO = 3
    // CaptureRequest.CONTROL_AWB_MODE_AUTO (the other WhiteBalance values match Camera2 one to one)
    const val AWB_AUTO = 1
    // CaptureRequest.CONTROL_AE_ANTIBANDING_MODE_*
    const val AB_OFF = 0
    const val AB_50HZ = 1
    const val AB_60HZ = 2
    const val AB_AUTO = 3

    // proto FocusMode / ExposureMode values
    const val FOCUS_LOCKED = 2
    const val FOCUS_MANUAL = 3
    const val EXPOSURE_MANUAL = 2

    const val DEFAULT_ISO = 400

    /** Focus mode after falling back to what the lens supports. Returns the proto value 1 (continuous), 2 or 3. */
    fun effectiveFocus(focusMode: Int, focusManual: Boolean, focusLock: Boolean): Int = when {
        focusMode == FOCUS_MANUAL && focusManual -> FOCUS_MANUAL
        focusMode == FOCUS_LOCKED && focusLock -> FOCUS_LOCKED
        else -> 1
    }

    /** Camera2 AF mode for an effective focus mode, or null when the lens has no suitable mode (leave it unset). */
    fun afMode(effectiveFocus: Int, continuousAvailable: Boolean): Int? = when (effectiveFocus) {
        FOCUS_MANUAL -> AF_OFF
        FOCUS_LOCKED -> AF_AUTO
        else -> if (continuousAvailable) AF_CONTINUOUS_VIDEO else null
    }

    /** permille (0 = infinity, 1000 = closest) -> LENS_FOCUS_DISTANCE in diopters. */
    fun focusDiopters(permille: Int, minFocusDistance: Float): Float =
        permille.coerceIn(0, 1000) / 1000f * minFocusDistance

    /** LENS_FOCUS_DISTANCE -> permille, clamped; 0 when the lens has no focus range. */
    fun focusPermille(diopters: Float, minFocusDistance: Float): Int =
        if (minFocusDistance <= 0f || diopters.isNaN()) 0
        else (diopters / minFocusDistance * 1000f).roundToInt().coerceIn(0, 1000)

    fun clampEv(index: Int, evMin: Int, evMax: Int): Int =
        if (evMin > evMax) 0 else index.coerceIn(evMin, evMax)

    class ManualExposure(val iso: Int, val exposureNs: Long, val frameDurationNs: Long)

    /**
     * AE-off values. [iso] / [exposureUs] of 0 mean "keep the current value": [currentIso] / [currentExposureUs]
     * (the latest readout, 0 if none yet), else a safe middle. The exposure is capped at the frame duration.
     */
    fun manualExposure(
        iso: Int, exposureUs: Int, isoMin: Int, isoMax: Int, exposureUsMin: Int, exposureUsMax: Int,
        fps: Int, currentIso: Int, currentExposureUs: Int
    ): ManualExposure {
        val frameNs = 1_000_000_000L / fps.coerceAtLeast(1)
        val isoWanted = when {
            iso > 0 -> iso
            currentIso > 0 -> currentIso
            else -> DEFAULT_ISO
        }
        val usWanted = when {
            exposureUs > 0 -> exposureUs.toLong()
            currentExposureUs > 0 -> currentExposureUs.toLong()
            else -> frameNs / 2000
        }
        val minNs = exposureUsMin.coerceAtLeast(1) * 1000L
        val maxNs = maxOf(minNs, exposureUsMax * 1000L)
        val ns = minOf(usWanted * 1000L, maxNs, frameNs).coerceAtLeast(minOf(minNs, frameNs))
        val isoOut = if (isoMax < isoMin || isoMax <= 0) isoWanted else isoWanted.coerceIn(isoMin, isoMax)
        return ManualExposure(isoOut, ns, frameNs)
    }

    /** Camera2 AWB mode for a WhiteBalance value; AUTO when unspecified or unavailable. */
    fun awbMode(wire: Int, available: IntArray): Int =
        if (wire in 2..8 && available.contains(wire)) wire else AWB_AUTO

    /** WhiteBalance values for the Camera2 modes a lens offers (OFF and unknown modes skipped). */
    fun whiteBalances(available: IntArray): List<Int> = available.filter { it in 1..8 }.distinct().sorted()

    /** Camera2 antibanding mode for an Antibanding value, or null when the lens offers none to set. */
    fun antibandingMode(wire: Int, available: IntArray): Int? {
        val want = when (wire) {
            2 -> AB_OFF
            3 -> AB_50HZ
            4 -> AB_60HZ
            else -> AB_AUTO
        }
        return when {
            available.contains(want) -> want
            available.contains(AB_AUTO) -> AB_AUTO
            else -> null
        }
    }

    /** Antibanding proto values for the Camera2 modes a lens offers. */
    fun antibandings(available: IntArray): List<Int> = available.toList().mapNotNull {
        when (it) {
            AB_AUTO -> 1
            AB_OFF -> 2
            AB_50HZ -> 3
            AB_60HZ -> 4
            else -> null
        }
    }.distinct().sorted()
}
