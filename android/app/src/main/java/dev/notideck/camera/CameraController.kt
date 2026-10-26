// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.os.BatteryManager
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.PowerManager
import android.util.Log
import android.view.Surface
import androidx.core.content.ContextCompat
import dev.notideck.MainActivity
import dev.notideck.net.CameraHandler
import dev.notideck.proto.v1.CameraControl
import dev.notideck.proto.v1.CameraMode
import dev.notideck.proto.v1.CameraStart
import dev.notideck.proto.v1.CameraState
import dev.notideck.proto.v1.CameraStatus
import dev.notideck.proto.v1.Capability
import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.Lens
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.io.IOException
import java.net.Socket

/** What the deck UI shows about the camera. */
data class CameraUi(val streaming: Boolean = false, val permissionNeeded: Boolean = false)

/** Process-wide handle so the foreground service's Stop action can reach the controller. */
object CameraRuntime {
    @Volatile var controller: CameraController? = null
}

/**
 * Owns the camera lifecycle for the control session: answers CameraStart/Stop/Control, reports
 * CameraStatus, runs the foreground service and the thermal guard. All state changes happen on one
 * private thread, so the pipeline never sees interleaved commands.
 *
 * [send] delivers envelopes on the control session; [openMediaSocket] opens the second TLS
 * connection (ADR 0005).
 *
 * Round 28: also the [CameraPreview]. Surfaces attach from the main thread straight into a [PreviewOutput]
 * that every pipeline's GL pass draws to; the controller thread only decides (via [PreviewPolicy]) whether a
 * preview-only pipeline (camera + GL, no encoder, no media link, nothing sent to the host) has to run.
 */
class CameraController(
    private val context: Context,
    private val send: (Envelope) -> Unit,
    private val openMediaSocket: () -> Socket,
    private val caps: CameraCapabilities = CameraCapabilities(context),
    private val startService: Boolean = true
) : CameraHandler, CameraPreview {
    companion object {
        const val TAG = "CameraController"
        const val OPEN_APP_MESSAGE = "Open Notideck on the phone to start the camera"
        const val REQUEST_CHANNEL = "camera-request"
        const val REQUEST_NOTIFICATION_ID = 42
        const val READOUT_MS = 1000L
        const val DEFAULT_BITRATE = 4_000_000
        const val THROTTLED_BITRATE = 2_500_000
        fun advertisedCapabilities(context: Context, caps: CameraCapabilities): List<Capability> {
            val out = mutableListOf(Capability.CAPABILITY_HAPTICS, Capability.CAPABILITY_PLAYER)
            if (CameraCapabilities.hasCameraHardware(context) && caps.hasCamera) {
                out += Capability.CAPABILITY_CAMERA
                if (caps.hasTorch) out += Capability.CAPABILITY_TORCH
            }
            return out
        }
    }

    private val app = context.applicationContext
    private val thread = HandlerThread("nd-cam-ctl").apply { start() }
    private val h = Handler(thread.looper)

    private val _ui = MutableStateFlow(CameraUi())
    val ui: StateFlow<CameraUi> = _ui.asStateFlow()

    private val _state = MutableStateFlow(PreviewState())
    override val state: StateFlow<PreviewState> = _state.asStateFlow()

    // preview: owners and the output are touched from the main thread, the rest from the controller thread
    private val owners = PreviewOwners<PreviewTarget>()   // guarded by itself
    private val attachLock = Any()                        // orders attach/detach -> previewOut.set
    private val previewOut: PreviewOutput = PreviewOutput(object : PreviewOutput.Listener {
        override fun onPreviewActive(target: PreviewTarget, active: Boolean) {
            if (previewOut.target === target) _state.update { it.copy(active = active, error = if (active) "" else it.error) }
        }
        override fun onPreviewError(target: PreviewTarget, message: String) {
            if (previewOut.target === target) _state.update { it.copy(active = false, error = message.take(80)) }
        }
    })
    private val previewStore = PreviewSettingsStore(context)
    private var previewPipeline: CameraPipeline? = null
    /** The last CameraStart of this run (plus later lens / image changes), for preview-only sessions. */
    private var lastStart: PreviewSettings? = null
    /** Set by the thermal stop: no preview-only session until the next attach. */
    @Volatile private var previewHold = false

    // controller-thread state
    private var sessionId: String? = null
    private var pipeline: CameraPipeline? = null
    private var request: CameraStart? = null
    private var active: CameraStart? = null
    private var pendingStart: CameraStart? = null
    private var throttled = false
    private var lastTempC = 0f
    private var thermalStatus = -1
    private var receiver: BroadcastReceiver? = null
    private var thermalListener: PowerManager.OnThermalStatusChangedListener? = null
    private val stepUp = ThermalPolicy.StepUpTracker()
    private var deferredStart: CameraStart? = null
    private val thermalTick = object : Runnable {
        override fun run() {
            try { evaluateThermal() } catch (e: Throwable) { Log.w(TAG, "thermal tick: $e") }
            if (receiver != null) h.postDelayed(this, 30_000)
        }
    }

    /** True while the thermal guard has stepped the stream down; reported in DeviceHealth. */
    @Volatile var isThrottled = false
        private set

    init { CameraRuntime.controller = this }

    /** Capabilities for Hello. Enumerates cameras, so call off the main thread. */
    fun capabilities(): List<Capability> = advertisedCapabilities(app, caps)

    fun release() {
        post { stopPreviewOnly(); teardown(CameraState.CAMERA_STATE_STOPPED, "", notify = false) }
        if (CameraRuntime.controller === this) CameraRuntime.controller = null
        thread.quitSafely()
    }

    private fun post(r: () -> Unit) { h.post { try { r() } catch (e: Throwable) { Log.w(TAG, "controller: $e") } } }

    /** [CameraUi] and the preview's streamingToPc share one truth. */
    private fun setUi(v: CameraUi) {
        _ui.value = v
        if (_state.value.streamingToPc != v.streaming) _state.update { it.copy(streamingToPc = v.streaming) }
    }

    // ------------------------------------------------------------------ CameraPreview

    override fun attach(owner: Any, surface: Surface, width: Int, height: Int) {
        val t = PreviewTarget(surface, width, height)
        previewHold = false
        synchronized(attachLock) {
            synchronized(owners) { owners.attach(owner, t) }
            _state.update { it.copy(active = false, error = "") }
            previewOut.set(t)
        }
        post { reconcilePreview() }
    }

    override fun detach(owner: Any) {
        synchronized(attachLock) {
            val top = synchronized(owners) {
                val before = owners.top
                val after = owners.detach(owner)
                if (before === after) return
                after
            }
            _state.update { it.copy(active = false) }
            // Waits until the GL thread no longer draws to the departing surface.
            previewOut.set(top)
        }
        post { reconcilePreview() }
    }

    /** The app went to the foreground or the background: preview-only sessions only run in the foreground. */
    fun onAppVisibility() = post { reconcilePreview() }

    /** Starts or stops the preview-only session to match the attached surfaces and the PC's stream. */
    private fun reconcilePreview() {
        val attached = synchronized(owners) { !owners.isEmpty } && !previewHold
        val streaming = pipeline != null
        val permitted = hasPermission()
        val needPermission = PreviewPolicy.permissionNeeded(streaming, attached, permitted, caps.hasCamera)
        if (_state.value.permissionNeeded != needPermission) _state.update { it.copy(permissionNeeded = needPermission) }
        if (attached && !streaming && !caps.hasCamera) _state.update { it.copy(error = "no camera") }
        when (PreviewPolicy.decide(streaming, attached, CameraService.appVisible, permitted, caps.hasCamera)) {
            PreviewMode.PREVIEW_ONLY -> if (previewPipeline == null) startPreviewOnly()
            PreviewMode.STREAMING, PreviewMode.OFF -> stopPreviewOnly()
        }
    }

    private fun startPreviewOnly() {
        val s = PreviewSettings.resolve(lastStart, previewStore.load())
        val choice = CameraModes.pickClosest(s.width, s.height, s.fps, s.lens, caps.modes)
            ?: return previewFailed("no supported camera mode")
        val cfg = PipelineConfig(choice.lens, choice.width, choice.height, choice.fps, 0, s.mirror, s.rotationDeg, s.image)
        lateinit var p: CameraPipeline
        p = CameraPipeline(app, caps, cfg, "", { throw IOException("preview only") }, object : PipelineListener {
            override fun onStreaming() {}
            override fun onFailure(message: String) = post {
                if (previewPipeline === p) { stopPreviewOnly(); previewFailed(message) }
            }
        }, preview = previewOut, previewOnly = true)
        _state.update { it.copy(frameWidth = cfg.width, frameHeight = cfg.height) }
        try {
            p.start()
        } catch (e: Throwable) {
            Log.w(TAG, "preview start failed: $e")
            return previewFailed((e.message ?: e.javaClass.simpleName).take(80))
        }
        Log.i(TAG, "preview only: ${cfg.lens} ${cfg.width}x${cfg.height}@${cfg.fps}")
        previewPipeline = p
    }

    private fun stopPreviewOnly() {
        val p = previewPipeline ?: return
        previewPipeline = null
        p.stop()
        _state.update { it.copy(active = false) }
    }

    private fun previewFailed(msg: String) {
        _state.update { it.copy(active = false, error = msg) }
    }

    /** CameraControl while only the phone previews: same effect as on a stream, nothing goes to the host. */
    private fun previewControl(m: CameraControl) {
        val p = previewPipeline ?: return
        m.zoom_ratio?.let { p.setZoom(it) }
        m.torch?.let { p.setTorch(it) }
        val fx = m.focus_x
        val fy = m.focus_y
        if (fx != null && fy != null) p.focusAt(fx, fy)
        val img = m.image
        if (img != null) {
            p.setImage(img)
            // Keep it for a preview restart (lens switch) even without a CameraStart in this run.
            if (lastStart == null) lastStart = PreviewSettings.resolve(null, previewStore.load()).copy(image = img)
        } else {
            m.exposure_comp?.let { p.setExposure(it) }
        }
        val lens = m.lens ?: return
        if (lens == Lens.LENS_UNSPECIFIED || lens == p.currentLens || caps.lenses[lens] == null) return
        lastStart = PreviewSettings.resolve(lastStart, previewStore.load()).copy(lens = lens)
        stopPreviewOnly()
        reconcilePreview()
    }

    // ------------------------------------------------------------------ CameraHandler

    override fun onSessionStart(sessionId: String) {
        post {
            this.sessionId = sessionId
            status(CameraState.CAMERA_STATE_STOPPED)
        }
    }

    override fun onCameraStart(m: CameraStart) = post {
        // Preview-only sessions reuse these settings (this run, and persisted for the next one).
        PreviewSettings.from(m).let { lastStart = it; previewStore.save(it) }
        begin(m)
    }

    override fun onCameraStop() = post { clearDeferred(); teardown(CameraState.CAMERA_STATE_STOPPED, "", notify = true) }

    /** Stop button on the notification. */
    fun stopFromUser() = post { teardown(CameraState.CAMERA_STATE_STOPPED, "", notify = true) }

    override fun onSessionEnd() {
        post {
            teardown(CameraState.CAMERA_STATE_STOPPED, "", notify = false)
            sessionId = null
            pendingStart = null
            clearDeferred()
            setUi(CameraUi())
        }
    }

    override fun onCameraControl(m: CameraControl) = post { control(m) }

    /** The app came to the front: run a CameraStart that arrived while it could not (see [OPEN_APP_MESSAGE]). */
    fun resumeDeferred() = post {
        val d = deferredStart ?: return@post
        clearDeferred()
        if (sessionId != null) begin(d)
    }

    private fun clearDeferred() {
        if (deferredStart == null) return
        deferredStart = null
        try { (app.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).cancel(REQUEST_NOTIFICATION_ID) } catch (_: Throwable) {}
    }

    private fun postOpenNotification() {
        try {
            val nm = app.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            if (Build.VERSION.SDK_INT >= 26) {
                nm.createNotificationChannel(NotificationChannel(REQUEST_CHANNEL, "Camera requests", NotificationManager.IMPORTANCE_DEFAULT))
            }
            val open = PendingIntent.getActivity(
                app, 2, Intent(app, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            )
            val b = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(app, REQUEST_CHANNEL) else @Suppress("DEPRECATION") Notification.Builder(app)
            nm.notify(REQUEST_NOTIFICATION_ID, b.setContentTitle("Your PC wants to use the camera")
                .setContentText("Tap to open Notideck and start it")
                .setSmallIcon(android.R.drawable.presence_video_online)
                .setAutoCancel(true).setContentIntent(open).build())
        } catch (e: Throwable) { Log.w(TAG, "request notification: $e") }
    }

    /** UI reports the outcome of the CAMERA permission request. */
    fun permissionResult(granted: Boolean) = post {
        val p = pendingStart
        pendingStart = null
        setUi(_ui.value.copy(permissionNeeded = false))
        if (granted && p != null && sessionId != null) begin(p)
        reconcilePreview()   // opens a preview that waited for the permission, if a surface is still attached
    }

    // ------------------------------------------------------------------ start / stop

    private fun hasPermission() =
        ContextCompat.checkSelfPermission(app, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

    private fun begin(m: CameraStart) {
        val sid = sessionId ?: return
        teardown(CameraState.CAMERA_STATE_STOPPED, "", notify = false)
        stopPreviewOnly()   // the PC wins the camera; the attached surface moves to the streaming pipeline
        request = m
        if (!caps.hasCamera) return fail("no camera")
        if (startService && !CameraService.appVisible && !CameraService.holdsCamera) {
            // Android 12+ will not let a background app start the camera or a camera-type service.
            deferredStart = m
            postOpenNotification()
            return fail(OPEN_APP_MESSAGE, keepRequest = false)
        }
        if (!hasPermission()) {
            pendingStart = m
            setUi(_ui.value.copy(permissionNeeded = true))
            return fail("camera permission", keepRequest = true)
        }
        status(CameraState.CAMERA_STATE_STARTING)
        var choice = CameraModes.pickClosest(m.width, m.height, m.fps, m.lens, caps.modes)
            ?: return fail("no supported camera mode")
        if (throttled) choice = CameraModes.throttled(choice, caps.modes)
        if (startService) startForegroundService()
        launch(m, choice, sid)
    }

    private fun launch(m: CameraStart, choice: CameraModes.Choice, sid: String) {
        val bitrate = (if (m.bitrate_bps > 0) m.bitrate_bps else DEFAULT_BITRATE)
            .coerceIn(300_000, 20_000_000).let { if (throttled) minOf(it, THROTTLED_BITRATE) else it }
        val cfg = PipelineConfig(choice.lens, choice.width, choice.height, choice.fps, bitrate, m.mirror, m.rotation_deg, m.image)
        lateinit var p: CameraPipeline
        p = CameraPipeline(app, caps, cfg, sid, openMediaSocket, object : PipelineListener {
            override fun onStreaming() = post {
                if (pipeline === p) {
                    setUi(_ui.value.copy(streaming = true))
                    status(if (throttled) CameraState.CAMERA_STATE_THROTTLED else CameraState.CAMERA_STATE_STREAMING)
                }
            }
            override fun onFailure(message: String) = post {
                if (pipeline === p) teardown(CameraState.CAMERA_STATE_ERROR, message, notify = true)
            }
        }, preview = previewOut)
        try {
            p.start()
        } catch (e: Throwable) {
            Log.w(TAG, "pipeline start failed: $e")
            val msg = e.message ?: e.javaClass.simpleName
            if (startService) stopForegroundService()
            fail(msg.take(80), keepRequest = false)
            return
        }
        pipeline = p
        previewHold = false
        _state.update { it.copy(frameWidth = cfg.width, frameHeight = cfg.height, permissionNeeded = false) }
        active = CameraStart(
            width = cfg.width, height = cfg.height, fps = cfg.fps, bitrate_bps = cfg.bitrateBps,
            lens = p.currentLens, rotation_deg = m.rotation_deg, mirror = m.mirror
        )
        startThermalGuard()
        h.removeCallbacks(readoutTick)
        h.postDelayed(readoutTick, READOUT_MS)
    }

    /** About once a second while a pipeline runs: what the camera is doing right now. */
    private val readoutTick = object : Runnable {
        override fun run() {
            val p = pipeline ?: return
            p.readout()?.let { send(Envelope(camera_readout = it)) }
            h.postDelayed(this, READOUT_MS)
        }
    }

    private fun teardown(state: CameraState, error: String, notify: Boolean) {
        val p = pipeline
        pipeline = null
        h.removeCallbacks(readoutTick)
        stopThermalGuard()
        p?.stop()
        val wasActive = p != null || active != null
        active = null
        if (p != null || _ui.value.streaming) {
            if (startService) stopForegroundService()
        }
        setUi(_ui.value.copy(streaming = false))
        if (notify && (wasActive || state == CameraState.CAMERA_STATE_ERROR || sessionId != null)) {
            status(state, error)
        }
        if (state != CameraState.CAMERA_STATE_ERROR) request = null
        // Falls back to preview only while a surface is attached (a no-op if begin() starts a stream again).
        post { reconcilePreview() }
    }

    private fun fail(msg: String, keepRequest: Boolean = false) {
        active = null
        status(CameraState.CAMERA_STATE_ERROR, msg)
        if (!keepRequest) request = null
    }

    private fun status(state: CameraState, error: String = "") {
        send(Envelope(camera_status = CameraStatus(
            state = state,
            active = active,
            supported_modes = if (caps.hasCamera) caps.modes else emptyList(),
            error = error,
            lens_caps = if (caps.hasCamera) caps.lensCaps else emptyList()
        )))
    }

    // ------------------------------------------------------------------ controls

    private fun control(m: CameraControl) {
        m.image?.let { img -> lastStart = lastStart?.copy(image = img) }
        val p = pipeline ?: return previewControl(m)
        if (m.request_keyframe) p.requestKeyFrame()
        m.zoom_ratio?.let { p.setZoom(it) }
        m.torch?.let { p.setTorch(it) }
        val fx = m.focus_x
        val fy = m.focus_y
        if (fx != null && fy != null) p.focusAt(fx, fy)
        val img = m.image
        if (img != null) {
            request = request?.copy(image = img)   // a thermal restart must keep the latest settings
            p.setImage(img)
        } else {
            m.exposure_comp?.let { p.setExposure(it) }
        }
        m.lens?.let { if (it != Lens.LENS_UNSPECIFIED) switchLens(it) }
    }

    private fun switchLens(lens: Lens) {
        val p = pipeline ?: return
        val a = active ?: return
        val req = request ?: return
        if (lens == p.currentLens) return
        val choice = CameraModes.pickClosest(a.width, a.height, a.fps, lens, caps.modes) ?: return
        if (choice.lens != lens) return // that lens does not exist
        if (choice.width == a.width && choice.height == a.height && choice.fps == a.fps) {
            try {
                p.switchLens(lens, req.mirror)
                active = a.copy(lens = lens)
                lastStart = lastStart?.copy(lens = lens)
                status(if (throttled) CameraState.CAMERA_STATE_THROTTLED else CameraState.CAMERA_STATE_STREAMING)
            } catch (e: Throwable) {
                teardown(CameraState.CAMERA_STATE_ERROR, "lens switch failed: ${e.message}".take(80), notify = true)
            }
        } else {
            // Different size: restart cleanly (new CONFIG packet).
            begin(req.copy(lens = lens, width = choice.width, height = choice.height, fps = choice.fps))
        }
    }

    // ------------------------------------------------------------------ thermal guard

    private fun startThermalGuard() {
        if (receiver != null) return
        val r = object : BroadcastReceiver() {
            override fun onReceive(c: Context?, i: Intent?) {
                val t = i?.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, Int.MIN_VALUE) ?: return
                if (t == Int.MIN_VALUE) return
                lastTempC = t / 10f
                evaluateThermal()
            }
        }
        receiver = r
        val sticky = app.registerReceiver(r, IntentFilter(Intent.ACTION_BATTERY_CHANGED), null, h)
        sticky?.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, Int.MIN_VALUE)?.takeIf { it != Int.MIN_VALUE }?.let { lastTempC = it / 10f }
        if (Build.VERSION.SDK_INT >= 29) {
            val pm = app.getSystemService(Context.POWER_SERVICE) as PowerManager
            val l = PowerManager.OnThermalStatusChangedListener { s -> post { thermalStatus = s; evaluateThermal() } }
            thermalListener = l
            try { pm.addThermalStatusListener(l) } catch (_: Throwable) { thermalListener = null }
        }
        evaluateThermal()
        h.postDelayed(thermalTick, 30_000)
    }

    private fun stopThermalGuard() {
        h.removeCallbacks(thermalTick)
        receiver?.let { try { app.unregisterReceiver(it) } catch (_: Exception) {} }
        receiver = null
        if (Build.VERSION.SDK_INT >= 29) {
            thermalListener?.let {
                try { (app.getSystemService(Context.POWER_SERVICE) as PowerManager).removeThermalStatusListener(it) } catch (_: Throwable) {}
            }
        }
        thermalListener = null
    }

    private fun evaluateThermal() {
        if (pipeline == null) return
        when (ThermalPolicy.decide(lastTempC, thermalStatus, throttled)) {
            ThermalPolicy.Action.Stop -> {
                // No automatic fall back to preview only on a hot phone; a new attach may try again.
                previewHold = true
                teardown(CameraState.CAMERA_STATE_ERROR, "phone too hot", notify = true)
                previewFailed("phone too hot")
            }
            ThermalPolicy.Action.Throttle -> {
                throttled = true
                isThrottled = true
                stepUp.reset()
                val req = request ?: return
                val sid = sessionId ?: return
                val cur = active ?: return
                val choice = CameraModes.throttled(
                    CameraModes.Choice(cur.lens, cur.width, cur.height, cur.fps), caps.modes
                )
                Log.i(TAG, "thermal throttle: ${lastTempC} C -> $choice")
                pipeline?.stop(); pipeline = null
                stopThermalGuard()
                launch(req, choice, sid)
                if (pipeline != null) status(CameraState.CAMERA_STATE_THROTTLED)
            }
            ThermalPolicy.Action.None -> {
                if (!throttled) return
                if (stepUp.update(lastTempC, thermalStatus)) {
                    val req = request ?: return
                    val cur = active ?: return
                    Log.i(TAG, "thermal recovered at $lastTempC C, back to the requested mode")
                    throttled = false
                    isThrottled = false
                    stepUp.reset()
                    begin(req.copy(lens = cur.lens))
                }
            }
        }
    }

    // ------------------------------------------------------------------ foreground service

    private fun startForegroundService() = sendToService(CameraService.ACTION_CAMERA_ON)

    private fun stopForegroundService() = sendToService(CameraService.ACTION_CAMERA_OFF)

    private fun sendToService(action: String) {
        try {
            val i = Intent(app, CameraService::class.java).setAction(action)
            // CAMERA_OFF must not start a service that is not running.
            if (action == CameraService.ACTION_CAMERA_OFF) {
                if (CameraService.running) app.startService(i)
            } else ContextCompat.startForegroundService(app, i)
        } catch (e: Throwable) {
            Log.w(TAG, "foreground service: $e")
        }
    }
}
