// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Rect
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraAccessException
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.CaptureResult
import android.hardware.camera2.TotalCaptureResult
import android.hardware.camera2.params.MeteringRectangle
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.SystemClock
import android.os.HandlerThread
import android.util.Log
import android.util.Range
import android.view.Surface
import dev.notideck.proto.v1.CameraImage
import dev.notideck.proto.v1.CameraReadout
import dev.notideck.proto.v1.ExposureMode
import dev.notideck.proto.v1.Lens
import dev.notideck.proto.v1.MediaKind
import dev.notideck.net.MediaConnection
import java.io.DataOutputStream
import java.io.IOException
import java.net.Socket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** What to capture and encode. [rotationOverrideDeg] 0 = automatic. */
data class PipelineConfig(
    val lens: Lens,
    val width: Int,
    val height: Int,
    val fps: Int,
    val bitrateBps: Int,
    val mirror: Boolean,
    val rotationOverrideDeg: Int = 0,
    val image: CameraImage? = null
)

interface PipelineListener {
    /** First key frame written to the host. */
    fun onStreaming()
    /** Fatal: the pipeline has stopped itself. */
    fun onFailure(message: String)
}

/**
 * Camera2 -> GL (rotate/mirror) -> MediaCodec H.264 -> backpressure queue -> media TLS socket.
 *
 * Threads: camera callbacks (nd-camera), GL (nd-gl), encoder drain (nd-enc-drain), socket writer
 * (nd-media-writer). [openMediaSocket] must return a connected, handshaken (TLS) socket.
 *
 * Round 28: the GL pass also draws to [preview]'s surface while one is attached. [previewOnly] runs the
 * camera and GL only (no encoder, no media connection; [sessionId] and [openMediaSocket] are unused).
 */
class CameraPipeline(
    private val context: Context,
    private val caps: CameraCapabilities,
    val config: PipelineConfig,
    private val sessionId: String,
    private val openMediaSocket: () -> Socket,
    private val listener: PipelineListener,
    private val queue: MediaBackpressureQueue = MediaBackpressureQueue(),
    private val preview: PreviewOutput? = null,
    val previewOnly: Boolean = false
) {
    companion object {
        const val TAG = "CameraPipeline"
        const val OPEN_TIMEOUT_S = 6L
        const val STATS_MS = 5000L
        const val STATS_TAG = "NdCamStats"
    }

    private val stopped = AtomicBoolean(false)
    private val failed = AtomicBoolean(false)
    private val camThread = HandlerThread("nd-camera").apply { start() }
    private val camHandler = Handler(camThread.looper)

    private var codec: MediaCodec? = null
    private var inputSurface: Surface? = null
    private var gl: GlFrameRenderer? = null
    private var cameraSurface: Surface? = null
    private var socket: Socket? = null
    private var drainThread: Thread? = null
    private var writerThread: Thread? = null

    @Volatile private var device: CameraDevice? = null
    @Volatile private var session: CameraCaptureSession? = null
    @Volatile private var builder: CaptureRequest.Builder? = null
    @Volatile private var lensInfo: LensInfo? = null

    // user-controlled camera state, re-applied on every request rebuild
    @Volatile private var zoom = 1f
    @Volatile private var torch = false
    @Volatile private var exposure = 0
    @Volatile private var focusPoint: Pair<Float, Float>? = null   // normalised sensor coordinates
    @Volatile private var image: CameraImage = config.image ?: CameraImage()
    @Volatile private var imageControlled = config.image != null   // then image.exposure_comp wins over [exposure]

    // latest CaptureResult values (written on the camera thread, read by the controller)
    @Volatile private var lastIso = 0
    @Volatile private var lastExposureUs = 0
    @Volatile private var lastFocusPermille = 0
    @Volatile private var lastFocused = false
    @Volatile private var lastReadoutMs = 0L

    private val stats = CamStats()
    private var statsFrames = 0L
    private var statsIn = 0L
    private var fpsLogged: Range<Int>? = null
    @Volatile private var realtimeClock = true
    private val orientation = DeviceOrientation(context) { lensInfo?.let { applyOrientation(it) } }

    @Volatile var currentLens: Lens = config.lens
        private set
    @Volatile var mirror: Boolean = config.mirror
        private set

    // ------------------------------------------------------------------ start / stop

    /** Blocks until frames are flowing into the encoder or throws. Cleans up after itself on failure. */
    fun start() {
        try {
            val info = caps.lenses[config.lens] ?: caps.lenses.values.firstOrNull()
                ?: throw IOException("no camera")
            lensInfo = info
            currentLens = info.lens
            if (previewOnly) gl = GlFrameRenderer(null, config.width, config.height, preview) else startEncoder()
            val st = gl!!.let { it.start() }
            realtimeClock = info.chars.get(CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE) ==
                CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE_REALTIME
            applyOrientation(info)
            orientation.enable()
            cameraSurface = Surface(st)
            if (!previewOnly) startMedia()
            openCamera(info)
            if (!previewOnly) camHandler.postDelayed(statsTick, STATS_MS)
        } catch (e: Throwable) {
            stop()
            throw e
        }
    }

    private fun startEncoder() {
        val enc = CameraCapabilities.findAvcEncoder() ?: throw IOException("no H.264 encoder")
        val c = MediaCodec.createByCodecName(enc.name)
        codec = c
        val cbr = enc.getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC)
            .encoderCapabilities.isBitrateModeSupported(MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR)
        // Optional tuning keys, dropped one group at a time when an encoder rejects the format.
        fun format(profile: Boolean, tuning: Int, mode: Boolean): MediaFormat =
            MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, config.width, config.height).apply {
                setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
                setInteger(MediaFormat.KEY_BIT_RATE, config.bitrateBps)
                setInteger(MediaFormat.KEY_FRAME_RATE, config.fps)
                setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2)
                if (profile) setInteger(MediaFormat.KEY_PROFILE, MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline)
                if (mode) setInteger(MediaFormat.KEY_BITRATE_MODE,
                    if (cbr) MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR else MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR)
                if (tuning >= 1) setInteger("prepend-sps-pps-to-idr-frames", 1)
                if (tuning >= 2) setInteger(MediaFormat.KEY_PRIORITY, 0)
                if (tuning >= 3 && Build.VERSION.SDK_INT >= 30) {
                    setInteger(MediaFormat.KEY_LATENCY, 0)
                    setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
                }
                if (tuning >= 4) setInteger("vendor.qti-ext-enc-low-latency.enable", 1)   // Qualcomm, API < 30 only
            }
        val ladder = listOfNotNull(
            if (Build.VERSION.SDK_INT < 30) Triple(true, 4, true) else null,
            Triple(true, 3, true), Triple(true, 2, true), Triple(true, 1, true), Triple(true, 0, true),
            Triple(true, 0, false), Triple(false, 0, false)
        )
        var configured = false
        var last: Exception? = null
        for ((i, v) in ladder.withIndex()) {
            try {
                if (i > 0) c.reset()
                c.configure(format(v.first, v.second, v.third), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                Log.i(TAG, "encoder ${enc.name} configured with profile=${v.first} tuning=${v.second} mode=${v.third}")
                configured = true
                break
            } catch (e: Exception) {
                last = e
                Log.w(TAG, "encoder format variant $i rejected: $e")
            }
        }
        if (!configured) throw IOException("encoder configure failed: ${last?.message}")
        inputSurface = c.createInputSurface()
        c.start()
        gl = GlFrameRenderer(inputSurface!!, config.width, config.height, preview)
        drainThread = Thread({ drainLoop(c) }, "nd-enc-drain").also { it.start() }
    }

    private fun startMedia() {
        val conn = MediaConnection.attach(openMediaSocket, sessionId, MediaKind.MEDIA_KIND_VIDEO) { socket = it }
        val out = conn.out
        writerThread = Thread({ writerLoop(out) }, "nd-media-writer").also { it.start() }
    }

    fun stop() {
        if (!stopped.compareAndSet(false, true)) return
        orientation.disable()
        camHandler.removeCallbacks(statsTick)
        queue.close()
        try { socket?.close() } catch (_: Exception) {}
        val latch = CountDownLatch(1)
        camHandler.post {
            try { session?.close() } catch (_: Exception) {}
            try { device?.close() } catch (_: Exception) {}
            session = null; device = null
            latch.countDown()
        }
        latch.await(2, TimeUnit.SECONDS)
        try { gl?.release() } catch (_: Exception) {}
        try { cameraSurface?.release() } catch (_: Exception) {}
        val c = codec
        drainThread?.let { if (it !== Thread.currentThread()) it.join(1000) }
        writerThread?.let { if (it !== Thread.currentThread()) it.join(1000) }
        try { c?.stop() } catch (_: Exception) {}
        try { c?.release() } catch (_: Exception) {}
        try { inputSurface?.release() } catch (_: Exception) {}
        camThread.quitSafely()
    }

    private fun fail(msg: String) {
        if (stopped.get() || !failed.compareAndSet(false, true)) return
        Log.w(TAG, "pipeline failure: $msg")
        listener.onFailure(msg)
    }

    // ------------------------------------------------------------------ encoder -> queue -> socket

    fun requestKeyFrame() {
        try {
            codec?.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
        } catch (_: Exception) {}
    }

    private fun drainLoop(c: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        var basePts = -1L
        try {
            while (!stopped.get()) {
                val i = c.dequeueOutputBuffer(info, 20_000)
                if (i < 0) continue
                try {
                    if (info.size > 0) {
                        val buf = c.getOutputBuffer(i)!!
                        buf.position(info.offset); buf.limit(info.offset + info.size)
                        val data = ByteArray(info.size)
                        buf.get(data)
                        if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) {
                            queue.offer(MediaPacket(0, true, false, data))
                        } else {
                            if (basePts < 0) basePts = info.presentationTimeUs
                            val key = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
                            stats.encoded.incrementAndGet()
                            if (key) stats.keyframes.incrementAndGet()
                            val nowUs = (if (realtimeClock) SystemClock.elapsedRealtimeNanos() else System.nanoTime()) / 1000
                            stats.encodeLatency(nowUs - info.presentationTimeUs)
                            val r = queue.offer(MediaPacket(info.presentationTimeUs - basePts, false, key, data))
                            if (r is MediaBackpressureQueue.Offer.Dropped && r.requestSync) requestKeyFrame()
                        }
                    }
                } finally {
                    c.releaseOutputBuffer(i, false)
                }
                if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) break
            }
        } catch (e: Throwable) {
            if (!stopped.get()) fail("encoder: ${e.message}")
        }
    }

    private fun writerLoop(out: DataOutputStream) {
        var announced = false
        try {
            while (true) {
                val p = queue.take() ?: break
                out.write(p.header())
                out.write(p.data)
                out.flush()
                stats.bytesSent.addAndGet((p.data.size + MediaPacket.HEADER_LEN).toLong())
                if (!announced && p.keyFrame) { announced = true; listener.onStreaming() }
            }
        } catch (e: Throwable) {
            if (!stopped.get()) fail("media connection lost")
        }
    }

    // ------------------------------------------------------------------ camera

    private val statsTick = object : Runnable {
        override fun run() {
            if (stopped.get()) return
            val g = gl
            val s = stats.drain()
            val secs = STATS_MS / 1000f
            val rendered = g?.framesRendered ?: 0L
            val fin = g?.framesIn ?: 0L
            val (qFrames, qMs, dropped) = queue.depth()
            Log.i(STATS_TAG, "cam=%.1ffps in=%.1ffps enc=%.1ffps lat=%.0f/%.0fms(avg/max) key=%d q=%df/%dms drop=%d tx=%.0fkB/s ae=%s rot=%d"
                .format(
                    (rendered - statsFrames) / secs, (fin - statsIn) / secs, s.encoded / secs, s.avgLatMs, s.maxLatMs,
                    s.keys, qFrames, qMs, dropped, s.bytes / secs / 1000f, fpsLogged, g?.rotationDeg ?: -1
                ))
            statsFrames = rendered; statsIn = fin
            camHandler.postDelayed(this, STATS_MS)
        }
    }

    /** Sets the GL rotation for [info] from the physical device rotation; safe to call live (GL fields are volatile). */
    private fun applyOrientation(info: LensInfo) {
        val sensor = info.chars.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: 0
        val auto = CameraGeometry.uprightRotation(sensor, orientation.degrees, info.lens == Lens.LENS_FRONT)
        gl?.rotationDeg = (auto + config.rotationOverrideDeg) % 360
        gl?.mirror = mirror
    }

    @SuppressLint("MissingPermission")
    private fun openCamera(info: LensInfo) {
        val opened = CountDownLatch(1)
        var error: String? = null
        val mgr = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        camHandler.post {
            try {
                mgr.openCamera(info.cameraId, object : CameraDevice.StateCallback() {
                    override fun onOpened(d: CameraDevice) {
                        if (stopped.get()) { d.close(); opened.countDown(); return }
                        device = d
                        createSession(d, info) { err -> error = err; opened.countDown() }
                    }
                    override fun onDisconnected(d: CameraDevice) {
                        d.close()
                        if (device === d) { device = null; fail("camera disconnected") }
                        error = error ?: "camera disconnected"; opened.countDown()
                    }
                    override fun onError(d: CameraDevice, code: Int) {
                        d.close()
                        if (device === d) device = null
                        error = error ?: "camera error $code"; opened.countDown()
                        fail("camera error $code")
                    }
                }, camHandler)
            } catch (e: Exception) {
                error = e.message ?: "cannot open camera"; opened.countDown()
            }
        }
        if (!opened.await(OPEN_TIMEOUT_S, TimeUnit.SECONDS)) throw IOException("camera open timed out")
        error?.let { throw IOException(it) }
    }

    @Suppress("DEPRECATION")
    private fun createSession(d: CameraDevice, info: LensInfo, done: (String?) -> Unit) {
        try {
            val surface = cameraSurface!!
            d.createCaptureSession(listOf(surface), object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(s: CameraCaptureSession) {
                    if (stopped.get() || device !== d) { done(null); return }
                    session = s
                    builder = d.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply { addTarget(surface) }
                    // LOCKED focus: one AF trigger to settle on the scene when the session starts.
                    applyRepeating(info, afTrigger = effectiveFocus(image, lensCapsOf(info)) == CameraImageMapper.FOCUS_LOCKED)
                    done(null)
                }
                override fun onConfigureFailed(s: CameraCaptureSession) = done("camera session configure failed")
            }, camHandler)
        } catch (e: Exception) {
            done(e.message ?: "camera session error")
        }
    }

    private fun fpsRange(info: LensInfo): Range<Int>? {
        val ranges = info.chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES) ?: return null
        val f = config.fps
        return ranges.filter { it.lower <= f && f <= it.upper }
            .minWithOrNull(compareBy<Range<Int>> { it.upper - it.lower }.thenBy { it.upper - f })
    }

    private fun applyRepeating(info: LensInfo, afTrigger: Boolean = false) {
        val b = builder ?: return
        val s = session ?: return
        val ch = info.chars
        b.set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
        val lc = lensCapsOf(info)
        val img = image
        val manualAe = img.exposure_mode == ExposureMode.EXPOSURE_MODE_MANUAL && lc.exposureManual
        if (manualAe) {
            val m = CameraImageMapper.manualExposure(
                img.iso, img.exposure_us, lc.isoMin, lc.isoMax, lc.expUsMin, lc.expUsMax,
                config.fps, lastIso, lastExposureUs
            )
            b.set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
            b.set(CaptureRequest.SENSOR_SENSITIVITY, m.iso)
            b.set(CaptureRequest.SENSOR_EXPOSURE_TIME, m.exposureNs)
            b.set(CaptureRequest.SENSOR_FRAME_DURATION, m.frameDurationNs)
        } else {
            b.set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON)
            if (lc.aeLock) b.set(CaptureRequest.CONTROL_AE_LOCK, img.ae_lock)
        }
        fpsRange(info)?.let {
            b.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, it)
            if (fpsLogged != it) { fpsLogged = it; Log.i(TAG, "AE fps range $it for ${config.fps}fps (available: ${ch.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)?.joinToString()})") }
        }
        val afModes = ch.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES) ?: intArrayOf()
        val focus = effectiveFocus(img, lc)
        CameraImageMapper.afMode(focus, afModes.contains(CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO))
            ?.let { b.set(CaptureRequest.CONTROL_AF_MODE, it) }
        if (focus == CameraImageMapper.FOCUS_MANUAL) {
            b.set(CaptureRequest.LENS_FOCUS_DISTANCE, CameraImageMapper.focusDiopters(img.focus_permille, lc.minFocus))
        }
        val awbModes = ch.get(CameraCharacteristics.CONTROL_AWB_AVAILABLE_MODES) ?: intArrayOf()
        val awb = CameraImageMapper.awbMode(img.white_balance.value, awbModes)
        if (awbModes.isNotEmpty()) b.set(CaptureRequest.CONTROL_AWB_MODE, awb)
        if (lc.awbLock) b.set(CaptureRequest.CONTROL_AWB_LOCK, img.awb_lock && awb == CameraImageMapper.AWB_AUTO)
        CameraImageMapper.antibandingMode(img.antibanding.value, ch.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_ANTIBANDING_MODES) ?: intArrayOf())
            ?.let { b.set(CaptureRequest.CONTROL_AE_ANTIBANDING_MODE, it) }
        if (lc.stabilization) {
            b.set(CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE,
                if (img.stabilization) CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_ON else CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_OFF)
        }
        // zoom
        val range = if (Build.VERSION.SDK_INT >= 30) ch.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE) else null
        if (Build.VERSION.SDK_INT >= 30 && range != null) {
            b.set(CaptureRequest.CONTROL_ZOOM_RATIO, zoom.coerceIn(range.lower, range.upper))
        } else {
            val active = ch.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE)
            val maxZoom = ch.get(CameraCharacteristics.SCALER_AVAILABLE_MAX_DIGITAL_ZOOM) ?: 1f
            if (active != null) {
                val c = CameraGeometry.cropForZoom(active.width(), active.height(), zoom.coerceIn(1f, maxZoom))
                b.set(CaptureRequest.SCALER_CROP_REGION, Rect(active.left + c[0], active.top + c[1], active.left + c[2], active.top + c[3]))
            }
        }
        b.set(CaptureRequest.FLASH_MODE, if (torch && info.hasFlash) CaptureRequest.FLASH_MODE_TORCH else CaptureRequest.FLASH_MODE_OFF)
        val evRange = ch.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)
        if (evRange != null) {
            val ev = if (imageControlled) img.exposure_comp else exposure
            b.set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION, CameraImageMapper.clampEv(ev, evRange.lower, evRange.upper))
        }
        focusPoint?.let { (nx, ny) ->
            val active = ch.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE)
            if (active != null) {
                val (sx, sy) = nx to ny
                val half = (minOf(active.width(), active.height()) * 0.08f).toInt()
                val cx = active.left + (sx * active.width()).toInt()
                val cy = active.top + (sy * active.height()).toInt()
                val r = MeteringRectangle(
                    (cx - half).coerceIn(active.left, active.right - 1),
                    (cy - half).coerceIn(active.top, active.bottom - 1),
                    2 * half, 2 * half, MeteringRectangle.METERING_WEIGHT_MAX - 1
                )
                if (focus != CameraImageMapper.FOCUS_MANUAL && (ch.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AF) ?: 0) > 0) b.set(CaptureRequest.CONTROL_AF_REGIONS, arrayOf(r))
                if ((ch.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AE) ?: 0) > 0) b.set(CaptureRequest.CONTROL_AE_REGIONS, arrayOf(r))
            }
        }
        try {
            if (afTrigger && focus != CameraImageMapper.FOCUS_MANUAL) {
                b.set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_START)
                s.capture(b.build(), null, camHandler)
            }
            b.set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_IDLE)
            s.setRepeatingRequest(b.build(), resultCallback, camHandler)
        } catch (e: CameraAccessException) {
            Log.w(TAG, "capture request: $e")
        } catch (e: IllegalStateException) {
            Log.w(TAG, "capture request (closed): $e")
        }
    }

    /** Static per-lens numbers the image settings need, read straight from the characteristics. */
    private class LensLimits(
        val focusManual: Boolean, val focusLock: Boolean, val exposureManual: Boolean, val minFocus: Float,
        val isoMin: Int, val isoMax: Int, val expUsMin: Int, val expUsMax: Int,
        val aeLock: Boolean, val awbLock: Boolean, val stabilization: Boolean
    )

    private fun lensCapsOf(info: LensInfo): LensLimits {
        val c = info.chars
        val af = c.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES) ?: intArrayOf()
        val minFocus = c.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) ?: 0f
        val iso = c.get(CameraCharacteristics.SENSOR_INFO_SENSITIVITY_RANGE)
        val exp = c.get(CameraCharacteristics.SENSOR_INFO_EXPOSURE_TIME_RANGE)
        val capList = c.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES) ?: intArrayOf()
        return LensLimits(
            focusManual = af.contains(CaptureRequest.CONTROL_AF_MODE_OFF) && minFocus > 0f,
            focusLock = af.contains(CaptureRequest.CONTROL_AF_MODE_AUTO),
            exposureManual = capList.contains(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_SENSOR) && iso != null && exp != null,
            minFocus = minFocus,
            isoMin = iso?.lower ?: 0, isoMax = iso?.upper ?: 0,
            expUsMin = exp?.let { (it.lower / 1000L).coerceIn(1L, Int.MAX_VALUE.toLong()).toInt() } ?: 1,
            expUsMax = exp?.let { (it.upper / 1000L).coerceIn(0L, Int.MAX_VALUE.toLong()).toInt() } ?: 0,
            aeLock = c.get(CameraCharacteristics.CONTROL_AE_LOCK_AVAILABLE) == true,
            awbLock = c.get(CameraCharacteristics.CONTROL_AWB_LOCK_AVAILABLE) == true,
            stabilization = (c.get(CameraCharacteristics.CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES) ?: intArrayOf())
                .contains(CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_ON)
        )
    }

    private fun effectiveFocus(img: CameraImage, lc: LensLimits): Int =
        CameraImageMapper.effectiveFocus(img.focus_mode.value, lc.focusManual, lc.focusLock)

    /** Keeps the latest sensor values for [readout]; cheap, and throttled to a few updates a second. */
    private val resultCallback = object : CameraCaptureSession.CaptureCallback() {
        override fun onCaptureCompleted(s: CameraCaptureSession, r: CaptureRequest, result: TotalCaptureResult) {
            val now = SystemClock.elapsedRealtime()
            if (now - lastReadoutMs < 250) return
            lastReadoutMs = now
            val info = lensInfo ?: return
            result.get(CaptureResult.SENSOR_SENSITIVITY)?.let { lastIso = it }
            result.get(CaptureResult.SENSOR_EXPOSURE_TIME)?.let { lastExposureUs = (it / 1000L).coerceIn(0L, Int.MAX_VALUE.toLong()).toInt() }
            val minFocus = info.chars.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) ?: 0f
            result.get(CaptureResult.LENS_FOCUS_DISTANCE)?.let { lastFocusPermille = CameraImageMapper.focusPermille(it, minFocus) }
            val af = result.get(CaptureResult.CONTROL_AF_STATE)
            lastFocused = af == CaptureResult.CONTROL_AF_STATE_FOCUSED_LOCKED || af == CaptureResult.CONTROL_AF_STATE_PASSIVE_FOCUSED
        }
    }

    /** The camera's current state for the host (null until the first result arrived). */
    fun readout(): CameraReadout? {
        if (lastReadoutMs == 0L) return null
        return CameraReadout(iso = lastIso, exposure_us = lastExposureUs, focus_permille = lastFocusPermille, focused = lastFocused)
    }

    private fun reapply(afTrigger: Boolean = false) {
        val info = lensInfo ?: return
        camHandler.post { if (!stopped.get()) applyRepeating(info, afTrigger) }
    }

    // ------------------------------------------------------------------ controls

    fun setZoom(ratio: Float) { zoom = ratio; reapply() }
    fun setTorch(on: Boolean) { torch = on; reapply() }
    fun setExposure(ev: Int) { exposure = ev; reapply() }
    /** Replaces the whole image settings set; entering LOCKED focus fires one AF trigger. */
    fun setImage(img: CameraImage) {
        val info = lensInfo
        val before = image
        image = img
        imageControlled = true
        val enteringLock = info != null && effectiveFocus(img, lensCapsOf(info)) == CameraImageMapper.FOCUS_LOCKED &&
            effectiveFocus(before, lensCapsOf(info)) != CameraImageMapper.FOCUS_LOCKED
        reapply(afTrigger = enteringLock)
    }

    fun focusAt(x: Float, y: Float) {
        // Map now with the current rotation so a later orientation change cannot skew the stored point.
        focusPoint = CameraGeometry.outputToSensor(x.coerceIn(0f, 1f), y.coerceIn(0f, 1f), gl?.rotationDeg ?: 0, mirror)
        reapply(afTrigger = true)
    }

    /** Moves capture to [lens] keeping the encoder and the connection. Blocks until the new camera streams. */
    fun switchLens(lens: Lens, mirrorNow: Boolean) {
        val info = caps.lenses[lens] ?: throw IOException("no such lens")
        if (info.lens == currentLens && mirrorNow == mirror) return
        val old = device
        val oldSession = session
        session = null; device = null
        val latch = CountDownLatch(1)
        camHandler.post {
            try { oldSession?.close() } catch (_: Exception) {}
            try { old?.close() } catch (_: Exception) {}
            latch.countDown()
        }
        latch.await(2, TimeUnit.SECONDS)
        lensInfo = info
        currentLens = info.lens
        mirror = mirrorNow
        zoom = 1f; focusPoint = null
        lastIso = 0; lastExposureUs = 0; lastFocusPermille = 0; lastFocused = false; lastReadoutMs = 0L
        applyOrientation(info)
        openCamera(info)
        requestKeyFrame()
    }
}
