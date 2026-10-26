// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.ui.Alignment
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import dev.notideck.net.SleepLogic
import dev.notideck.net.SleepTimer
import dev.notideck.net.WakeReason
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.platform.LocalContext
import dev.notideck.net.ConnectionStatus
import dev.notideck.net.FeedbackFlashes
import dev.notideck.net.Flash
import dev.notideck.proto.v1.FeedbackKind
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdShapes
import dev.notideck.ui.theme.NdType

enum class Screen { Deck, Pcs, Scan }

/**
 * Top-level screen: the deck plate plus the connection overlay.
 *
 * [icons] defaults to empty; once DeckViewModel exposes `icons` (WP-A1) the caller passes
 * `viewModel.icons.collectAsState().value`.
 */
@Composable
fun DeckScreen(
    viewModel: DeckViewModel,
    modifier: Modifier = Modifier,
    icons: Map<String, ImageBitmap> = emptyMap()
) {
    val context = LocalContext.current
    val connectionStatus by viewModel.connectionStatus.collectAsState()
    val deckState by viewModel.deckState.collectAsState()
    val hosts by viewModel.hosts.collectAsState()
    val discovered by viewModel.discovered.collectAsState()
    var screen by rememberSaveable { mutableStateOf(Screen.Deck) }

    LaunchedEffect(Unit) {
        viewModel.uiEvents.collect { e ->
            screen = when (e) {
                UiEvent.ShowDeck -> Screen.Deck
                UiEvent.ShowPcs -> Screen.Pcs
            }
        }
    }

    var flashes by remember { mutableStateOf<Map<String, Flash>>(emptyMap()) }
    var flashSeq by remember { mutableStateOf(0L) }
    val latestDeck by rememberUpdatedState(deckState)
    val railNow by viewModel.railControls.collectAsState()
    val latestRail by rememberUpdatedState(railNow)

    LaunchedEffect(Unit) {
        viewModel.feedbackEvents.collect { feedback ->
            when (feedback.kind) {
                FeedbackKind.FEEDBACK_KIND_HAPTIC -> Haptics.vibrateClick(context)
                FeedbackKind.FEEDBACK_KIND_SHOW_OK,
                FeedbackKind.FEEDBACK_KIND_SHOW_ALERT -> {
                    val next = FeedbackFlashes.apply(
                        flashes, feedback, latestDeck.controls.keys + latestRail.map { it.id }, ++flashSeq
                    )
                    if (next !== flashes) {
                        flashes = next
                        if (feedback.kind == FeedbackKind.FEEDBACK_KIND_SHOW_ALERT) {
                            Haptics.vibrateAlert(context)
                        } else {
                            Haptics.vibrateClick(context)
                        }
                    }
                }
                else -> Unit
            }
        }
    }

    val isConnected = connectionStatus is ConnectionStatus.Connected

    // Deck sleep: idle timer, inhibitors, wake reasons.
    val sleepConfig by viewModel.sleepConfig.collectAsState()
    val cameraFullScreenNow by viewModel.cameraFullScreen.collectAsState()
    val cameraUiNow by viewModel.cameraUi.collectAsState()
    val micUiNow by viewModel.micUi.collectAsState()
    val playingNow by viewModel.playRequest.collectAsState()
    var tilePreviews by remember { mutableStateOf(emptySet<Any>()) }
    var notificationPromptShowing by remember { mutableStateOf(false) }
    val sleepInhibited = !isConnected || cameraFullScreenNow || tilePreviews.isNotEmpty() || playingNow != null ||
        cameraUiNow.permissionNeeded || micUiNow.permissionNeeded || notificationPromptShowing || screen != Screen.Deck
    val sleepTimer = remember { SleepTimer(android.os.SystemClock::elapsedRealtime) }
    var asleep by remember { mutableStateOf(false) }
    val latestSleepConfig by rememberUpdatedState(sleepConfig)
    LaunchedEffect(sleepInhibited, sleepConfig, asleep) {
        if (asleep) {
            // Settings turned sleep off or something needs attention (overlay, player, dialog): wake.
            if (!SleepLogic.maySleep(sleepConfig, sleepInhibited)) { asleep = false; sleepTimer.touch() }
            return@LaunchedEffect
        }
        sleepTimer.touch() // the timer restarts when a setting or an inhibitor changes
        while (true) {
            val wait = sleepTimer.remainingMs(sleepConfig, sleepInhibited) ?: return@LaunchedEffect
            if (wait <= 0L) { asleep = true; return@LaunchedEffect }
            kotlinx.coroutines.delay(wait)
        }
    }
    LaunchedEffect(Unit) {
        viewModel.wakeEvents.collect { reason ->
            if (reason == WakeReason.Player || latestSleepConfig?.wakeOnCameraMic == true) {
                asleep = false
                sleepTimer.touch()
            }
        }
    }
    val lifecycle = androidx.lifecycle.compose.LocalLifecycleOwner.current.lifecycle
    DisposableEffect(lifecycle) {
        val obs = androidx.lifecycle.LifecycleEventObserver { _, e ->
            if (e == androidx.lifecycle.Lifecycle.Event.ON_STOP) { asleep = false; sleepTimer.touch() }
        }
        lifecycle.addObserver(obs)
        onDispose { lifecycle.removeObserver(obs) }
    }
    SleepBrightness(asleep)
    val dim by animateFloatAsState(if (isConnected) 1f else 0.3f, tween(420), label = "deckDim")

    Box(
        modifier = modifier
            .fillMaxSize()
            .background(NdColors.Base)
            // Watches every touch (before the children, never consuming) to restart the idle timer.
            .pointerInput(Unit) {
                awaitPointerEventScope {
                    while (true) {
                        val event = awaitPointerEvent(PointerEventPass.Initial)
                        if (event.changes.any { it.pressed || it.previousPressed }) sleepTimer.touch()
                    }
                }
            }
    ) {
        val navUi by viewModel.navUi.collectAsState()
        val railControls by viewModel.railControls.collectAsState()
        val cameraUi by viewModel.cameraUi.collectAsState()
        val micUi by viewModel.micUi.collectAsState()
        val cameraFullScreen by viewModel.cameraFullScreen.collectAsState()
        val cameraHost = remember(cameraFullScreen) {
            CameraHost(
                viewModel.cameraPreview, cameraFullScreen,
                viewModel::openCameraFullScreen, viewModel::cameraPermissionResult,
                tilePreview = { owner, on -> tilePreviews = if (on) tilePreviews + owner else tilePreviews - owner }
            )
        }
        CompositionLocalProvider(LocalCameraHost provides cameraHost) {
        val showRail = isConnected || deckState.controls.isNotEmpty()
        Row(Modifier.fillMaxSize()) {
        if (showRail) {
            NavRail(
                nav = navUi,
                enabled = isConnected,
                onAction = viewModel::sendNav,
                modifier = Modifier.alpha(dim),
                railControls = railControls,
                icons = icons,
                flashes = flashes,
                onRailInput = { controlId, kind, delta, value ->
                    viewModel.sendInput(controlId, kind, delta, value, rail = true)
                },
                link = (connectionStatus as? ConnectionStatus.Connected)?.transport,
                cameraLive = cameraUi.streaming,
                micLive = micUi.streaming,
                onCameraChip = viewModel::openCameraFullScreen
            )
        }
        DeckGrid(
            cols = deckState.cols,
            rows = deckState.rows,
            controls = deckState.controls,
            enabled = isConnected,
            icons = icons,
            flashes = flashes,
            pageIndex = navUi.pageIndex,
            pageCount = navUi.pageCount,
            onGoToPage = viewModel::sendGoToPage,
            onInput = { controlId, kind, delta, value ->
                viewModel.sendInput(controlId = controlId, kind = kind, delta = delta, value = value)
            },
            modifier = Modifier.weight(1f).alpha(dim)
        )
        }
        }

        ConnectionOverlay(
            status = connectionStatus,
            hasLayout = deckState.controls.isNotEmpty(),
            onOpenPcs = { screen = Screen.Pcs },
            onScanQr = { screen = Screen.Scan }
        )

        if (cameraFullScreen) {
            CameraFullScreen(
                preview = viewModel.cameraPreview,
                onPermissionResult = viewModel::cameraPermissionResult,
                onClose = viewModel::closeCameraFullScreen
            )
        }

        if (cameraUi.permissionNeeded) {
            CameraPermissionCard(
                onResult = viewModel::cameraPermissionResult,
                modifier = Modifier.align(Alignment.Center)
            )
        }

        MicPermissionRequest(micUi.permissionNeeded, viewModel::micPermissionResult)

        NotificationPrompt(isConnected, Modifier.align(Alignment.Center), onShowing = { notificationPromptShowing = it })

        val playRequest by viewModel.playRequest.collectAsState()
        playRequest?.let { StreamPlayer(it, onClose = viewModel::closePlayer, hostClockOffsetMs = viewModel::hostClockOffsetMs) }

        when (screen) {
            Screen.Pcs -> PcsScreen(
                hosts = hosts,
                discovered = discovered,
                onForget = viewModel::forget,
                onScan = { screen = Screen.Scan },
                onBack = { screen = Screen.Deck }
            )
            Screen.Scan -> ScannerScreen(
                onCode = viewModel::startPairing,
                onBack = { screen = Screen.Pcs }
            )
            Screen.Deck -> Unit
        }

        SleepOverlay(
            asleep = asleep,
            showClock = sleepConfig?.showClock == true,
            onWake = { asleep = false; sleepTimer.touch() }
        )
    }
}

/** "● CAMERA" tag shown while the phone streams its camera to the PC. */
@Composable
fun CameraIndicator(modifier: Modifier = Modifier) {
    Row(
        modifier
            .background(NdColors.Base.copy(alpha = 0.85f), NdShapes.Pill)
            .border(1.dp, NdColors.Alert.copy(alpha = 0.6f), NdShapes.Pill)
            .padding(horizontal = 10.dp, vertical = 5.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Box(Modifier.size(7.dp).background(NdColors.Alert, CircleShape))
        Spacer(Modifier.width(6.dp))
        Text("Camera", style = NdType.Micro, color = NdColors.Text)
    }
}

/**
 * Shows the system RECORD_AUDIO dialog once when the PC's microphone request needs it. [needed] is set
 * by the mic controller only while the app is visible, and cleared by [onResult].
 */
@Composable
fun MicPermissionRequest(needed: Boolean, onResult: (Boolean) -> Unit) {
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { onResult(it) }
    LaunchedEffect(needed) { if (needed) launcher.launch(Manifest.permission.RECORD_AUDIO) }
}

/** Asks for the CAMERA permission the PC's webcam request needs; the deck stays usable around it. */
@Composable
fun CameraPermissionCard(onResult: (Boolean) -> Unit, modifier: Modifier = Modifier) {
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { onResult(it) }
    Column(
        modifier
            .padding(28.dp)
            .background(NdColors.Panel, NdShapes.Panel)
            .border(1.dp, NdColors.Hairline, NdShapes.Panel)
            .padding(18.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp)
    ) {
        Text("Webcam needs the camera", style = NdType.Label, color = NdColors.Text)
        Text(
            "Your PC asked to use this phone as a webcam. The video goes only to the PC you paired with.",
            style = NdType.Micro, color = NdColors.TextDim
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            NdButton("Allow camera", { launcher.launch(Manifest.permission.CAMERA) }, primary = true)
            NdButton("Not now", { onResult(false) }, primary = false)
        }
    }
}

/**
 * Android 13+: asks once, after the first successful connection, for the notification permission the
 * "connected" / "camera in use" notification needs. Never shown again whatever the answer.
 */
@Composable
fun NotificationPrompt(connected: Boolean, modifier: Modifier = Modifier, onShowing: (Boolean) -> Unit = {}) {
    if (android.os.Build.VERSION.SDK_INT < 33) return
    val context = LocalContext.current
    val prefs = remember { context.getSharedPreferences("notideck_ui", android.content.Context.MODE_PRIVATE) }
    var show by remember { mutableStateOf(false) }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { show = false }
    LaunchedEffect(connected) {
        if (!connected) return@LaunchedEffect
        val granted = androidx.core.content.ContextCompat.checkSelfPermission(
            context, Manifest.permission.POST_NOTIFICATIONS
        ) == android.content.pm.PackageManager.PERMISSION_GRANTED
        if (!granted && !prefs.getBoolean("notif_asked", false)) {
            prefs.edit().putBoolean("notif_asked", true).apply()
            show = true
        }
    }
    LaunchedEffect(show) { onShowing(show) }
    if (!show) return
    Column(
        modifier
            .padding(28.dp)
            .background(NdColors.Panel, NdShapes.Panel)
            .border(1.dp, NdColors.Hairline, NdShapes.Panel)
            .padding(18.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp)
    ) {
        Text("Show a connection notice?", style = NdType.Label, color = NdColors.Text)
        Text(
            "Notideck keeps running in the background so the webcam can start with the screen off. Android shows a small notification for that.",
            style = NdType.Micro, color = NdColors.TextDim
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            NdButton("Allow", { launcher.launch(Manifest.permission.POST_NOTIFICATIONS) }, primary = true)
            NdButton("Not now", { show = false }, primary = false)
        }
    }
}
