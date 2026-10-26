// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import android.content.Context
import android.os.Build
import android.provider.Settings
import android.util.Log
import dev.notideck.pairing.HostStore
import dev.notideck.pairing.PairedHost
import dev.notideck.pairing.PairingCrypto
import dev.notideck.pairing.PairingUri
import dev.notideck.proto.v1.GoodbyeReason
import dev.notideck.proto.v1.PairResult
import dev.notideck.proto.v1.Welcome
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException

/** One-shot things the UI should react to. */
sealed class ManagerEvent {
    /** A stored PC rejected us (REVOKED / UNAUTHORIZED); it was forgotten. UI goes to the PCs screen. */
    data class HostRemoved(val name: String) : ManagerEvent()
}

/** How a [ConnectionManager] reaches out; replaceable so tests need no sockets. */
interface Connector {
    fun connect(endpoint: Endpoint, timeoutMs: Int): TlsLink
    /** True if something answers on the USB tunnel (adb reverse accepts, TLS server waits for us). */
    fun usbAlive(): Boolean
}

class TlsConnector(private val identity: ClientIdentity) : Connector {
    override fun connect(endpoint: Endpoint, timeoutMs: Int): TlsLink =
        Tls.connect(endpoint.host, endpoint.port, endpoint.pinFp, identity, timeoutMs)

    override fun usbAlive(): Boolean {
        val s = Socket()
        return try {
            s.connect(InetSocketAddress(ConnectionPlan.USB_HOST, ConnectionPlan.USB_PORT), 500)
            s.soTimeout = 400
            // adbd accepts the phone-side connection even when the PC is down and then closes it.
            // A live agent sends nothing first (TLS waits for our ClientHello), so a timeout = alive.
            try { s.getInputStream().read() != -1 } catch (_: SocketTimeoutException) { true }
        } catch (_: Exception) {
            false
        } finally {
            try { s.close() } catch (_: Exception) {}
        }
    }
}

/**
 * Chooses and secures the control connection (ADR 0004 §8): USB first, then the paired PCs' last
 * addresses, then mDNS; probes USB while on LAN and moves over to it; walks QR pairing.
 */
class ConnectionManager(
    private val client: SessionClient,
    private val hostStore: HostStore,
    private val identity: ClientIdentity,
    private val discovery: HostDiscovery,
    private val connector: Connector = TlsConnector(identity),
    private val scope: CoroutineScope = CoroutineScope(Dispatchers.IO + SupervisorJob()),
    private val deviceName: String = "${Build.MODEL}",
    private val clock: () -> Long = System::currentTimeMillis
) {
    companion object {
        const val TAG = "ConnectionManager"
        const val CONNECT_TIMEOUT_MS = 1500
        const val INITIAL_BACKOFF_MS = 1000L
        const val MAX_BACKOFF_MS = 10_000L
        const val BACKOFF_MULTIPLIER = 1.5
        const val USB_PROBE_INTERVAL_MS = 5000L
        const val NOTICE_MS = 4000L
        private val PAIRING_REASONS = setOf(
            GoodbyeReason.GOODBYE_REASON_UNAUTHORIZED,
            GoodbyeReason.GOODBYE_REASON_UNPAIRED,
            GoodbyeReason.GOODBYE_REASON_PAIRING_DENIED
        )

        fun deviceName(context: Context): String =
            try { Settings.Global.getString(context.contentResolver, "device_name") } catch (_: Exception) { null }
                ?.takeIf { it.isNotBlank() } ?: Build.MODEL
    }

    private val _hosts = MutableStateFlow(hostStore.all())
    /** Paired PCs, most recently used first. */
    val hosts: StateFlow<List<PairedHost>> = _hosts.asStateFlow()

    private val _discovered = MutableStateFlow<List<DiscoveredHost>>(emptyList())
    val discovered: StateFlow<List<DiscoveredHost>> = _discovered.asStateFlow()

    private val _events = MutableSharedFlow<ManagerEvent>(extraBufferCapacity = 8)
    val events: SharedFlow<ManagerEvent> = _events.asSharedFlow()

    @Volatile private var running = false
    private var loopJob: Job? = null
    private var attemptJob: Job? = null
    private var mirrorJob: Job? = null
    @Volatile private var pendingPair: PairingUri? = null
    private val wake = Channel<Unit>(Channel.CONFLATED)

    /** Sleeps up to [ms], returning early when [pair] wants the loop's attention. */
    private suspend fun nap(ms: Long) {
        if (pendingPair == null) withTimeoutOrNull(ms) { wake.receive() }
    }

    // ------------------------------------------------------------------ lifecycle

    fun start() {
        if (running) return
        running = true
        client.deviceName = deviceName
        client.listener = Listener()
        mirrorJob = scope.launch {
            discovery.services.collect { refreshHosts() }
        }
        loopJob = scope.launch {
            // Key generation can take a moment on first launch; keep it off the main thread.
            client.certFp = try { identity.fingerprintHex() } catch (e: Throwable) { Log.w(TAG, "identity: $e"); "" }
            loop()
        }
    }

    fun stop() {
        running = false
        loopJob?.cancel(); mirrorJob?.cancel()
        loopJob = null; mirrorJob = null
        discovery.stop()
        client.setStatus(ConnectionStatus.Disconnected)
    }

    /** Starts pairing from a scanned/opened code. Interrupts whatever the loop is doing. */
    fun pair(uri: PairingUri) {
        pendingPair = uri
        attemptJob?.cancel()
        wake.trySend(Unit)
    }

    fun forget(hostId: String) {
        hostStore.remove(hostId)
        refreshHosts()
    }

    private fun refreshHosts() {
        _hosts.value = hostStore.all().sortedByDescending { it.lastConnected }
        _discovered.value = ConnectionPlan.classify(discovery.services.value, _hosts.value)
    }

    // ------------------------------------------------------------------ main loop

    private suspend fun loop() {
        var attempt = 0
        var backoff = INITIAL_BACKOFF_MS
        while (scope.isActive && running) {
            val pair = pendingPair
            pendingPair = null
            wake.tryReceive()
            var outcome: Outcome = Outcome.Retry(null)
            val job = scope.launch { outcome = if (pair != null) runPairing(pair) else runNormal() }
            attemptJob = job
            try { job.join() } catch (e: CancellationException) { job.cancel(); throw e }
            if (job.isCancelled && pendingPair != null) continue // interrupted by pair()
            if (!running) break
            when (val o = outcome) {
                is Outcome.Connected -> { attempt = 0; backoff = INITIAL_BACKOFF_MS; nap(o.pauseMs) }
                is Outcome.Notice -> {
                    client.setStatus(ConnectionStatus.Notice(o.message, o.isError))
                    attempt = 0; backoff = INITIAL_BACKOFF_MS
                    nap(o.holdMs)
                    if (pendingPair == null) client.setStatus(ConnectionStatus.Connecting)
                }
                is Outcome.Retry -> {
                    attempt++
                    client.setStatus(ConnectionStatus.Reconnecting(attempt, backoff, o.error))
                    nap(backoff)
                    backoff = (backoff * BACKOFF_MULTIPLIER).toLong().coerceAtMost(MAX_BACKOFF_MS)
                }
            }
        }
        client.setStatus(ConnectionStatus.Disconnected)
    }

    private sealed class Outcome {
        data class Retry(val error: String?) : Outcome()
        data class Connected(val pauseMs: Long) : Outcome()
        data class Notice(val message: String, val isError: Boolean = true, val holdMs: Long = NOTICE_MS) : Outcome()
    }

    // ------------------------------------------------------------------ normal sessions

    private suspend fun runNormal(): Outcome {
        client.setStatus(
            if (client.connectionStatus.value is ConnectionStatus.Reconnecting) client.connectionStatus.value
            else ConnectionStatus.Connecting
        )
        discovery.start()
        val hosts = hostStore.all()
        var lastError: String? = null
        var link: TlsLink? = null
        var used: Endpoint? = null
        for (ep in ConnectionPlan.plan(hosts, discovery.services.value)) {
            try {
                link = withContext(Dispatchers.IO) { connector.connect(ep, CONNECT_TIMEOUT_MS) }
                used = ep
                break
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                // Plain "connection refused" on the USB port is the normal "no cable" case.
                if (ep.transport == Transport.LAN) lastError = describe(e)
                else if (lastError == null && !isRefused(e)) lastError = describe(e)
            }
        }
        if (link == null || used == null) return Outcome.Retry(lastError)
        discovery.stop()
        return runLinked(link, used, pairing = null, pairUri = null)
    }

    /** Runs a session on a connected link and turns its end into an [Outcome]. */
    private suspend fun runLinked(link: TlsLink, ep: Endpoint, pairing: PairingAttempt?, pairUri: PairingUri?): Outcome {
        current = SessionContext(link, ep, pairUri)
        val end = coroutineScope {
            var probe: Job? = null
            if (ep.transport == Transport.LAN) {
                probe = launch {
                    while (isActive) {
                        delay(USB_PROBE_INTERVAL_MS)
                        if (connector.usbAlive()) {
                            Log.i(TAG, "USB came up; leaving LAN session")
                            usbSwitch = true
                            try { link.socket.close() } catch (_: Exception) {}
                            return@launch
                        }
                    }
                }
            }
            try {
                client.runSession(link.socket, ep.transport, pairing)
            } finally {
                probe?.cancel()
            }
        }
        val ctx = current
        current = null
        val switched = usbSwitch
        usbSwitch = false
        if (end.welcomed) {
            ctx?.hostId?.let { id ->
                hostStore.get(id)?.let { h ->
                    val addrs = if (ep.transport == Transport.LAN) listOf(ep.host) + h.addrs.filter { it != ep.host } else h.addrs
                    hostStore.put(h.copy(lastConnected = clock(), addrs = addrs.take(4)))
                }
                refreshHosts()
            }
        }
        val g = end.goodbye
        return when {
            ctx?.rejected != null -> Outcome.Notice(ctx.rejected!!, holdMs = 6000)
            switched -> Outcome.Connected(pauseMs = 0)
            pairUri != null && g != null && g.reason in PAIRING_REASONS ->
                Outcome.Notice(g.message.ifBlank { "Pairing failed" }, holdMs = 6000)
            g == null -> if (end.welcomed) Outcome.Connected(pauseMs = 500) else Outcome.Retry(end.error?.let(::describe))
            else -> afterGoodbye(g.reason, g.message, ep, link, ctx)
        }
    }

    private fun afterGoodbye(
        reason: GoodbyeReason,
        message: String,
        ep: Endpoint,
        link: TlsLink,
        ctx: SessionContext?
    ): Outcome {
        val stored = ctx?.hostId?.let { hostStore.get(it) } ?: hostStore.byFingerprint(link.serverFp)
        return when (reason) {
            GoodbyeReason.GOODBYE_REASON_SUPERSEDED -> Outcome.Connected(pauseMs = 300) // quiet: USB takes over
            GoodbyeReason.GOODBYE_REASON_SHUTDOWN -> Outcome.Retry(null)
            GoodbyeReason.GOODBYE_REASON_VERSION_MISMATCH ->
                Outcome.Notice(message.ifBlank { "Version mismatch: update the Notideck app and the PC app" }, holdMs = 8000)
            GoodbyeReason.GOODBYE_REASON_PAIRING_DENIED ->
                Outcome.Notice("The PC denied this phone", holdMs = 10_000)
            GoodbyeReason.GOODBYE_REASON_REVOKED -> removedByHost(stored, "This PC removed the phone")
            GoodbyeReason.GOODBYE_REASON_UNAUTHORIZED ->
                if (stored != null) removedByHost(stored, "This PC no longer knows the phone")
                else Outcome.Notice(message.ifBlank { "Not authorised: pair this phone by scanning the QR code" })
            GoodbyeReason.GOODBYE_REASON_UNPAIRED ->
                if (stored != null && ep.transport == Transport.LAN) removedByHost(stored, "This PC no longer knows the phone")
                else Outcome.Notice(message.ifBlank { "Pairing timed out" })
            else -> Outcome.Retry(message.ifBlank { null })
        }
    }

    private fun removedByHost(stored: PairedHost?, text: String): Outcome {
        if (stored != null) {
            hostStore.remove(stored.hostId)
            refreshHosts()
            _events.tryEmit(ManagerEvent.HostRemoved(stored.name))
        }
        return Outcome.Notice(text, holdMs = 6000)
    }

    // ------------------------------------------------------------------ QR pairing

    private suspend fun runPairing(uri: PairingUri): Outcome {
        if (uri.isExpired(clock())) return Outcome.Notice("Code expired. Make a new one on the PC.")
        client.setStatus(ConnectionStatus.Pairing(uri.hostName))
        var lastError: String? = null
        var link: TlsLink? = null
        var used: Endpoint? = null
        for (addr in uri.addrs) {
            val ep = Endpoint(addr, uri.port, Transport.LAN, uri.hostFpHex, uri.hostId)
            try {
                link = withContext(Dispatchers.IO) { connector.connect(ep, 3000) }
                used = ep
                break
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                lastError = describe(e)
            }
        }
        if (link == null || used == null) {
            return Outcome.Notice("Could not reach ${uri.hostName}: ${lastError ?: "no address"}", holdMs = 6000)
        }
        discovery.stop()
        val clientFp = identity.fingerprintHex()
        val proof = PairingCrypto.proof(uri.secret, clientFp, link.serverFp)
        pairingFailure = null
        val outcome = runLinked(link, used, PairingAttempt(proof, deviceName), uri)
        pairingFailure?.let { msg -> pairingFailure = null; return Outcome.Notice(msg, holdMs = 6000) }
        return outcome
    }

    /**
     * Opens the media connection (ADR 0005): a second TLS connection to the endpoint the live
     * control session uses, same client identity, pinned to the certificate that session saw.
     */
    fun openMediaLink(): TlsLink {
        val ctx = current ?: throw java.io.IOException("no control session")
        val ep = ctx.ep.copy(pinFp = ctx.ep.pinFp ?: ctx.link.serverFp)
        return connector.connect(ep, 3000)
    }

    // ------------------------------------------------------------------ session callbacks

    private class SessionContext(val link: TlsLink, val ep: Endpoint, val pairUri: PairingUri?) {
        @Volatile var hostId: String? = ep.hostId
        @Volatile var rejected: String? = null
    }

    @Volatile private var current: SessionContext? = null
    @Volatile private var usbSwitch = false
    @Volatile private var pairingFailure: String? = null

    private inner class Listener : SessionListener {
        override fun onPairingPending(hostName: String) {
            client.setStatus(ConnectionStatus.WaitingApproval(hostName.ifBlank { "the PC" }))
        }

        override fun onPairResult(result: PairResult) {
            val ctx = current ?: return
            val uri = ctx.pairUri
            if (!result.ok) {
                pairingFailure = result.error.ifBlank { "The PC refused the pairing" }
                return
            }
            if (uri != null) {
                if (result.host_id.isNotEmpty() && result.host_id != uri.hostId) {
                    pairingFailure = "Unexpected PC identity"
                    ctx.rejected = pairingFailure
                    return
                }
                hostStore.put(
                    PairedHost(
                        hostId = uri.hostId,
                        name = result.host_name.ifBlank { uri.hostName },
                        fp = ctx.link.serverFp,
                        addrs = (listOf(ctx.ep.host) + uri.addrs).distinct(),
                        port = uri.port,
                        lastConnected = clock()
                    )
                )
                ctx.hostId = uri.hostId
                refreshHosts()
            }
            client.setStatus(ConnectionStatus.Paired(result.host_name.ifBlank { uri?.hostName ?: "PC" }))
        }

        override fun onWelcome(welcome: Welcome): Boolean {
            val ctx = current ?: return true
            val fp = ctx.link.serverFp
            val id = welcome.host_id
            if (id.isEmpty()) return true // older host build; nothing to pin against
            val known = hostStore.get(id)
            if (known != null && !known.fp.equals(fp, ignoreCase = true)) {
                ctx.rejected = "${known.name}: certificate changed. Forget this PC and pair again."
                return false
            }
            val pinned = ctx.ep.pinFp
            if (pinned != null && !pinned.equals(fp, ignoreCase = true)) return false
            if (known == null) {
                if (ctx.pairUri != null && ctx.pairUri.hostId != id) {
                    ctx.rejected = "Unexpected PC identity"
                    return false
                }
                // Trust on first use over USB (the adb tunnel); QR pairing stored it already.
                hostStore.put(PairedHost(id, welcome.host_name.ifBlank { id }, fp, emptyList(), SessionClient.DEFAULT_PORT, clock()))
            } else if (known.name != welcome.host_name && welcome.host_name.isNotBlank()) {
                hostStore.put(known.copy(name = welcome.host_name))
            }
            ctx.hostId = id
            refreshHosts()
            if (client.connectionStatus.value is ConnectionStatus.Paired) {
                try { Thread.sleep(900) } catch (_: InterruptedException) {} // let "Paired" register
            }
            return true
        }
    }

    // ------------------------------------------------------------------ helpers

    private fun isRefused(e: Exception): Boolean =
        e is java.net.ConnectException || e.message?.contains("refused", ignoreCase = true) == true

    private fun describe(e: Throwable): String {
        val root = generateSequence(e) { it.cause }.last()
        val m = root.message ?: root.javaClass.simpleName
        return when {
            root is SocketTimeoutException -> "timed out"
            m.contains("pinned", ignoreCase = true) -> "PC certificate changed. Forget it and pair again"
            else -> m
        }.take(100)
    }
}
