// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.ImageBitmap
import kotlinx.coroutines.delay
import dev.notideck.net.Flash
import dev.notideck.net.Transport
import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.InputKind
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.notideck.net.NavUi
import dev.notideck.proto.v1.NavAction
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdType
import kotlinx.coroutines.withTimeoutOrNull

private const val NAV_LONG_PRESS_MS = 500L
val NavRailWidth = 56.dp

/** What a rail press resolves to: tap (released inside the window) or long press. */
internal enum class NavPress { Tap, Long }

/**
 * Built-in navigation rail, top to bottom: Home (pause badge when auto-switch is paused), camera / mic
 * in-use chips, up to three pinned keys, Back, link status and a clock. What fits is decided by
 * [planRail]. The camera chip opens the full-screen preview via [onCameraChip]; the mic chip is display only.
 */
@Composable
fun NavRail(
    nav: NavUi,
    enabled: Boolean,
    onAction: (NavAction) -> Unit,
    modifier: Modifier = Modifier,
    railControls: List<Control> = emptyList(),
    icons: Map<String, ImageBitmap> = emptyMap(),
    flashes: Map<String, Flash> = emptyMap(),
    onRailInput: (controlId: String, kind: InputKind, delta: Int, value: Float) -> Unit = { _, _, _, _ -> },
    link: Transport? = null,
    cameraLive: Boolean = false,
    micLive: Boolean = false,
    onCameraChip: () -> Unit = {}
) {
    val context = LocalContext.current
    Box(
        modifier
            .fillMaxHeight()
            .windowInsetsPadding(WindowInsets.displayCutout.only(WindowInsetsSides.Start))
    ) {
        BoxWithConstraints(
            Modifier.width(NavRailWidth).fillMaxHeight()
                .background(NdColors.Panel, RoundedCornerShape(topEnd = 20.dp, bottomEnd = 20.dp))
        ) {
            val plan = planRail(
                availableDp = maxHeight.value,
                pinned = railControls.size,
                canGoBack = nav.canGoBack,
                hasLink = link != null,
                cameraLive = cameraLive,
                micLive = micLive
            )
            Column(
                Modifier.fillMaxHeight().width(NavRailWidth - 1.dp).padding(vertical = RailDims.PAD.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Top
            ) {
                RailButton(
                    label = if (nav.paused) "Home (auto-switch paused)" else "Home",
                    enabled = enabled, selected = nav.onHome, badgePaused = nav.paused,
                    onTap = { onAction(NavAction.NAV_ACTION_HOME) },
                    onLong = {
                        Haptics.vibrateAlert(context)
                        onAction(NavAction.NAV_ACTION_HOME_PAUSE)
                    }
                ) { c -> drawHome(c) }

                if (plan.showCameraChip) {
                    Spacer(Modifier.height(RailDims.GROUP_GAP.dp))
                    InUseChip(InUseKind.Camera, onClick = onCameraChip)
                }
                if (plan.showMicChip) {
                    Spacer(Modifier.height((if (plan.showCameraChip) RailDims.CHIP_GAP else RailDims.GROUP_GAP).dp))
                    InUseChip(InUseKind.Mic, onClick = null)
                }

                railControls.take(plan.pinnedShown).forEachIndexed { i, control ->
                    Spacer(Modifier.height((if (i == 0) RailDims.GROUP_GAP else RailDims.KEY_GAP).dp))
                    Box(Modifier.size(RailDims.KEY.dp)) {
                        DeckControlItem(
                            control = control,
                            enabled = enabled,
                            icons = icons,
                            flash = flashes[control.id],
                            compact = true,
                            onInput = { kind, delta, value -> onRailInput(control.id, kind, delta, value) }
                        )
                    }
                }

                if (plan.showBack) {
                    Spacer(Modifier.height(RailDims.GROUP_GAP.dp))
                    RailButton(
                        label = "Back", enabled = enabled, selected = false,
                        onTap = { onAction(NavAction.NAV_ACTION_BACK) }
                    ) { c -> drawBack(c) }
                }

                Spacer(Modifier.weight(1f))

                if (plan.showStatus) {
                    if (link != null) {
                        Canvas(
                            Modifier.size(RailDims.LINK.dp).semantics {
                                contentDescription = if (link == Transport.USB) "USB link" else "Wi-Fi link"
                            }
                        ) { if (link == Transport.USB) drawUsb(NdColors.TextDim) else drawWifi(NdColors.TextDim) }
                    }
                }
                if (plan.showClock) {
                    Text(
                        rememberClockText(), style = NdType.Micro.copy(fontSize = 10.sp),
                        color = NdColors.TextDim, maxLines = 1,
                        modifier = Modifier.padding(top = RailDims.GROUP_GAP.dp)
                    )
                }
            }
        }
    }
}

/** HH:mm or h:mm per the system 12/24 h setting, refreshed on each minute boundary. */
@Composable
private fun rememberClockText(): String {
    val context = LocalContext.current
    val is24 = remember { android.text.format.DateFormat.is24HourFormat(context) }
    val fmt = remember(is24) {
        java.text.SimpleDateFormat(if (is24) "HH:mm" else "h:mm", java.util.Locale.getDefault())
    }
    var now by remember { mutableStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            now = System.currentTimeMillis()
            delay(60_000L - now % 60_000L)
        }
    }
    return fmt.format(java.util.Date(now))
}

@Composable
private fun RailButton(
    label: String,
    enabled: Boolean,
    selected: Boolean,
    onTap: () -> Unit,
    onLong: (() -> Unit)? = null,
    badgePaused: Boolean = false,
    glyph: DrawScope.(Color) -> Unit
) {
    val context = LocalContext.current
    val latestTap by rememberUpdatedState(onTap)
    val latestLong by rememberUpdatedState(onLong)
    var pressed by remember { mutableStateOf(false) }
    val amt by animateFloatAsState(if (pressed) 1f else 0f, tween(if (pressed) 60 else 240), label = "railPress")
    val color = if (selected || pressed) NdColors.AccentInk else NdColors.TextDim
    val shape = RoundedCornerShape(18.dp)

    Column(
        Modifier
            .size(48.dp)
            .clip(shape)
            .background(
                if (selected) NdColors.Accent.copy(alpha = 0.24f + 0.10f * amt)
                else NdColors.Hairline.copy(alpha = 0.5f * amt)
            )
            .semantics { contentDescription = label }
            .then(
                if (!enabled) Modifier else Modifier.pointerInput(label) {
                    awaitEachGesture {
                        awaitFirstDown(requireUnconsumed = false)
                        pressed = true
                        Haptics.vibrateClick(context)
                        var finished = false
                        withTimeoutOrNull(NAV_LONG_PRESS_MS) {
                            waitForUpOrCancellation()
                            finished = true
                        }
                        val press = classifyNavPress(finished)
                        if (press == NavPress.Long && latestLong != null) {
                            latestLong?.invoke()
                            waitForUpOrCancellation()
                        } else if (press == NavPress.Long) {
                            waitForUpOrCancellation()
                            latestTap()
                        } else {
                            latestTap()
                        }
                        pressed = false
                    }
                }
            ),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center
    ) {
        Box(Modifier.size(48.dp), contentAlignment = Alignment.Center) {
            Canvas(Modifier.size(24.dp)) { glyph(color) }
            if (badgePaused) {
                Canvas(Modifier.align(Alignment.TopEnd).padding(top = 3.dp, end = 3.dp).size(15.dp)) {
                    drawCircle(NdColors.Accent)
                    val sw = 1.5.dp.toPx()
                    drawLine(NdColors.OnAccent, Offset(size.width * 0.38f, size.height * 0.28f),
                        Offset(size.width * 0.38f, size.height * 0.72f), sw, StrokeCap.Butt)
                    drawLine(NdColors.OnAccent, Offset(size.width * 0.62f, size.height * 0.28f),
                        Offset(size.width * 0.62f, size.height * 0.72f), sw, StrokeCap.Butt)
                }
            }
        }
    }
}

/** Released inside the long-press window = tap; still down at timeout = long press. */
internal fun classifyNavPress(releasedInWindow: Boolean): NavPress =
    if (releasedInWindow) NavPress.Tap else NavPress.Long

private fun DrawScope.stroke() = Stroke(
    width = 2.dp.toPx(), cap = StrokeCap.Round, join = StrokeJoin.Round
)

private fun DrawScope.drawHome(c: Color) {
    val w = size.width; val h = size.height
    val p = Path().apply {
        moveTo(w * 0.08f, h * 0.50f)
        lineTo(w * 0.50f, h * 0.12f)
        lineTo(w * 0.92f, h * 0.50f)
        moveTo(w * 0.20f, h * 0.44f)
        lineTo(w * 0.20f, h * 0.88f)
        lineTo(w * 0.80f, h * 0.88f)
        lineTo(w * 0.80f, h * 0.44f)
    }
    drawPath(p, c, style = stroke())
}

private fun DrawScope.drawBack(c: Color) {
    val w = size.width; val h = size.height
    val p = Path().apply {
        moveTo(w * 0.90f, h * 0.50f)
        lineTo(w * 0.10f, h * 0.50f)
        moveTo(w * 0.42f, h * 0.18f)
        lineTo(w * 0.10f, h * 0.50f)
        lineTo(w * 0.42f, h * 0.82f)
    }
    drawPath(p, c, style = stroke())
}

private fun DrawScope.drawUsb(c: Color) {
    val w = size.width; val h = size.height
    val sw = 1.5.dp.toPx()
    drawLine(c, Offset(w * 0.5f, h * 0.12f), Offset(w * 0.5f, h * 0.80f), sw, StrokeCap.Round)
    drawLine(c, Offset(w * 0.5f, h * 0.55f), Offset(w * 0.22f, h * 0.38f), sw, StrokeCap.Round)
    drawLine(c, Offset(w * 0.5f, h * 0.68f), Offset(w * 0.78f, h * 0.50f), sw, StrokeCap.Round)
    drawCircle(c, w * 0.09f, Offset(w * 0.22f, h * 0.34f))
    drawRect(c, Offset(w * 0.72f, h * 0.38f), Size(w * 0.12f, w * 0.12f))
    drawCircle(c, w * 0.11f, Offset(w * 0.5f, h * 0.86f))
}

private fun DrawScope.drawWifi(c: Color) {
    val w = size.width; val h = size.height
    val sw = 1.5.dp.toPx()
    val cx = w * 0.5f; val cy = h * 0.82f
    for (r in listOf(0.30f, 0.52f, 0.74f)) {
        val rad = w * r
        drawArc(
            c, startAngle = -135f, sweepAngle = 90f, useCenter = false,
            topLeft = Offset(cx - rad, cy - rad), size = Size(rad * 2, rad * 2),
            style = Stroke(width = sw, cap = StrokeCap.Round)
        )
    }
    drawCircle(c, w * 0.07f, Offset(cx, cy))
}
