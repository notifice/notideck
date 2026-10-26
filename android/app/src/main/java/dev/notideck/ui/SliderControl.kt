// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.ColorMatrix
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawOutline
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.InputKind
import dev.notideck.ui.FaderGeometry.Zone
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdType
import kotlin.math.min
import kotlin.math.roundToInt

private const val SLIDER_MIN_INTERVAL_MS = 16L      // ~60 Hz
private val ThumbHeight = 24.dp
private val ThumbWidth = 44.dp
private val TrackWidth = 12.dp

private val ReadoutStyle = NdType.Readout.copy(fontSize = 24.sp, lineHeight = 26.sp, textAlign = TextAlign.Center)

/** Channel-strip fader: readout on top, recessed track + LED ladder, machined cap, app badge below. */
@Composable
internal fun SliderControl(
    control: Control,
    enabled: Boolean,
    fg: Color,
    onInput: ControlInput,
    modifier: Modifier,
    meter: State<Float>? = null,
    icons: Map<String, ImageBitmap> = emptyMap()
) {
    val horizontal = control.horizontal
    val muted = control.state_index == 1
    // Dimmed = no app behind this slider (empty mixer slot): no cap, no fill, "–" readout.
    val empty = control.dimmed
    // Hold the previous icon while a new hash is being fetched.
    val resolved = if (control.icon_hash.isNotEmpty()) icons[control.icon_hash] else null
    var held by remember { mutableStateOf<ImageBitmap?>(null) }
    if (control.icon_hash.isEmpty()) held = null else if (resolved != null) held = resolved
    val icon = resolved ?: held

    var touching by remember { mutableStateOf(false) }
    var localValue by remember { mutableFloatStateOf(control.value_.coerceIn(0f, 1f)) }
    val shown = if (touching) localValue else control.value_.coerceIn(0f, 1f)
    val animated by animateFloatAsState(shown, tween(if (touching) 0 else 200), label = "sliderValue")

    val track: @Composable (Modifier) -> Unit = { m ->
        FaderTrack(
            modifier = m, horizontal = horizontal, enabled = enabled, controlId = control.id,
            value = { animated }, meter = meter, muted = muted, empty = empty, touching = touching,
            onTouching = { touching = it }, localValue = { localValue }, setLocal = { localValue = it },
            onInput = onInput
        )
    }
    val readout: @Composable (Modifier) -> Unit = { m ->
        Readout(animated, muted, empty, touching, m)
    }
    val badge: @Composable () -> Unit = {
        SliderAppBadge(icon, control.title, muted, enabled, compact = horizontal, onInput = onInput)
    }

    KeyCap(pressed = touching, lit = false, tint = null, modifier = modifier) {
        if (horizontal) {
            Row(
                Modifier.fillMaxSize().padding(horizontal = 10.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                badge()
                Spacer(Modifier.width(10.dp))
                track(Modifier.weight(1f).fillMaxHeight())
                Spacer(Modifier.width(10.dp))
                readout(Modifier.width(60.dp))
            }
        } else {
            Column(Modifier.fillMaxSize().padding(horizontal = 10.dp, vertical = 8.dp)) {
                readout(Modifier.fillMaxWidth())
                Spacer(Modifier.height(10.dp))
                track(Modifier.weight(1f).fillMaxWidth())
                badge()
            }
        }
    }
}

@Composable
private fun Readout(value: Float, muted: Boolean, empty: Boolean, touching: Boolean, modifier: Modifier) {
    Box(modifier.height(28.dp), contentAlignment = Alignment.Center) {
        if (empty) {
            Text("–", color = NdColors.TextFaint, style = ReadoutStyle, maxLines = 1)
        } else if (muted) {
            Text("Muted", color = NdColors.Alert, style = NdType.Label, maxLines = 1)
        } else {
            Text(
                "${(value * 100).roundToInt()}",
                color = if (touching) NdColors.AccentInk else NdColors.Text,
                style = ReadoutStyle,
                maxLines = 1
            )
        }
    }
}

/** The drag surface: only this box reacts to slider drags. */
@Composable
private fun FaderTrack(
    modifier: Modifier,
    horizontal: Boolean,
    enabled: Boolean,
    controlId: String,
    value: () -> Float,
    meter: State<Float>?,
    muted: Boolean,
    empty: Boolean,
    touching: Boolean,
    onTouching: (Boolean) -> Unit,
    localValue: () -> Float,
    setLocal: (Float) -> Unit,
    onInput: ControlInput
) {
    val haptic = LocalHapticFeedback.current
    Box(
        modifier.pointerInput(controlId, horizontal, enabled) {
            if (!enabled) return@pointerInput
            awaitEachGesture {
                val down = awaitFirstDown(requireUnconsumed = false)
                val thumb = ThumbHeight.toPx()
                fun valueAt(p: Offset): Float {
                    val len = if (horizontal) size.width else size.height
                    if (len <= 0) return localValue()
                    val pos = if (horizontal) p.x else len - p.y
                    return FaderGeometry.valueAt(pos, len.toFloat(), thumb)
                }
                onTouching(true)
                haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                onInput(InputKind.INPUT_KIND_TOUCH_START, 0, 0f)
                setLocal(valueAt(down.position))
                var lastSent = android.os.SystemClock.uptimeMillis()
                onInput(InputKind.INPUT_KIND_SLIDER_VALUE, 0, localValue())
                do {
                    val ev = awaitPointerEvent()
                    val ch = ev.changes.first()
                    setLocal(valueAt(ch.position))
                    ch.consume()
                    val now = android.os.SystemClock.uptimeMillis()
                    if (now - lastSent >= SLIDER_MIN_INTERVAL_MS) {
                        lastSent = now
                        onInput(InputKind.INPUT_KIND_SLIDER_VALUE, 0, localValue())
                    }
                } while (ev.changes.any { it.pressed })
                onInput(InputKind.INPUT_KIND_SLIDER_VALUE, 0, localValue())
                onInput(InputKind.INPUT_KIND_TOUCH_END, 0, 0f)
                onTouching(false)
            }
        }
    ) {
        Canvas(Modifier.fillMaxSize()) {
            // Meter State is read here only, so level ticks redraw without recomposing.
            val level = if (muted) 0f else meter?.value
            val v = value()
            // Horizontal draws the vertical layout rotated 90 degrees clockwise: frame y runs right-to-left.
            if (horizontal) {
                withTransform({
                    translate(left = size.width, top = 0f)
                    rotate(90f, Offset.Zero)
                }) { drawFader(size.height, size.width, v, level, meter != null && !empty, touching, muted, empty) }
            } else {
                drawFader(size.width, size.height, v, level, meter != null && !empty, touching, muted, empty)
            }
        }
    }
}

/**
 * Draws one fader in a vertical frame [frameW] x [frameLen]: low end at the bottom, value 1 at the top.
 * [level] null/hasMeter false means no ladder; the thumb stays inside [0, frameLen] at both extremes.
 */
private fun DrawScope.drawFader(
    frameW: Float, frameLen: Float, value: Float, level: Float?, hasMeter: Boolean,
    touching: Boolean, muted: Boolean, empty: Boolean = false
) {
    val thumbH = ThumbHeight.toPx()
    val thumbW = min(ThumbWidth.toPx(), frameW * 0.62f)
    val trackW = TrackWidth.toPx()
    val ladderW = 7.dp.toPx()
    // Track on the centre line (aligned with readout and badge); the ladder hangs off the right of the cap,
    // with the gap shrinking (8 dp → 3 dp) to fit narrow columns. Only if even that fails, centre the group.
    val room = frameW / 2f - thumbW / 2f - ladderW
    val centred = !hasMeter || room >= 3.dp.toPx()
    val gap = if (centred) min(8.dp.toPx(), room.coerceAtLeast(0f)) else 3.dp.toPx()
    val groupW = if (hasMeter) thumbW + gap + ladderW else thumbW
    val left = if (centred) frameW / 2f - thumbW / 2f else ((frameW - groupW) / 2f).coerceAtLeast(0f)
    val cx = left + thumbW / 2f

    // Recessed channel.
    val tx = cx - trackW / 2f
    val r = CornerRadius(trackW / 2f)
    drawRoundRect(NdColors.Well, Offset(tx, 0f), Size(trackW, frameLen), r)
    drawRoundRect(NdColors.Hairline, Offset(tx, 0f), Size(trackW, frameLen), r, style = Stroke(1.dp.toPx()))
    // inner top shadow for depth
    drawLine(NdColors.Shadow.copy(alpha = if (NdColors.Light) 0.10f else 0.55f), Offset(tx + 3.dp.toPx(), 2.dp.toPx()), Offset(tx + trackW - 3.dp.toPx(), 2.dp.toPx()), 2.dp.toPx(), StrokeCap.Round)

    if (empty) return

    val cy = frameLen - FaderGeometry.thumbCenter(value, frameLen, thumbH)
    val inset = 2.dp.toPx()
    val fillTop = cy.coerceIn(0f, frameLen)
    if (frameLen - fillTop > inset) {
        val fill = if (muted) SolidFill(NdColors.MutedFill, NdColors.MutedFill) else SolidFill(NdColors.Accent, NdColors.AccentDeep)
        drawRoundRect(
            Brush.verticalGradient(listOf(fill.top, fill.bottom), startY = fillTop, endY = frameLen),
            Offset(tx + inset, fillTop), Size(trackW - inset * 2, frameLen - fillTop - inset),
            CornerRadius((trackW - inset * 2) / 2f)
        )
    }

    // LED ladder: segments only light up.
    if (hasMeter) {
        val lx = left + thumbW + gap
        val n = FaderGeometry.ledCount(frameLen, 5.dp.toPx(), 2.dp.toPx())
        val lit = FaderGeometry.ledLit(level ?: 0f, n)
        val stride = frameLen / n
        val segH = stride - 2.dp.toPx()
        for (i in 0 until n) {
            val c = when (FaderGeometry.zone(i, n)) {
                Zone.GREEN -> NdColors.Accent
                Zone.AMBER -> NdColors.Hot
                Zone.RED -> NdColors.Alert
            }
            val y = frameLen - (i + 1) * stride + (stride - segH) / 2f
            drawRoundRect(
                if (i < lit) c else c.copy(alpha = 0.12f),
                Offset(lx, y), Size(ladderW, segH), CornerRadius(2.dp.toPx())
            )
        }
    }

    // Soft pill cap: diffuse shadow, gently graded face, hairline edge, centre line.
    val light = NdColors.Light
    val cr = CornerRadius(thumbH / 2f)
    val top = cy - thumbH / 2f
    val tl = cx - thumbW / 2f
    for (k in 1..4) {
        val g = k * 1.5.dp.toPx()
        drawRoundRect(
            NdColors.Shadow.copy(alpha = if (light) 0.04f else 0.14f),
            Offset(tl - g / 2f, top + g * 0.9f), Size(thumbW + g, thumbH + g), CornerRadius(thumbH / 2f + g / 2f)
        )
    }
    val faceTop = if (light) NdColors.KeyFace else NdColors.Text
    val faceBottom = if (light) NdColors.KeyFacePressed else NdColors.Text.let { Color(it.red * 0.86f, it.green * 0.86f, it.blue * 0.86f, 1f) }
    drawRoundRect(
        Brush.verticalGradient(listOf(faceTop, faceBottom), startY = top, endY = top + thumbH),
        Offset(tl, top), Size(thumbW, thumbH), cr
    )
    if (light) {
        drawRoundRect(NdColors.Hairline, Offset(tl, top), Size(thumbW, thumbH), cr, style = Stroke(1.dp.toPx()))
    }
    val line = when {
        touching && !muted -> NdColors.AccentDeep
        light -> NdColors.TextFaint
        else -> NdColors.Base
    }
    drawLine(line, Offset(tl + 8.dp.toPx(), cy), Offset(tl + thumbW - 8.dp.toPx(), cy), 2.dp.toPx(), StrokeCap.Round)
}

private class SolidFill(val top: Color, val bottom: Color)

/**
 * App icon + name under (or beside, when [compact]) the track. Tapping it sends KEY_DOWN then KEY_UP
 * (toggle mute); it is separate from the drag surface. Also works with a title but no icon.
 */
@Composable
private fun SliderAppBadge(
    icon: ImageBitmap?, name: String, muted: Boolean, enabled: Boolean, compact: Boolean, onInput: ControlInput
) {
    if (icon == null && name.isEmpty()) return
    val haptic = LocalHapticFeedback.current
    var pressed by remember { mutableStateOf(false) }
    val press by animateFloatAsState(if (pressed) 1f else 0f, tween(90), label = "badgePress")
    val shape = androidx.compose.foundation.shape.RoundedCornerShape(14.dp)
    val textColor = if (muted) NdColors.TextDim else NdColors.Text
    val mod = Modifier
        .then(if (compact) Modifier.width(64.dp) else Modifier.fillMaxWidth())
        .graphicsLayer { scaleX = 1f - 0.06f * press; scaleY = 1f - 0.06f * press }
        .drawBehind {
            if (press > 0.01f) {
                drawOutline(shape.createOutline(size, layoutDirection, this), NdColors.Accent.copy(alpha = 0.16f * press))
            }
        }
        .pointerInput(enabled) {
            if (!enabled) return@pointerInput
            detectTapGestures(
                onPress = {
                    pressed = true
                    try { tryAwaitRelease() } finally { pressed = false }
                },
                onTap = {
                    haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                    onInput(InputKind.INPUT_KIND_KEY_DOWN, 0, 0f)
                    onInput(InputKind.INPUT_KIND_KEY_UP, 0, 0f)
                }
            )
        }
        .padding(horizontal = 4.dp, vertical = 4.dp)
    Column(mod, horizontalAlignment = Alignment.CenterHorizontally) {
        if (icon != null) {
            Box(Modifier.size(40.dp)) {
                Crossfade(targetState = icon, animationSpec = tween(180), label = "sliderIcon") { bmp ->
                    Image(
                        bitmap = bmp,
                        contentDescription = name.ifEmpty { null },
                        contentScale = ContentScale.Fit,
                        alpha = if (muted) 0.5f else 1f,
                        colorFilter = if (muted) ColorFilter.colorMatrix(ColorMatrix().apply { setToSaturation(0f) }) else if (rememberIconClass(bmp) == IconClass.GLYPH) ColorFilter.tint(NdColors.Text) else null,
                        modifier = Modifier.fillMaxSize()
                    )
                }
                if (muted) MutedGlyph(Modifier.align(Alignment.BottomEnd).size(18.dp))
            }
        } else if (muted) {
            MutedGlyph(Modifier.size(18.dp))
        }
        if (name.isNotEmpty()) {
            Text(
                name,
                color = textColor,
                style = if (compact) NdType.Micro else NdType.Label,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 4.dp)
            )
        }
    }
}

/** Small speaker with a slash, drawn as vectors; sits on a dark disc so it reads over any icon. */
@Composable
private fun MutedGlyph(modifier: Modifier) {
    Canvas(modifier) {
        val s = size.minDimension
        drawCircle(NdColors.Base, s / 2f)
        drawCircle(NdColors.Alert.copy(alpha = 0.6f), s / 2f - 0.5.dp.toPx(), style = Stroke(1.dp.toPx()))
        val c = NdColors.Alert
        val p = androidx.compose.ui.graphics.Path().apply {
            moveTo(s * 0.26f, s * 0.42f); lineTo(s * 0.40f, s * 0.42f); lineTo(s * 0.55f, s * 0.28f)
            lineTo(s * 0.55f, s * 0.72f); lineTo(s * 0.40f, s * 0.58f); lineTo(s * 0.26f, s * 0.58f); close()
        }
        drawPath(p, c)
        drawLine(c, Offset(s * 0.28f, s * 0.78f), Offset(s * 0.74f, s * 0.22f), 1.6.dp.toPx(), StrokeCap.Round)
    }
}
