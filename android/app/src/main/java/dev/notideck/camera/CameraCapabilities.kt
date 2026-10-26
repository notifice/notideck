// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import android.content.Context
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.util.Log
import dev.notideck.proto.v1.CameraMode
import dev.notideck.proto.v1.Antibanding
import dev.notideck.proto.v1.Lens
import dev.notideck.proto.v1.LensCaps
import dev.notideck.proto.v1.WhiteBalance

/** Camera2 characteristics we need for one lens. */
class LensInfo(
    val lens: Lens,
    val cameraId: String,
    val chars: CameraCharacteristics,
    val hasFlash: Boolean
)

/** Queries Camera2 and the H.264 encoder once and keeps the result. */
class CameraCapabilities(private val context: Context) {
    companion object {
        const val TAG = "CameraCaps"
        fun hasCameraHardware(context: Context): Boolean =
            context.packageManager.hasSystemFeature(android.content.pm.PackageManager.FEATURE_CAMERA_ANY)

        fun lensOf(facing: Int?): Lens? = when (facing) {
            CameraCharacteristics.LENS_FACING_BACK -> Lens.LENS_BACK
            CameraCharacteristics.LENS_FACING_FRONT -> Lens.LENS_FRONT
            CameraCharacteristics.LENS_FACING_EXTERNAL -> Lens.LENS_EXTERNAL
            else -> null
        }

        /** Finds an H.264 encoder (hardware preferred) that can take a Surface. */
        fun findAvcEncoder(): MediaCodecInfo? {
            val all = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
                .filter { it.isEncoder && it.supportedTypes.any { t -> t.equals(MediaFormat.MIMETYPE_VIDEO_AVC, true) } }
            return all.firstOrNull { !it.name.startsWith("OMX.google.") && !it.name.startsWith("c2.android.") }
                ?: all.firstOrNull()
        }
    }

    val lenses: Map<Lens, LensInfo> by lazy { loadLenses() }
    val hasTorch: Boolean get() = lenses.values.any { it.hasFlash }
    val hasCamera: Boolean get() = lenses.isNotEmpty()

    val modes: List<CameraMode> by lazy {
        try { loadModes() } catch (e: Throwable) { Log.w(TAG, "mode query failed: $e"); emptyList() }
    }

    /** One [LensCaps] per lens; every value survives missing characteristics keys. */
    val lensCaps: List<LensCaps> by lazy {
        try { lenses.values.map { buildLensCaps(it) } } catch (e: Throwable) { Log.w(TAG, "lens caps failed: $e"); emptyList() }
    }

    private fun buildLensCaps(info: LensInfo): LensCaps {
        val c = info.chars
        val afModes = c.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES) ?: intArrayOf()
        val minFocus = c.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) ?: 0f
        val capList = c.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES) ?: intArrayOf()
        val iso = c.get(CameraCharacteristics.SENSOR_INFO_SENSITIVITY_RANGE)
        val exp = c.get(CameraCharacteristics.SENSOR_INFO_EXPOSURE_TIME_RANGE)
        val ev = c.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)
        val awb = c.get(CameraCharacteristics.CONTROL_AWB_AVAILABLE_MODES) ?: intArrayOf()
        val ab = c.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_ANTIBANDING_MODES) ?: intArrayOf()
        val stab = c.get(CameraCharacteristics.CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES) ?: intArrayOf()
        val zoomMax = (if (Build.VERSION.SDK_INT >= 30) c.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE)?.upper else null)
            ?: c.get(CameraCharacteristics.SCALER_AVAILABLE_MAX_DIGITAL_ZOOM) ?: 1f
        return LensCaps(
            lens = info.lens,
            focus_manual = afModes.contains(CameraCharacteristics.CONTROL_AF_MODE_OFF) && minFocus > 0f,
            focus_lock = afModes.contains(CameraCharacteristics.CONTROL_AF_MODE_AUTO),
            exposure_manual = capList.contains(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_SENSOR),
            iso_min = iso?.lower ?: 0,
            iso_max = iso?.upper ?: 0,
            exposure_us_min = exp?.let { (it.lower / 1000L).coerceIn(1L, Int.MAX_VALUE.toLong()).toInt() } ?: 0,
            exposure_us_max = exp?.let { (it.upper / 1000L).coerceIn(0L, Int.MAX_VALUE.toLong()).toInt() } ?: 0,
            ev_min = ev?.lower ?: 0,
            ev_max = ev?.upper ?: 0,
            ev_step = c.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_STEP)?.toFloat() ?: 0f,
            ae_lock = c.get(CameraCharacteristics.CONTROL_AE_LOCK_AVAILABLE) == true,
            awb_lock = c.get(CameraCharacteristics.CONTROL_AWB_LOCK_AVAILABLE) == true,
            white_balances = CameraImageMapper.whiteBalances(awb).mapNotNull { WhiteBalance.fromValue(it) },
            antibandings = CameraImageMapper.antibandings(ab).mapNotNull { Antibanding.fromValue(it) },
            stabilization = stab.contains(CameraCharacteristics.CONTROL_VIDEO_STABILIZATION_MODE_ON),
            zoom_max = zoomMax
        )
    }

    private fun loadLenses(): Map<Lens, LensInfo> {
        val out = LinkedHashMap<Lens, LensInfo>()
        try {
            val mgr = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
            for (id in mgr.cameraIdList) {
                val c = mgr.getCameraCharacteristics(id)
                val lens = lensOf(c.get(CameraCharacteristics.LENS_FACING)) ?: continue
                if (lens in out) continue
                val caps = c.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES) ?: intArrayOf()
                // Skip depth-only / monochrome helper cameras.
                if (caps.isNotEmpty() && !caps.contains(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_BACKWARD_COMPATIBLE)) continue
                val flash = c.get(CameraCharacteristics.FLASH_INFO_AVAILABLE) == true
                out[lens] = LensInfo(lens, id, c, flash)
            }
        } catch (e: Throwable) {
            Log.w(TAG, "camera enumeration failed: $e")
        }
        return out
    }

    private fun loadModes(): List<CameraMode> {
        val enc = findAvcEncoder() ?: return emptyList()
        val vcaps = enc.getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC).videoCapabilities
        val result = ArrayList<CameraMode>()
        for ((lens, info) in lenses) {
            val map = info.chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP) ?: continue
            val aeRanges = info.chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES) ?: emptyArray()
            val sizes = map.getOutputSizes(SurfaceTexture::class.java) ?: continue
            val caps = ArrayList<SizeCaps>()
            for (s in sizes) {
                if (s.width < s.height || s.width > CameraModes.MAX_W || s.height > CameraModes.MAX_H) continue
                if (!vcaps.isSizeSupported(s.width, s.height)) continue
                val minDurNs = map.getOutputMinFrameDuration(SurfaceTexture::class.java, s)
                val encFps = try { vcaps.getSupportedFrameRatesFor(s.width, s.height) } catch (_: Exception) { null }
                val fps = CameraModes.FPS_CANDIDATES.filter { f ->
                    aeRanges.any { it.lower <= f && f <= it.upper } &&
                        (minDurNs <= 0 || minDurNs <= 1_000_000_000L / f + 1_000_000L) &&
                        (encFps == null || f <= encFps.upper + 0.5)
                }
                if (fps.isNotEmpty()) caps += SizeCaps(s.width, s.height, fps)
            }
            result += CameraModes.toProto(lens, caps)
        }
        return result
    }
}
