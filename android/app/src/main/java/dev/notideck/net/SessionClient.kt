// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import android.content.Context
import android.os.Build
import android.os.SystemClock
import android.util.Log
import dev.notideck.proto.v1.Capability
import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.ControlPatch
import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.Feedback
import dev.notideck.proto.v1.Goodbye
import dev.notideck.proto.v1.GoodbyeReason
import dev.notideck.proto.v1.Hello
import dev.notideck.proto.v1.PairRequest
import dev.notideck.proto.v1.PairResult
import dev.notideck.proto.v1.Welcome
import dev.notideck.proto.v1.InputEvent
import dev.notideck.proto.v1.InputKind
import dev.notideck.proto.v1.NeedIcon
import dev.notideck.proto.v1.Ping
import dev.notideck.proto.v1.Pong
import dev.notideck.proto.v1.ScreenInfo
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelChildren
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.IOException
import okio.ByteString.Companion.toByteString
import java.net.Socket
import java.util.UUID
import java.util.concurrent.atomic.AtomicInteger

/** Callbacks from the session reader to [ConnectionManager]. All run on the session's IO thread. */
interface SessionListener {
    /** Host asked us to wait for Allow on the PC. */
    fun onPairingPending(hostName: String) {}
    fun onPairResult(result: PairResult) {}
    /** Return false to drop the connection (host identity check failed); the manager sets the status. */
    fun onWelcome(welcome: Welcome): Boolean = true
}

/** Camera commands from the host (R6-1); implemented by `camera.CameraController`. Called on the session IO thread. */
interface CameraHandler {
    /** Welcome received: the control session [sessionId] is live. */
    fun onSessionStart(sessionId: String)
    fun onCameraStart(m: dev.notideck.proto.v1.CameraStart)
    fun onCameraStop()
    fun onCameraControl(m: dev.notideck.proto.v1.CameraControl)
    /** The control session ended for any reason; stop everything. */
    fun onSessionEnd()
}

/** Microphone commands from the host (round 25); implemented by `mic.MicController`. Called on the session IO thread. */
interface MicHandler {
    /** Welcome received: the control session [sessionId] is live. */
    fun onSessionStart(sessionId: String)
    fun onMicStart(m: dev.notideck.proto.v1.MicStart)
    fun onMicStop()
    /** The control session ended for any reason; stop everything. */
    fun onSessionEnd()
}

/** How a session ended. [goodbye] is set when the host said why; [error] for transport failures. */
class SessionEnd(val welcomed: Boolean, val goodbye: Goodbye?, val error: Throwable?)

/** Data for the PairRequest sent right after Hello when pairing from a QR code. */
class PairingAttempt(val proof: ByteArray, val deviceName: String)

/**
 * Speaks the control protocol over an already established (TLS) socket: framing, Hello, heartbeat,
 * layout/icon/feedback state. Choosing and securing the connection is [ConnectionManager]'s job.
 */
class SessionClient(
    private val context: Context? = null,
    private val scope: CoroutineScope = CoroutineScope(Dispatchers.IO + SupervisorJob()),
    private val themeStore: ThemeStore =
        if (context != null) PrefsThemeStore(context) else MemoryThemeStore()
) {
    companion object {
        const val TAG = "SessionClient"
        const val DEFAULT_PORT = 47800
        const val PROTO_VERSION = 2
        const val READ_TIMEOUT_MS = 8000
        const val PING_INTERVAL_MS = 2000L
        const val MAX_FRAME_SIZE = 4 * 1024 * 1024 // 4 MB safety limit
    }

    private object NdLog {
        fun d(tag: String, msg: String) {
            try { Log.d(tag, msg) } catch (_: Throwable) { println("[$tag] D: $msg") }
        }
        fun i(tag: String, msg: String) {
            try { Log.i(tag, msg) } catch (_: Throwable) { println("[$tag] I: $msg") }
        }
        fun w(tag: String, msg: String) {
            try { Log.w(tag, msg) } catch (_: Throwable) { println("[$tag] W: $msg") }
        }
    }

    private val seqCounter = AtomicInteger(1)

    /** Receives PutIcon payloads; also consulted to decide which icons to request. */
    @Volatile
    var iconSink: IconSink? = null

    @Volatile
    var listener: SessionListener? = null

    @Volatile
    var cameraHandler: CameraHandler? = null
    var micHandler: MicHandler? = null

    /** Advertised in Hello. The camera controller adds CAMERA (+TORCH) when the phone has them. */
    @Volatile
    var capabilities: List<Capability> = listOf(Capability.CAPABILITY_HAPTICS, Capability.CAPABILITY_PLAYER)

    /** Identity facts for Hello; set by the manager (the TLS cert fingerprint, the phone's name). */
    @Volatile
    var certFp: String = ""
    @Volatile
    var deviceName: String = ""

    /** Transport of the session currently running (for the Connected status). */
    @Volatile
    private var transport: Transport = Transport.USB

    /** Hashes already requested via NeedIcon on this session (dedupe). Guarded by itself. */
    private val pendingIcons = HashSet<String>()

    @Volatile
    private var activeSendChannel: Channel<Envelope>? = null

    @Volatile
    private var welcomed = false

    @Volatile
    private var lastGoodbye: Goodbye? = null

    // Pluggable time provider for testing without Android SystemClock mocks
    var timeProvider: () -> Long = {
        try {
            SystemClock.uptimeMillis()
        } catch (_: Exception) {
            System.currentTimeMillis()
        }
    }

    private val _connectionStatus = MutableStateFlow<ConnectionStatus>(ConnectionStatus.Disconnected)
    val connectionStatus: StateFlow<ConnectionStatus> = _connectionStatus.asStateFlow()

    internal fun setStatus(status: ConnectionStatus) { _connectionStatus.value = status }

    private val _deckState = MutableStateFlow(DeckState())
    val deckState: StateFlow<DeckState> = _deckState.asStateFlow()

    private val _navUi = MutableStateFlow(NavUi())
    /** Side-rail state; all-false until the host sends NavState, and again after a session ends. */
    val navUi: StateFlow<NavUi> = _navUi.asStateFlow()

    fun sendNav(action: dev.notideck.proto.v1.NavAction) {
        sendEnvelope(Envelope(nav_request = dev.notideck.proto.v1.NavRequest(action = action)))
    }

    fun sendGoToPage(index: Int) {
        sendEnvelope(Envelope(nav_request = goToPageRequest(index)))
    }

    private val _playRequest = MutableStateFlow<PlayerRequest?>(null)
    /**
     * Stream the host asked the built-in player to show; null = closed. Deliberately untouched when a
     * session ends: playback continues without the host.
     */
    val playRequest: StateFlow<PlayerRequest?> = _playRequest.asStateFlow()
    private val playSeq = AtomicInteger(0)

    /** Closes the player (host stopped/replaced it, or tests). Sends nothing to the host. */
    fun closePlayer() { _playRequest.value = null }

    /**
     * The user closed the player (Back, close button, app to background): clears it and, when the
     * request carried a session id and a session is connected, tells the host via PlayerClosed.
     */
    fun userClosedPlayer() {
        val req = _playRequest.value ?: return
        _playRequest.value = null
        if (req.sessionId.isNotEmpty()) {
            sendEnvelope(Envelope(player_closed = dev.notideck.proto.v1.PlayerClosed(session_id = req.sessionId)))
        }
    }

    private val clockEstimator = HostClockEstimator()
    /** Host wall clock minus phone wall clock in ms; 0 until a Pong with host_time_ms arrives. */
    val hostClockOffsetMs: Long get() = clockEstimator.offsetMs
    internal var wallClock: () -> Long = { System.currentTimeMillis() }

    private val _railControls = MutableStateFlow<List<Control>>(emptyList())
    /** Pinned side-rail keys from SetRail; empty until sent and after a session ends. */
    val railControls: StateFlow<List<Control>> = _railControls.asStateFlow()

    private val _feedbackEvents = MutableSharedFlow<Feedback>(extraBufferCapacity = 16)
    val feedbackEvents: SharedFlow<Feedback> = _feedbackEvents.asSharedFlow()

    private val _theme = MutableStateFlow(themeStore.load())
    /** Phone theme last chosen by the host (persisted; Black until a host says otherwise). */
    val theme: StateFlow<AppTheme> = _theme.asStateFlow()

    internal fun applyAppearance(proto: dev.notideck.proto.v1.PhoneTheme) {
        val next = AppTheme.fromProto(proto, _theme.value)
        if (next != _theme.value) {
            _theme.value = next
            try { themeStore.save(next) } catch (e: Throwable) { NdLog.w(TAG, "theme save failed: $e") }
        }
    }

    private val _sleep = MutableStateFlow<SleepConfig?>(null)
    /** Deck sleep settings from the last Appearance; null until one with `sleep` arrives (never sleeps). */
    val sleep: StateFlow<SleepConfig?> = _sleep.asStateFlow()

    internal fun applySleep(proto: dev.notideck.proto.v1.DeckSleep?) {
        _sleep.value = proto?.let { SleepConfig.from(it) }
    }

    private val _wakeEvents = MutableSharedFlow<WakeReason>(extraBufferCapacity = 8)
    /** Host-driven reasons to wake a sleeping deck (CameraStart / MicStart, PlayStream). */
    val wakeEvents: SharedFlow<WakeReason> = _wakeEvents.asSharedFlow()

    private fun nextSeq(): Int = seqCounter.getAndIncrement()

    fun sendInput(
        controlId: String,
        kind: InputKind,
        delta: Int = 0,
        value: Float = 0f,
        rail: Boolean = false
    ) {
        val currentPage = if (rail) RAIL_PAGE_ID else _deckState.value.pageId
        val input = InputEvent(
            page_id = currentPage,
            control_id = controlId,
            kind = kind,
            delta = delta,
            value_ = value,
            phone_time_ms = timeProvider()
        )
        sendEnvelope(Envelope(seq = nextSeq(), input = input))
    }

    fun sendEnvelope(envelope: Envelope) {
        val channel = activeSendChannel
        if (channel != null) {
            val sequenced = if (envelope.seq == 0) envelope.copy(seq = nextSeq()) else envelope
            channel.trySend(sequenced)
        }
    }

    /**
     * Runs one control session on [socket] until it ends. Sends Hello (and PairRequest if
     * [pairing] is set), then pumps messages. Never throws except for cancellation; closes the
     * socket when it returns or the caller is cancelled.
     */
    suspend fun runSession(
        socket: Socket,
        via: Transport,
        pairing: PairingAttempt? = null
    ): SessionEnd = withContext(Dispatchers.IO) {
        transport = via
        welcomed = false
        lastGoodbye = null
        synchronized(pendingIcons) { pendingIcons.clear() }
        val sendChannel = Channel<Envelope>(capacity = Channel.BUFFERED)
        activeSendChannel = sendChannel
        clockEstimator.reset()
        var error: Throwable? = null
        try {
            socket.tcpNoDelay = true
            socket.soTimeout = READ_TIMEOUT_MS
            val dataIn = DataInputStream(socket.getInputStream().buffered())
            val dataOut = DataOutputStream(socket.getOutputStream().buffered())
            coroutineScope {
                // Blocking reads ignore coroutine cancellation: closing the socket ends them.
                launch(start = CoroutineStart.UNDISPATCHED) {
                    try { awaitCancellation() } finally { try { socket.close() } catch (_: Exception) {} }
                }
                val reader = launch {
                    try {
                        writeEnvelope(dataOut, Envelope(seq = nextSeq(), hello = buildHello(context)))
                        if (pairing != null) {
                            writeEnvelope(
                                dataOut,
                                Envelope(
                                    seq = nextSeq(),
                                    pair_request = PairRequest(proof = pairing.proof.toByteString(), device_name = pairing.deviceName)
                                )
                            )
                        }
                        val writer = launch {
                            try {
                                for (msg in sendChannel) writeEnvelope(dataOut, msg)
                            } catch (e: Exception) {
                                NdLog.d(TAG, "Writer loop ended: ${e.message}")
                            }
                        }
                        val pinger = launch {
                            while (isActive) {
                                delay(PING_INTERVAL_MS)
                                val nonce = timeProvider()
                                clockEstimator.onPingSent(nonce, wallClock())
                                sendChannel.trySend(Envelope(seq = nextSeq(), ping = Ping(nonce = nonce)))
                            }
                        }
                        try {
                            while (isActive && lastGoodbye == null) {
                                val length = dataIn.readInt()
                                if (length <= 0 || length > MAX_FRAME_SIZE) throw IOException("Invalid frame size: $length")
                                val payload = ByteArray(length)
                                dataIn.readFully(payload)
                                handleIncomingEnvelope(Envelope.ADAPTER.decode(payload), sendChannel)
                            }
                        } finally {
                            pinger.cancel()
                            writer.cancel()
                        }
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        error = e
                    }
                }
                reader.join()
                coroutineContext.cancelChildren()
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            error = e
        } finally {
            activeSendChannel = null
            _navUi.value = NavUi()
            _railControls.value = emptyList()
            sendChannel.close()
            try { cameraHandler?.onSessionEnd() } catch (e: Throwable) { NdLog.w(TAG, "camera end: $e") }
            try { micHandler?.onSessionEnd() } catch (e: Throwable) { NdLog.w(TAG, "mic end: $e") }
            try { socket.close() } catch (_: Exception) {}
        }
        SessionEnd(welcomed, lastGoodbye, if (lastGoodbye != null) null else error)
    }

    private fun writeEnvelope(out: DataOutputStream, envelope: Envelope) {
        val payload = Envelope.ADAPTER.encode(envelope)
        out.writeInt(payload.size)
        out.write(payload)
        out.flush()
    }

    internal fun handleIncomingEnvelope(envelope: Envelope, sendChannel: Channel<Envelope>) {
        when {
            envelope.welcome != null -> {
                val welcome = envelope.welcome
                NdLog.i(TAG, "Welcome received: session=${welcome.session_id}, host=${welcome.host_name}, proto=${welcome.proto_version}")
                if (listener?.onWelcome(welcome) == false) {
                    // Identity check failed: end the session; the manager has set the status.
                    lastGoodbye = Goodbye(reason = GoodbyeReason.GOODBYE_REASON_UNSPECIFIED, message = "host identity rejected")
                    return
                }
                welcomed = true
                _connectionStatus.value = ConnectionStatus.Connected(
                    sessionId = welcome.session_id,
                    hostName = welcome.host_name,
                    protoVersion = welcome.proto_version,
                    transport = transport
                )
                if (welcome.session_id.isNotEmpty()) {
                    try { cameraHandler?.onSessionStart(welcome.session_id) } catch (e: Throwable) { NdLog.w(TAG, "camera start: $e") }
                    try { micHandler?.onSessionStart(welcome.session_id) } catch (e: Throwable) { NdLog.w(TAG, "mic start: $e") }
                }
            }
            envelope.camera_start != null -> {
                _wakeEvents.tryEmit(WakeReason.CameraOrMic)
                cameraHandler?.onCameraStart(envelope.camera_start)
            }
            envelope.camera_stop != null -> cameraHandler?.onCameraStop()
            envelope.camera_control != null -> cameraHandler?.onCameraControl(envelope.camera_control)
            envelope.mic_start != null -> {
                _wakeEvents.tryEmit(WakeReason.CameraOrMic)
                micHandler?.onMicStart(envelope.mic_start)
            }
            envelope.mic_stop != null -> micHandler?.onMicStop()
            envelope.ping != null -> {
                // Respond immediately to Ping with Pong
                val pong = Pong(nonce = envelope.ping.nonce)
                val response = Envelope(seq = nextSeq(), pong = pong)
                sendChannel.trySend(response)
            }
            envelope.pong != null -> {
                // Heartbeat response from host
                NdLog.d(TAG, "Pong received from host (nonce=${envelope.pong.nonce})")
                clockEstimator.onPong(envelope.pong.nonce, envelope.pong.host_time_ms, wallClock())
            }
            envelope.set_layout != null -> {
                val layout = envelope.set_layout
                val cols = if (layout.cols > 0) layout.cols else 5
                val rows = if (layout.rows > 0) layout.rows else 3
                val controlsMap = layout.controls.associateBy { it.id }
                _deckState.value = DeckState(
                    pageId = layout.page_id,
                    cols = cols,
                    rows = rows,
                    controls = controlsMap
                )
                requestMissingIcons(controlsMap.values, sendChannel)
                // If we hadn't received Welcome yet, SetLayout confirms active connection
                if (_connectionStatus.value !is ConnectionStatus.Connected) {
                    welcomed = true
                    _connectionStatus.value = ConnectionStatus.Connected(
                        sessionId = "",
                        hostName = "Host",
                        protoVersion = PROTO_VERSION,
                        transport = transport
                    )
                }
            }
            envelope.set_rail != null -> {
                _railControls.value = envelope.set_rail.controls.take(3)
                requestMissingIcons(_railControls.value, sendChannel)
            }
            envelope.update_controls != null -> {
                val update = envelope.update_controls
                if (update.page_id == RAIL_PAGE_ID) {
                    applyRailPatches(update.patches)
                    requestMissingIcons(_railControls.value, sendChannel)
                } else {
                    applyPatches(update.page_id, update.patches)
                    requestMissingIcons(_deckState.value.controls.values, sendChannel)
                }
            }
            envelope.put_icon != null -> {
                val icon = envelope.put_icon
                synchronized(pendingIcons) { pendingIcons.remove(icon.hash) }
                val ok = iconSink?.put(icon.hash, icon.mime_type, icon.data_.toByteArray()) ?: false
                if (!ok) NdLog.w(TAG, "Icon ${icon.hash} rejected or no sink")
            }
            envelope.feedback != null -> {
                _feedbackEvents.tryEmit(envelope.feedback)
            }
            envelope.play_stream != null -> {
                _playRequest.value = PlayerRequest.from(envelope.play_stream, playSeq.incrementAndGet().toLong())
                if (_playRequest.value != null) _wakeEvents.tryEmit(WakeReason.Player)
            }
            envelope.nav_state != null -> _navUi.value = NavUi.from(envelope.nav_state)
            envelope.appearance != null -> {
                applyAppearance(envelope.appearance.theme)
                applySleep(envelope.appearance.sleep)
            }
            envelope.pairing_pending != null -> listener?.onPairingPending(envelope.pairing_pending.host_name)
            envelope.pair_result != null -> listener?.onPairResult(envelope.pair_result)
            envelope.goodbye != null -> {
                NdLog.i(TAG, "Host sent Goodbye: reason=${envelope.goodbye.reason}, message=${envelope.goodbye.message}")
                lastGoodbye = envelope.goodbye
            }
        }
    }

    /** Sends one batched NeedIcon for referenced hashes that are neither cached nor already requested. */
    internal fun requestMissingIcons(controls: Collection<Control>, sendChannel: Channel<Envelope>) {
        val sink = iconSink ?: return
        val missing = LinkedHashSet<String>()
        for (c in controls) {
            val h = c.icon_hash
            if (h.isNotEmpty() && !sink.contains(h)) missing.add(h)
        }
        if (missing.isEmpty()) return
        val toRequest = synchronized(pendingIcons) { missing.filter { pendingIcons.add(it) } }
        if (toRequest.isEmpty()) return
        sendChannel.trySend(Envelope(seq = nextSeq(), need_icon = NeedIcon(hashes = toRequest)))
    }

    /** Patches for the pinned rail keys; ids not in the rail are ignored. */
    internal fun applyRailPatches(patches: List<ControlPatch>) {
        val byId = _railControls.value.associateBy { it.id }.toMutableMap()
        for (patch in patches) {
            val existing = byId[patch.id] ?: continue
            val c = patch.control ?: continue
            byId[patch.id] = mergeControl(existing, c, patch.fields)
        }
        _railControls.value = _railControls.value.map { byId[it.id] ?: it }
    }

    internal fun applyPatches(pageId: String, patches: List<ControlPatch>) {
        val current = _deckState.value
        if (pageId.isNotEmpty() && current.pageId.isNotEmpty() && pageId != current.pageId) {
            // Update is for another page; ignore
            return
        }

        val updatedMap = current.controls.toMutableMap()
        for (patch in patches) {
            val existing = updatedMap[patch.id]
            if (existing != null && patch.control != null) {
                updatedMap[patch.id] = mergeControl(existing, patch.control, patch.fields)
            } else if (patch.control != null) {
                updatedMap[patch.id] = patch.control
            }
        }
        _deckState.value = current.copy(controls = updatedMap)
    }

    internal fun mergeControl(base: Control, patch: Control, fields: List<String>): Control {
        if (fields.isEmpty()) {
            return patch
        }

        var id = base.id
        var kind = base.kind
        var col = base.col
        var row = base.row
        var spanW = base.span_w
        var spanH = base.span_h
        var iconHash = base.icon_hash
        var title = base.title
        var fgArgb = base.fg_argb
        var bgArgb = base.bg_argb
        var stateIndex = base.state_index
        var value = base.value_
        var horizontal = base.horizontal
        var subtitle = base.subtitle
        var meter = base.meter
        var level = base.level
        var dimmed = base.dimmed
        var face = base.face
        var detail = base.detail

        for (field in fields) {
            when (field) {
                "id" -> id = patch.id
                "kind" -> kind = patch.kind
                "col" -> col = patch.col
                "row" -> row = patch.row
                "span_w", "spanW" -> spanW = patch.span_w
                "span_h", "spanH" -> spanH = patch.span_h
                "icon_hash", "iconHash" -> iconHash = patch.icon_hash
                "title" -> title = patch.title
                "fg_argb", "fgArgb" -> fgArgb = patch.fg_argb
                "bg_argb", "bgArgb" -> bgArgb = patch.bg_argb
                "state_index", "stateIndex" -> stateIndex = patch.state_index
                "value", "value_" -> value = patch.value_
                "horizontal" -> horizontal = patch.horizontal
                "subtitle" -> subtitle = patch.subtitle
                "meter" -> meter = patch.meter
                "level" -> level = patch.level
                "dimmed" -> dimmed = patch.dimmed
                "face" -> face = patch.face
                "detail" -> detail = patch.detail
            }
        }

        return Control(
            id = id,
            kind = kind,
            col = col,
            row = row,
            span_w = spanW,
            span_h = spanH,
            icon_hash = iconHash,
            title = title,
            fg_argb = fgArgb,
            bg_argb = bgArgb,
            state_index = stateIndex,
            value_ = value,
            horizontal = horizontal,
            subtitle = subtitle,
            meter = meter,
            level = level,
            dimmed = dimmed,
            face = face,
            detail = detail
        )
    }

    fun buildHello(
        context: Context? = null,
        deviceId: String? = null,
        deviceModel: String? = null,
        androidSdk: Int? = null,
        appVersion: String? = null,
        screen: ScreenInfo? = null,
        certFp: String? = null,
        deviceName: String? = null
    ): Hello {
        val resolvedDeviceId = deviceId ?: context?.let { getOrCreateDeviceId(it) } ?: UUID.randomUUID().toString()
        val resolvedModel = deviceModel ?: "${Build.MANUFACTURER} ${Build.MODEL}".trim()
        val resolvedSdk = androidSdk ?: Build.VERSION.SDK_INT
        val resolvedAppVersion = appVersion ?: context?.let {
            try {
                it.packageManager.getPackageInfo(it.packageName, 0).versionName ?: "0.1.0"
            } catch (_: Exception) {
                "0.1.0"
            }
        } ?: "0.1.0"

        val resolvedScreen = screen ?: context?.let {
            val dm = it.resources.displayMetrics
            ScreenInfo(
                width_px = dm.widthPixels,
                height_px = dm.heightPixels,
                dpi = dm.densityDpi
            )
        } ?: ScreenInfo(width_px = 1920, height_px = 1080, dpi = 400)

        return Hello(
            proto_version = PROTO_VERSION,
            device_id = resolvedDeviceId,
            device_model = resolvedModel,
            android_sdk = resolvedSdk,
            app_version = resolvedAppVersion,
            screen = resolvedScreen,
            capabilities = capabilities,
            cert_fp = certFp ?: this.certFp,
            device_name = deviceName ?: this.deviceName.ifEmpty { resolvedModel }
        )
    }

    private fun getOrCreateDeviceId(context: Context): String {
        val prefs = context.getSharedPreferences("notideck_prefs", Context.MODE_PRIVATE)
        var id = prefs.getString("device_id", null)
        if (id.isNullOrEmpty()) {
            id = UUID.randomUUID().toString()
            prefs.edit().putString("device_id", id).apply()
        }
        return id
    }
}
