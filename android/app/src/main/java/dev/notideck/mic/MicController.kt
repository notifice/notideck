// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.mic

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import androidx.core.content.ContextCompat
import dev.notideck.camera.CameraService
import dev.notideck.net.MicHandler
import dev.notideck.proto.v1.Capability
import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.MicStart
import dev.notideck.proto.v1.MicState
import dev.notideck.proto.v1.MicStatus
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.net.Socket

/** What the deck UI shows about the microphone. */
data class MicUi(val streaming: Boolean = false, val permissionNeeded: Boolean = false)

/**
 * Owns the phone-microphone lifecycle for the control session: answers MicStart/MicStop, reports
 * MicStatus, asks for RECORD_AUDIO and keeps the session foreground service informed. All state
 * changes happen on one private thread. Independent of the camera (its own media connection).
 *
 * [send] delivers envelopes on the control session; [openMediaSocket] opens the audio TLS connection.
 */
class MicController(
    private val context: Context,
    private val send: (Envelope) -> Unit,
    private val openMediaSocket: () -> Socket,
    private val startService: Boolean = true
) : MicHandler {
    companion object {
        const val TAG = "MicController"
        const val OPEN_APP_MESSAGE = "Open Notideck on the phone to start the microphone"

        fun advertisedCapabilities(context: Context): List<Capability> =
            if (context.packageManager.hasSystemFeature(PackageManager.FEATURE_MICROPHONE)) listOf(Capability.CAPABILITY_MIC)
            else emptyList()
    }

    private val app = context.applicationContext
    private val thread = HandlerThread("nd-mic-ctl").apply { start() }
    private val h = Handler(thread.looper)

    private val _ui = MutableStateFlow(MicUi())
    val ui: StateFlow<MicUi> = _ui.asStateFlow()

    // controller-thread state
    private var sessionId: String? = null
    private var pipeline: MicPipeline? = null
    private val logic = MicRequestLogic()

    fun capabilities(): List<Capability> = advertisedCapabilities(app)

    fun release() {
        post { teardown(MicState.MIC_STATE_STOPPED, "", notify = false) }
        thread.quitSafely()
    }

    private fun post(r: () -> Unit) { h.post { try { r() } catch (e: Throwable) { Log.w(TAG, "controller: $e") } } }

    private fun granted() =
        ContextCompat.checkSelfPermission(app, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED

    // ------------------------------------------------------------------ MicHandler

    override fun onSessionStart(sessionId: String) = post {
        this.sessionId = sessionId
        status(if (granted()) MicState.MIC_STATE_STOPPED else MicState.MIC_STATE_NO_PERMISSION)
    }

    override fun onMicStart(m: MicStart) = post { begin(m) }

    override fun onMicStop() = post {
        logic.onStop()
        _ui.value = _ui.value.copy(permissionNeeded = false)
        teardown(MicState.MIC_STATE_STOPPED, "", notify = true)
    }

    override fun onSessionEnd() = post {
        teardown(MicState.MIC_STATE_STOPPED, "", notify = false)
        logic.onStop()
        sessionId = null
        _ui.value = MicUi()
    }

    /** UI reports the outcome of the RECORD_AUDIO request. A denial is final until the PC asks again. */
    fun permissionResult(granted: Boolean) = post {
        _ui.value = _ui.value.copy(permissionNeeded = false)
        val p = logic.onPermissionResult(granted)
        if (granted) refreshService()
        if (sessionId == null) return@post
        if (p != null) begin(p) else if (!granted) status(MicState.MIC_STATE_NO_PERMISSION)
    }

    /** The app came to the front: run a start that was waiting for that, ask for a pending permission once. */
    fun resume() = post {
        if (sessionId == null) return@post
        val ok = granted()
        if (startService && ok && !CameraService.holdsMic) refreshService()
        val a = logic.onVisible(ok)
        a.retry?.let { begin(it) }
        if (a.askPermission) _ui.value = _ui.value.copy(permissionNeeded = true)
    }

    // ------------------------------------------------------------------ start / stop

    private fun begin(m: MicStart) {
        val sid = sessionId ?: return
        teardown(MicState.MIC_STATE_STOPPED, "", notify = false)
        val ok = granted()
        val visible = !startService || CameraService.appVisible
        val plan = logic.onStart(m, ok, visible, canRunInBackground = !startService || CameraService.holdsMic)
        when (plan) {
            is MicPlan.NeedPermission -> {
                status(MicState.MIC_STATE_NO_PERMISSION)
                if (plan.askNow) _ui.value = _ui.value.copy(permissionNeeded = true)
            }
            MicPlan.NeedForeground -> status(MicState.MIC_STATE_ERROR, OPEN_APP_MESSAGE)
            MicPlan.Begin -> launch(m, sid)
        }
    }

    private fun launch(m: MicStart, sid: String) {
        val rate = if (m.sample_rate > 0) m.sample_rate else MicAudio.DEFAULT_RATE
        val frameMs = MicAudio.frameMs(m.frame_ms)
        status(MicState.MIC_STATE_STARTING, rate = rate)
        lateinit var p: MicPipeline
        p = MicPipeline(rate, frameMs, m.voice_processing, sid, openMediaSocket, object : MicPipelineListener {
            override fun onStreaming() = post {
                if (pipeline === p) {
                    _ui.value = _ui.value.copy(streaming = true)
                    status(MicState.MIC_STATE_STREAMING, rate = p.actualRate)
                }
            }
            override fun onFailure(message: String) = post {
                if (pipeline === p) teardown(MicState.MIC_STATE_ERROR, message, notify = true)
            }
        })
        pipeline = p
        if (startService) serviceMic(true)
        try {
            p.start()
        } catch (e: Throwable) {
            Log.w(TAG, "pipeline start failed: $e")
            pipeline = null
            if (startService) serviceMic(false)
            status(MicState.MIC_STATE_ERROR, (e.message ?: e.javaClass.simpleName).take(80))
            return
        }
        status(MicState.MIC_STATE_STARTING, rate = p.actualRate)
    }

    private fun teardown(state: MicState, error: String, notify: Boolean) {
        val p = pipeline
        pipeline = null
        val had = p != null
        if (p != null) {
            try { p.stop() } catch (e: Throwable) { Log.w(TAG, "stop: $e") }
            if (startService) serviceMic(false)
        }
        if (_ui.value.streaming) _ui.value = _ui.value.copy(streaming = false)
        if (notify && (had || state == MicState.MIC_STATE_ERROR || state == MicState.MIC_STATE_STOPPED)) status(state, error)
    }

    private fun status(state: MicState, error: String = "", rate: Int = 0) {
        if (sessionId == null) return
        send(Envelope(mic_status = MicStatus(state = state, error = error, sample_rate = rate)))
    }

    private fun serviceMic(on: Boolean) = CameraService.micChanged(app, on)

    private fun refreshService() { if (startService) CameraService.refresh(app) }
}
