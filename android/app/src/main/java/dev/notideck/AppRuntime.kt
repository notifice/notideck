// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck

import android.Manifest
import android.app.Application
import android.content.pm.PackageManager
import androidx.core.content.ContextCompat
import dev.notideck.camera.CameraCapabilities
import dev.notideck.camera.CameraController
import dev.notideck.camera.CameraPreview
import dev.notideck.camera.CameraService
import dev.notideck.net.ClientIdentity
import dev.notideck.net.ConnectionManager
import dev.notideck.net.ConnectionStatus
import dev.notideck.net.HealthReporter
import dev.notideck.net.NsdDiscovery
import dev.notideck.net.SessionClient
import dev.notideck.mic.MicController
import dev.notideck.pairing.PrefsHostStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/**
 * Process-wide owner of the control session and the camera. It outlives the activity's ViewModel,
 * so the foreground service keeps the connection (and a running stream) when the user swipes the
 * app away; the UI just binds to it. Released when nothing needs it any more: no UI attached and
 * no service running.
 */
class AppRuntime private constructor(private val app: Application) {
    companion object {
        @Volatile private var instance: AppRuntime? = null

        fun get(app: Application): AppRuntime = synchronized(this) {
            instance ?: AppRuntime(app).also { instance = it }
        }

        fun peek(): AppRuntime? = instance
    }

    val client = SessionClient(context = app)
    val hostStore = PrefsHostStore(app)
    val manager = ConnectionManager(
        client = client,
        hostStore = hostStore,
        identity = ClientIdentity(),
        discovery = NsdDiscovery(app),
        deviceName = ConnectionManager.deviceName(app)
    )
    val camera = CameraController(
        context = app,
        send = client::sendEnvelope,
        openMediaSocket = { manager.openMediaLink().socket },
        caps = CameraCapabilities(app)
    )
    /** Round 28: preview for the CAMERA tile / full-screen view. */
    val cameraPreview: CameraPreview = camera

    val mic = MicController(
        context = app,
        send = client::sendEnvelope,
        openMediaSocket = { manager.openMediaLink().socket }
    )

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val health = HealthReporter(app, client::sendEnvelope, { camera.isThrottled })
    private var started = false
    @Volatile private var uiAttached = false
    private var hostName = ""
    private var connected = false

    init {
        client.cameraHandler = camera
        client.micHandler = mic
        scope.launch {
            client.connectionStatus.collect { s ->
                if (s is ConnectionStatus.Connected) {
                    hostName = s.hostName
                    if (!connected) { connected = true; health.start() }
                    ensureSessionService()
                } else if (connected) {
                    connected = false
                    health.stop()
                    CameraService.endSession(app)
                }
            }
        }
    }

    /** Idempotent: enumerates cameras (Hello needs the capability list) then starts connecting. */
    fun start() {
        if (started) return
        started = true
        scope.launch(Dispatchers.Default) {
            client.capabilities = try { camera.capabilities() + mic.capabilities() } catch (_: Throwable) { client.capabilities }
            manager.start()
        }
    }

    private fun cameraGranted() =
        ContextCompat.checkSelfPermission(app, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

    /** Starts the idle "Connected to <PC>" foreground service; only possible while the app is visible. */
    private fun ensureSessionService() {
        if (connected && CameraService.appVisible && !CameraService.sessionUp) CameraService.startSession(app, hostName)
    }

    fun onVisible() {
        CameraService.appVisible = true
        start()
        ensureSessionService()
        camera.resumeDeferred()
        camera.onAppVisibility()
        mic.resume()
    }

    fun onHidden() {
        CameraService.appVisible = false
        camera.onAppVisibility()   // a preview-only session stops in the background
    }

    /** The notification's Disconnect action. The next time the app is shown it connects again. */
    fun disconnect() {
        started = false
        manager.stop()
    }

    fun attachUi() { uiAttached = true }

    fun detachUi() {
        uiAttached = false
        if (!CameraService.running && !CameraService.sessionUp) shutdown()
    }

    /** Called by the service when it goes away. */
    fun onServiceGone() {
        if (!uiAttached) shutdown()
    }

    private fun shutdown() {
        synchronized(Companion) { if (instance === this) instance = null }
        health.stop()
        camera.release()
        mic.release()
        manager.stop()
        scope.cancel()
    }
}
