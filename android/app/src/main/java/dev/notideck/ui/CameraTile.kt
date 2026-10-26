// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import android.Manifest
import android.graphics.SurfaceTexture
import android.view.Surface
import android.view.TextureView
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.notideck.camera.CameraPreview
import dev.notideck.camera.PreviewState
import dev.notideck.net.Flash
import dev.notideck.proto.v1.Control
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdShapes
import dev.notideck.ui.theme.NdType
import kotlinx.coroutines.withTimeoutOrNull

private const val CAMERA_LONG_PRESS_MS = 500L

/** What the camera tiles and the full-screen view talk to; provided by DeckScreen. Null = no preview (previews/tests). */
internal class CameraHost(
    val preview: CameraPreview,
    /** The full-screen view is open: tiles drop their inline preview. */
    val fullScreen: Boolean,
    val openFullScreen: () -> Unit,
    val permissionResult: (Boolean) -> Unit,
    /** A tile turned its inline preview on / off (keeps the deck from sleeping); [owner] identifies the tile. */
    val tilePreview: (owner: Any, on: Boolean) -> Unit = { _, _ -> }
)

internal val LocalCameraHost = staticCompositionLocalOf<CameraHost?> { null }

/** What a camera tile shows; pure, for tests. */
internal data class CameraTileFace(
    /** The TextureView is in the tree and attached to the preview. */
    val surface: Boolean,
    val needsPermission: Boolean,
    val error: String,
    /** Red "PC" pill: the PC is receiving the camera. */
    val pcPill: Boolean,
    val showHint: Boolean
)

internal fun cameraTileFace(previewOn: Boolean, fullScreenOpen: Boolean, st: PreviewState): CameraTileFace {
    val live = previewOn && !fullScreenOpen
    return CameraTileFace(
        surface = live,
        needsPermission = live && st.permissionNeeded,
        error = if (live && !st.permissionNeeded) st.error else "",
        pcPill = st.streamingToPc,
        showHint = !live
    )
}

/** Size of the frame fitted (not cropped) into a container; both in the same unit. */
internal data class Fit(val width: Float, val height: Float)

/** Letterbox math for the full-screen view: fit [frameW] x [frameH] (fallback 16:9 when unknown) into the container. */
internal fun fitLetterbox(containerW: Float, containerH: Float, frameW: Int, frameH: Int): Fit {
    if (containerW <= 0f || containerH <= 0f) return Fit(0f, 0f)
    val aspect = if (frameW > 0 && frameH > 0) frameW.toFloat() / frameH else 16f / 9f
    val byWidthH = containerW / aspect
    return if (byWidthH <= containerH) Fit(containerW, byWidthH) else Fit(containerH * aspect, containerH)
}

/** Holds a TextureView's surface and keeps the [preview] attachment of [owner] in step with it. */
private class SurfaceBinding(val preview: CameraPreview, val owner: Any) {
    var surface: Surface? = null
    var w = 0
    var h = 0

    fun available(st: SurfaceTexture, width: Int, height: Int) {
        surface?.release()
        surface = Surface(st)
        w = width; h = height
        attach()
    }

    fun resized(width: Int, height: Int) {
        w = width; h = height
        if (surface != null) attach()
    }

    fun attach() {
        surface?.let { preview.attach(owner, it, w, h) }
    }

    fun destroyed() {
        val s = surface ?: return
        preview.detach(owner)
        s.release()
        surface = null
    }
}

/** A TextureView that shows the camera preview for [owner]; re-attaches when [reattachKey] changes. */
@Composable
internal fun PreviewSurface(preview: CameraPreview, owner: Any, reattachKey: Any?, modifier: Modifier = Modifier) {
    val binding = remember(preview, owner) { SurfaceBinding(preview, owner) }
    AndroidView(
        factory = { ctx ->
            TextureView(ctx).apply {
                surfaceTextureListener = object : TextureView.SurfaceTextureListener {
                    override fun onSurfaceTextureAvailable(st: SurfaceTexture, width: Int, height: Int) =
                        binding.available(st, width, height)
                    override fun onSurfaceTextureSizeChanged(st: SurfaceTexture, width: Int, height: Int) =
                        binding.resized(width, height)
                    override fun onSurfaceTextureDestroyed(st: SurfaceTexture): Boolean {
                        binding.destroyed()
                        return true
                    }
                    override fun onSurfaceTextureUpdated(st: SurfaceTexture) {}
                }
            }
        },
        onRelease = { binding.destroyed() },
        modifier = modifier
    )
    DisposableEffect(binding) { onDispose { binding.destroyed() } }
    LaunchedEffect(reattachKey) { binding.attach() }
}

/** Launches the system CAMERA request; the result goes to [CameraHost.permissionResult]. */
@Composable
private fun rememberCameraPermissionRequest(host: CameraHost?): () -> Unit {
    val latest by rememberUpdatedState(host)
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { latest?.permissionResult?.invoke(it) }
    return remember(launcher) { { launcher.launch(Manifest.permission.CAMERA) } }
}

/**
 * Camera tile (CONTROL_FACE_CAMERA): tap toggles the live preview inside the tile, long press (500 ms)
 * opens the full-screen preview. Handled on the phone; no InputEvent is sent for it.
 */
@Composable
internal fun CameraTile(
    control: Control,
    enabled: Boolean,
    tint: Color?,
    flash: Flash?,
    modifier: Modifier
) {
    val context = LocalContext.current
    val swipeGate = LocalSwipeGate.current
    val host = LocalCameraHost.current
    val st by (host?.preview?.state ?: remember { kotlinx.coroutines.flow.MutableStateFlow(PreviewState()) }).collectAsState()
    var previewOn by remember { mutableStateOf(false) }
    var pressed by remember { mutableStateOf(false) }
    val owner = remember { Any() }
    val face = cameraTileFace(previewOn, host?.fullScreen == true, st)
    val requestPermission = rememberCameraPermissionRequest(host)
    val latestPermission by rememberUpdatedState(st.permissionNeeded && previewOn)
    val latestRequest by rememberUpdatedState(requestPermission)
    val latestHost by rememberUpdatedState(host)

    // An inline preview keeps the deck awake.
    DisposableEffect(previewOn) {
        latestHost?.tilePreview?.invoke(owner, previewOn)
        onDispose { latestHost?.tilePreview?.invoke(owner, false) }
    }

    // The preview does not outlive the app being visible.
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    DisposableEffect(lifecycle) {
        val obs = LifecycleEventObserver { _, e -> if (e == Lifecycle.Event.ON_STOP) previewOn = false }
        lifecycle.addObserver(obs)
        onDispose { lifecycle.removeObserver(obs) }
    }

    KeyCap(
        pressed = pressed,
        lit = false,
        tint = tint,
        flash = flash,
        modifier = modifier
            .semantics { contentDescription = control.title.ifEmpty { "Camera" } + ", camera preview" }
            .then(
                if (!enabled || host == null) Modifier else Modifier.pointerInput(control.id) {
                    awaitEachGesture {
                        val down = awaitFirstDown(requireUnconsumed = false)
                        pressed = true
                        Haptics.vibrateClick(context)
                        val outcome = awaitPressGate(down, swipeGate, viewConfiguration.touchSlop)
                        if (outcome is PressOutcome.Cancelled) {
                            pressed = false
                            return@awaitEachGesture
                        }
                        val fire = outcome as PressOutcome.Fire
                        var longPress = false
                        if (!fire.released) {
                            swipeGate.keyHeld = swipeGate.available
                            var finished = false
                            withTimeoutOrNull((CAMERA_LONG_PRESS_MS - fire.elapsedMs).coerceAtLeast(1L)) {
                                waitForUpOrCancellation()
                                finished = true
                            }
                            if (!finished) {
                                longPress = true
                                latestHost?.openFullScreen?.invoke()
                                waitForUpOrCancellation()
                            }
                            swipeGate.keyHeld = false
                        }
                        pressed = false
                        if (!longPress) {
                            if (latestPermission) latestRequest() else previewOn = !previewOn
                        }
                    }
                }
            )
    ) {
        if (face.surface && host != null) {
            PreviewSurface(host.preview, owner, st.permissionNeeded, Modifier.fillMaxSize())
        }
        if (face.needsPermission || face.error.isNotEmpty()) {
            Column(
                Modifier.fillMaxSize().background(NdColors.Panel.copy(alpha = 0.92f)).padding(8.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center
            ) {
                if (face.needsPermission) {
                    Box(
                        Modifier.background(NdColors.Accent, NdShapes.Pill).padding(horizontal = 12.dp, vertical = 6.dp)
                    ) { Text("Allow camera", style = NdType.Label, color = NdColors.OnAccent, maxLines = 1) }
                } else {
                    Text(
                        face.error, style = NdType.Micro, color = NdColors.Text, textAlign = TextAlign.Center,
                        maxLines = 4, overflow = TextOverflow.Ellipsis
                    )
                }
            }
        }
        if (face.showHint) {
            BoxWithConstraints(Modifier.fillMaxSize()) {
                val roomy = maxHeight >= 72.dp
                Column(
                    Modifier.fillMaxSize().padding(horizontal = 6.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.Center
                ) {
                    Canvas(Modifier.size(if (roomy) 32.dp else 24.dp)) { drawCameraGlyph(NdColors.Text) }
                    if (roomy) {
                        Spacer(Modifier.size(4.dp))
                        Text(
                            control.title.ifEmpty { "Camera" }, style = NdType.KeyTitle, color = NdColors.Text,
                            maxLines = 1, overflow = TextOverflow.Ellipsis
                        )
                        Text(
                            "Tap to preview", style = NdType.Micro.copy(fontSize = 10.sp, lineHeight = 12.sp),
                            color = NdColors.TextDim, maxLines = 1
                        )
                    }
                }
            }
        }
        if (face.pcPill) {
            RedPill("PC", Modifier.align(Alignment.TopStart).padding(start = 7.dp, top = 7.dp), compact = true)
        }
    }
}

/** Small red pill with a white dot and [text]. */
@Composable
internal fun RedPill(text: String, modifier: Modifier = Modifier, compact: Boolean = false) {
    Row(
        modifier
            .background(LiveRed, NdShapes.Pill)
            .padding(horizontal = if (compact) 6.dp else 10.dp, vertical = if (compact) 1.dp else 4.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Box(Modifier.size(if (compact) 4.dp else 6.dp).background(Color.White, CircleShape))
        Spacer(Modifier.width(if (compact) 3.dp else 5.dp))
        Text(
            text,
            style = NdType.Micro.copy(
                fontSize = if (compact) 9.sp else 12.sp,
                lineHeight = if (compact) 11.sp else 14.sp,
                letterSpacing = 0.6.sp
            ),
            color = Color.White,
            maxLines = 1
        )
    }
}

/**
 * Full-screen camera preview over the deck: black background, frame fitted (letterboxed) to the PC's output
 * size, close button top-right, "Live on PC" pill while streaming. Own preview owner; Back and the app
 * leaving the foreground close it.
 */
@Composable
internal fun CameraFullScreen(
    preview: CameraPreview,
    onPermissionResult: (Boolean) -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier
) {
    val st by preview.state.collectAsState()
    val owner = remember { Any() }
    val host = remember(preview, onPermissionResult) { CameraHost(preview, true, {}, onPermissionResult) }
    val requestPermission = rememberCameraPermissionRequest(host)

    BackHandler(onBack = onClose)
    // Keep the screen on while the view is open (the activity also does; this survives a change there).
    val view = LocalView.current
    DisposableEffect(view) {
        val before = view.keepScreenOn
        view.keepScreenOn = true
        onDispose { view.keepScreenOn = before }
    }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val latestClose by rememberUpdatedState(onClose)
    DisposableEffect(lifecycle) {
        val obs = LifecycleEventObserver { _, e -> if (e == Lifecycle.Event.ON_STOP) latestClose() }
        lifecycle.addObserver(obs)
        onDispose { lifecycle.removeObserver(obs) }
    }

    Box(
        modifier.fillMaxSize().background(Color.Black)
            // Swallow touches so nothing reaches the deck underneath.
            .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null) {}
    ) {
        BoxWithConstraints(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            val density = LocalDensity.current
            val fit = fitLetterbox(
                with(density) { maxWidth.toPx() }, with(density) { maxHeight.toPx() }, st.frameWidth, st.frameHeight
            )
            PreviewSurface(
                preview, owner, st.permissionNeeded,
                Modifier.size(with(density) { fit.width.toDp() }, with(density) { fit.height.toDp() })
            )
        }
        if (st.permissionNeeded || st.error.isNotEmpty()) {
            Column(
                Modifier.align(Alignment.Center).padding(32.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(12.dp)
            ) {
                if (st.permissionNeeded) {
                    NdButton("Allow camera", requestPermission, primary = true)
                } else {
                    Text(st.error, style = NdType.Label, color = Color.White, textAlign = TextAlign.Center)
                }
            }
        }
        Row(
            Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.displayCutout).padding(12.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.Top
        ) {
            if (st.streamingToPc) RedPill("Live on PC", Modifier.padding(top = 12.dp)) else Spacer(Modifier.size(1.dp))
            Box(
                Modifier.size(48.dp).background(Color.Black.copy(alpha = 0.55f), CircleShape)
                    .semantics { contentDescription = "Close camera preview" }
                    .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null, onClick = onClose),
                contentAlignment = Alignment.Center
            ) {
                Canvas(Modifier.size(20.dp)) {
                    val sw = 2.dp.toPx()
                    drawLine(Color.White, Offset(0f, 0f), Offset(size.width, size.height), sw, StrokeCap.Round)
                    drawLine(Color.White, Offset(size.width, 0f), Offset(0f, size.height), sw, StrokeCap.Round)
                }
            }
        }
    }
}
