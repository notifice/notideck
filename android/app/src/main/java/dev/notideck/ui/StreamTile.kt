// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.ColorMatrix
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.notideck.net.Flash
import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.InputKind
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdShapes
import dev.notideck.ui.theme.NdType
import kotlinx.coroutines.withTimeoutOrNull

private const val STREAM_LONG_PRESS_MS = 500L

/** What a stream tile shows for [control]; pure, for tests. */
internal data class StreamFace(val live: Boolean, val greyscale: Boolean, val showPill: Boolean, val plain: Boolean)

internal fun streamFaceFor(control: Control, hasImage: Boolean): StreamFace {
    val live = control.state_index > 0
    return StreamFace(live = live, greyscale = hasImage && !live, showPill = live, plain = !hasImage)
}

private val GreyscaleFilter = ColorFilter.colorMatrix(ColorMatrix().apply { setToSaturation(0f) })

/**
 * Live-stream tile (CONTROL_FACE_STREAM): the still fills the tile under a dark scrim with the channel,
 * stream title and viewer count; a red LIVE pill when live, greyscale when offline.
 * Presses behave like a key: KEY_DOWN / LONG_PRESS (held 500 ms) / KEY_UP.
 */
@Composable
internal fun StreamTile(
    control: Control,
    enabled: Boolean,
    tint: Color?,
    icons: Map<String, ImageBitmap>,
    flash: Flash?,
    onInput: ControlInput,
    modifier: Modifier
) {
    val context = LocalContext.current
    val swipeGate = LocalSwipeGate.current
    var pressed by remember { mutableStateOf(false) }
    val image = if (control.icon_hash.isNotEmpty()) icons[control.icon_hash] else null
    val face = streamFaceFor(control, image != null)

    KeyCap(
        pressed = pressed,
        lit = false,
        // No still: a plain accent-tinted face.
        tint = tint ?: if (face.plain) NdColors.Accent.copy(alpha = if (NdColors.Light) 0.55f else 0.45f) else null,
        flash = flash,
        modifier = modifier.then(
            if (!enabled) Modifier else Modifier.pointerInput(control.id) {
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
                    onInput(InputKind.INPUT_KIND_KEY_DOWN, 0, 0f)
                    if (fire.released) {
                        pressed = false
                        onInput(InputKind.INPUT_KIND_KEY_UP, 0, 0f)
                        return@awaitEachGesture
                    }
                    swipeGate.keyHeld = swipeGate.available
                    var finished = false
                    withTimeoutOrNull((STREAM_LONG_PRESS_MS - fire.elapsedMs).coerceAtLeast(1L)) {
                        waitForUpOrCancellation()
                        finished = true
                    }
                    if (!finished) {
                        onInput(InputKind.INPUT_KIND_LONG_PRESS, 0, 0f)
                        waitForUpOrCancellation()
                    }
                    pressed = false
                    onInput(InputKind.INPUT_KIND_KEY_UP, 0, 0f)
                    swipeGate.keyHeld = false
                }
            }
        )
    ) {
        // Light themes: the caption sits on a rounded plate over the still instead of a dark gradient.
        val plate = image != null && NdColors.Light
        if (image != null) {
            Image(
                bitmap = image, contentDescription = null, contentScale = ContentScale.Crop,
                colorFilter = if (face.greyscale) GreyscaleFilter else null,
                modifier = Modifier.fillMaxSize()
            )
            // Dark themes: dark scrim so the white text reads over any still. Light themes use a plate instead.
            if (!plate) Box(
                Modifier.fillMaxSize().background(
                    Brush.verticalGradient(
                        0.35f to Color.Transparent,
                        1f to Color.Black.copy(alpha = 0.82f)
                    )
                )
            )
        }
        val titleColor = if (image != null && !plate) Color.White else NdColors.Text
        val dimColor = if (image != null && !plate) Color.White.copy(alpha = 0.82f) else NdColors.TextDim

        if (face.showPill) {
            LivePill(Modifier.align(Alignment.TopStart).padding(start = 7.dp, top = 7.dp), compact = true)
        }
        Column(
            Modifier
                .align(Alignment.BottomStart)
                .fillMaxWidth()
                .then(
                    if (plate) Modifier.padding(5.dp)
                        .background(NdColors.KeyFace.copy(alpha = 0.9f), NdShapes.Tag)
                        .border(1.dp, NdColors.Hairline, NdShapes.Tag)
                    else Modifier
                )
                .padding(horizontal = 8.dp, vertical = 6.dp),
            verticalArrangement = Arrangement.spacedBy(1.dp)
        ) {
            Text(
                control.title.ifEmpty { "Stream" },
                color = titleColor,
                style = NdType.KeyTitle.copy(fontSize = 13.sp, lineHeight = 15.sp),
                maxLines = 1, overflow = TextOverflow.Ellipsis
            )
            if (control.subtitle.isNotEmpty()) {
                Text(
                    control.subtitle,
                    color = dimColor,
                    style = NdType.Micro.copy(fontSize = 10.sp, lineHeight = 12.sp),
                    maxLines = 2, overflow = TextOverflow.Ellipsis
                )
            }
            if (control.detail.isNotEmpty()) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    EyeGlyph(dimColor, Modifier.size(width = 12.dp, height = 8.dp))
                    Spacer(Modifier.width(4.dp))
                    Text(
                        control.detail,
                        color = dimColor,
                        style = NdType.Micro.copy(fontSize = 10.sp, lineHeight = 12.sp),
                        maxLines = 1, overflow = TextOverflow.Ellipsis
                    )
                }
            }
        }
    }
}

/** Tiny "viewers" eye: an almond outline with a pupil. */
@Composable
private fun EyeGlyph(color: Color, modifier: Modifier = Modifier) {
    Canvas(modifier) {
        val w = size.width
        val h = size.height
        val path = androidx.compose.ui.graphics.Path().apply {
            moveTo(0f, h / 2f)
            quadraticBezierTo(w / 2f, -h * 0.55f, w, h / 2f)
            quadraticBezierTo(w / 2f, h * 1.55f, 0f, h / 2f)
            close()
        }
        drawPath(path, color, style = Stroke(1.dp.toPx()))
        drawCircle(color, radius = h * 0.22f, center = Offset(w / 2f, h / 2f))
    }
}
