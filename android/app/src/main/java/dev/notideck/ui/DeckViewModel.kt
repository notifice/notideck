// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import android.app.Application
import android.graphics.BitmapFactory
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.lifecycle.AndroidViewModel
import dev.notideck.AppRuntime
import dev.notideck.camera.CameraController
import dev.notideck.camera.CameraUi
import dev.notideck.net.AppTheme
import dev.notideck.net.IconCache
import dev.notideck.net.IconDecoder
import dev.notideck.net.ConnectionStatus
import dev.notideck.net.DeckState
import dev.notideck.net.ClientIdentity
import dev.notideck.net.ConnectionManager
import dev.notideck.net.DiscoveredHost
import dev.notideck.net.ManagerEvent
import dev.notideck.net.NsdDiscovery
import dev.notideck.net.SessionClient
import dev.notideck.pairing.PairedHost
import dev.notideck.pairing.PairingUri
import dev.notideck.pairing.PrefsHostStore
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import dev.notideck.proto.v1.Feedback
import dev.notideck.proto.v1.InputKind
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow

enum class PairStart { Started, Expired, Invalid }

sealed class UiEvent {
    object ShowDeck : UiEvent()
    object ShowPcs : UiEvent()
}

/**
 * ViewModel managing the Deck session state and lifecycle.
 */
class DeckViewModel(application: Application) : AndroidViewModel(application) {

    private val ctx = application.applicationContext
    private val runtime = AppRuntime.get(application).also { it.attachUi() }
    private val client = runtime.client
    private val manager = runtime.manager
    private val camera = runtime.camera

    /** Camera streaming / permission state for the deck UI. */
    val cameraUi: StateFlow<CameraUi> = camera.ui

    /** Local camera preview (tile and full-screen view). */
    val cameraPreview: dev.notideck.camera.CameraPreview = runtime.cameraPreview

    private val _cameraFullScreen = kotlinx.coroutines.flow.MutableStateFlow(false)
    /** The full-screen camera preview is shown over the deck. */
    val cameraFullScreen: StateFlow<Boolean> = _cameraFullScreen
    fun openCameraFullScreen() { _cameraFullScreen.value = true }
    fun closeCameraFullScreen() { _cameraFullScreen.value = false }

    fun cameraPermissionResult(granted: Boolean) = camera.permissionResult(granted)

    /** Microphone streaming / permission state for the deck UI. */
    val micUi: StateFlow<dev.notideck.mic.MicUi> = runtime.mic.ui

    fun micPermissionResult(granted: Boolean) = runtime.mic.permissionResult(granted)

    /** Paired PCs and PCs seen on the LAN, for the PCs screen. */
    val hosts: StateFlow<List<PairedHost>> = manager.hosts
    val discovered: StateFlow<List<DiscoveredHost>> = manager.discovered

    private val _uiEvents = MutableSharedFlow<UiEvent>(extraBufferCapacity = 8)
    val uiEvents: SharedFlow<UiEvent> = _uiEvents.asSharedFlow()

    val connectionStatus: StateFlow<ConnectionStatus> = client.connectionStatus
    val deckState: StateFlow<DeckState> = client.deckState
    val navUi: StateFlow<dev.notideck.net.NavUi> = client.navUi
    fun sendNav(action: dev.notideck.proto.v1.NavAction) = client.sendNav(action)
    fun sendGoToPage(index: Int) = client.sendGoToPage(index)
    val railControls: StateFlow<List<dev.notideck.proto.v1.Control>> = client.railControls
    val theme: StateFlow<AppTheme> = client.theme
    val playRequest: StateFlow<dev.notideck.net.PlayerRequest?> = client.playRequest
    fun closePlayer() = client.userClosedPlayer()
    fun hostClockOffsetMs(): Long = client.hostClockOffsetMs
    val sleepConfig: StateFlow<dev.notideck.net.SleepConfig?> = client.sleep
    val wakeEvents: SharedFlow<dev.notideck.net.WakeReason> = client.wakeEvents
    val feedbackEvents: SharedFlow<Feedback> = client.feedbackEvents

    private val iconCache = IconCache<ImageBitmap>(
        decoder = IconDecoder { bytes ->
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
        },
        sizeOf = { it.width * it.height * 4 }
    )

    /** Decoded icons keyed by hex sha256 hash. */
    val icons: StateFlow<Map<String, ImageBitmap>> = iconCache.icons

    init {
        client.iconSink = iconCache
        runtime.start()
        viewModelScope.launch {
            manager.events.collect { e ->
                if (e is ManagerEvent.HostRemoved) _uiEvents.emit(UiEvent.ShowPcs)
            }
        }
    }

    /**
     * Starts pairing from QR text or a `notideck://pair` link. [Expired] and [Invalid] are
     * reported to the caller without touching the connection; a valid code interrupts it.
     */
    fun startPairing(text: String): PairStart {
        val uri = PairingUri.parseOrNull(text) ?: return PairStart.Invalid
        if (uri.isExpired()) return PairStart.Expired
        manager.pair(uri)
        _uiEvents.tryEmit(UiEvent.ShowDeck)
        return PairStart.Started
    }

    /** Deep link entry: errors surface on the connection overlay since no scanner is open. */
    fun openPairingLink(text: String) {
        when (startPairing(text)) {
            PairStart.Started -> Unit
            PairStart.Expired -> client.setStatus(ConnectionStatus.Notice("Code expired. Make a new one on the PC."))
            PairStart.Invalid -> client.setStatus(ConnectionStatus.Notice("Not a Notideck pairing code."))
        }
        _uiEvents.tryEmit(UiEvent.ShowDeck)
    }

    fun forget(hostId: String) = manager.forget(hostId)

    fun sendInput(
        controlId: String,
        kind: InputKind,
        delta: Int = 0,
        value: Float = 0f,
        rail: Boolean = false
    ) {
        client.sendInput(
            rail = rail,
            controlId = controlId,
            kind = kind,
            delta = delta,
            value = value
        )
    }

    override fun onCleared() {
        super.onCleared()
        client.iconSink = null
        runtime.detachUi()
    }
}
