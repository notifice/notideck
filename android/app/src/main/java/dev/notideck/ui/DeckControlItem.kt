// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
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
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalViewConfiguration
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.notideck.net.Flash
import dev.notideck.net.FlashKind
import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.ControlKind
import dev.notideck.proto.v1.InputKind
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdShapes
import dev.notideck.ui.theme.NdType
import kotlin.math.PI
import kotlin.math.atan2
import kotlin.math.roundToInt

/** Callback for everything a control can emit: kind, dial detent delta, slider value. */
typealias ControlInput = (kind: InputKind, delta: Int, value: Float) -> Unit

private const val DIMMED_ALPHA = 0.4f
private const val FLASH_MS = 600
private const val LONG_PRESS_MS = 500L
private const val SLIDER_MIN_INTERVAL_MS = 16L      // ~60 Hz
private const val DIAL_DEGREES_PER_DETENT = 12f
private const val DIAL_STEP = 0.02f                 // optimistic local value change per detent

private fun argb(v: Int): Color = Color(v.toLong() and 0xFFFFFFFFL)

/** Renders one deck cell (key, dial, slider or tile). */
@Composable
fun DeckControlItem(
    control: Control,
    enabled: Boolean = true,
    icons: Map<String, ImageBitmap> = emptyMap(),
    flash: Flash? = null,
    onInput: ControlInput,
    modifier: Modifier = Modifier,
    /** Side-rail sizing: icon-first, no title strip, short title as the fallback face. */
    compact: Boolean = false
) {
    val fg = if (control.fg_argb != 0) argb(control.fg_argb) else NdColors.Text
    val tint = if (control.bg_argb != 0) argb(control.bg_argb) else null
    val lit = control.state_index > 0

    // Gesture blocks outlive recompositions; route through the latest callback.
    val latest by rememberUpdatedState(onInput)
    val stable: ControlInput = remember { { k, d, v -> latest(k, d, v) } }

    // Unavailable target: ~40% alpha, input still delivered. Alpha is read in the layer block only.
    val dim by animateFloatAsState(if (control.dimmed) DIMMED_ALPHA else 1f, tween(150), label = "dim")
    val meterLevel = rememberMeterLevel(control.level, control.meter)
    val m: State<Float>? = if (control.meter) meterLevel else null
    val dimMod = modifier.graphicsLayer { alpha = dim }

    when (control.kind) {
        ControlKind.CONTROL_KIND_DIAL -> DialControl(control, enabled, fg, stable, dimMod, m)
        ControlKind.CONTROL_KIND_SLIDER -> SliderControl(control, enabled, fg, stable, dimMod, m, icons)
        ControlKind.CONTROL_KIND_TILE ->
            when (control.face) {
                dev.notideck.proto.v1.ControlFace.CONTROL_FACE_MEDIA ->
                    MediaTile(control, enabled, tint, icons, flash, stable, dimMod)
                dev.notideck.proto.v1.ControlFace.CONTROL_FACE_STREAM ->
                    StreamTile(control, enabled, tint, icons, flash, stable, dimMod)
                dev.notideck.proto.v1.ControlFace.CONTROL_FACE_CAMERA ->
                    CameraTile(control, enabled, tint, flash, dimMod)
                else -> TileControl(control, enabled, fg, tint, icons, flash, stable, dimMod, m)
            }
        else -> KeyControl(control, enabled, fg, tint, lit, icons, flash, stable, dimMod, compact, m)
    }
}

/**
 * Soft key: gently graded face, a faint hairline edge and a low, diffuse shadow underneath that
 * flattens while pressed. Lit keys get a pastel accent wash and edge. [pressed] is driven by the
 * caller's gesture.
 */
@Composable
internal fun KeyCap(
    pressed: Boolean,
    lit: Boolean,
    tint: Color?,
    modifier: Modifier = Modifier,
    flash: Flash? = null,
    compact: Boolean = false,
    content: @Composable androidx.compose.foundation.layout.BoxScope.() -> Unit
) {
    val scale by animateFloatAsState(
        targetValue = if (pressed) 0.96f else 1f,
        animationSpec = spring(dampingRatio = 0.6f, stiffness = Spring.StiffnessMedium),
        label = "capScale"
    )
    val glow by animateFloatAsState(
        targetValue = if (pressed) 1f else 0f,
        animationSpec = tween(if (pressed) 60 else 320),
        label = "capGlow"
    )
    val litAmt by animateFloatAsState(if (lit) 1f else 0f, tween(240), label = "capLit")

    // Host feedback flash (SHOW_OK accent / SHOW_ALERT alert): snap to full, then fade out.
    val flashAmt = remember { Animatable(0f) }
    LaunchedEffect(flash?.seq) {
        if (flash != null) {
            flashAmt.snapTo(1f)
            flashAmt.animateTo(0f, tween(FLASH_MS))
        }
    }
    val flashColor = if (flash?.kind == FlashKind.ALERT) NdColors.Alert else NdColors.Accent

    val faceTop = keyFaceTop(tint)
    val faceBottom = tint?.let { lerp(NdColors.KeyFacePressed, it, 0.22f) } ?: lerp(NdColors.KeyFace, NdColors.KeyFacePressed, 0.55f)
    val edgeBase = lerp(NdColors.Hairline, NdColors.Accent.copy(alpha = 0.85f), maxOf(glow * 0.6f, litAmt * 0.8f))
    val edge = lerp(edgeBase, flashColor, flashAmt.value)
    val pad = if (compact) 2.dp else 4.dp

    Box(
        modifier = modifier
            .fillMaxSize()
            .scale(scale)
            .padding(pad)
            .drawBehind {
                // Diffuse shadow: a few stacked low-alpha rounded rects, offset downwards.
                val a = (if (NdColors.Light) 0.045f else 0.16f) * (1f - 0.7f * glow)
                for (k in 1..3) {
                    val g = k * (if (compact) 1.dp else 1.5.dp).toPx()
                    drawRoundRect(
                        NdColors.Shadow.copy(alpha = a),
                        Offset(-g / 2f, g * 0.45f), Size(size.width + g, size.height + g * 0.8f),
                        CornerRadius(22.dp.toPx() + g / 2f)
                    )
                }
            }
            .clip(NdShapes.Key)
            .drawBehind {
                drawRect(
                    Brush.verticalGradient(
                        listOf(
                            lerp(faceTop, NdColors.KeyFacePressed, glow),
                            lerp(faceBottom, NdColors.KeyFacePressed, glow)
                        )
                    )
                )
                if (litAmt > 0.01f) {
                    drawRect(NdColors.Accent.copy(alpha = (if (NdColors.Light) 0.22f else 0.14f) * litAmt))
                }
                if (flashAmt.value > 0.01f) {
                    drawRect(flashColor.copy(alpha = 0.28f * flashAmt.value))
                }
                // state dot, top-left
                if (litAmt > 0.01f) {
                    drawRoundRect(
                        NdColors.AccentInk.copy(alpha = litAmt),
                        topLeft = Offset((if (compact) 8.dp else 12.dp).toPx(), (if (compact) 6.dp else 10.dp).toPx()),
                        size = Size((if (compact) 8.dp else 12.dp).toPx(), 4.dp.toPx()),
                        cornerRadius = CornerRadius(2.dp.toPx())
                    )
                }
            }
            .border(1.dp, edge, NdShapes.Key)
    ) { content() }
}

@Composable
private fun KeyControl(
    control: Control,
    enabled: Boolean,
    fg: Color,
    tint: Color?,
    lit: Boolean,
    icons: Map<String, ImageBitmap>,
    flash: Flash?,
    onInput: ControlInput,
    modifier: Modifier,
    compact: Boolean = false,
    meter: State<Float>? = null
) {
    val context = LocalContext.current
    val swipeGate = LocalSwipeGate.current
    var pressed by remember { mutableStateOf(false) }

    KeyCap(
        pressed = pressed,
        lit = lit,
        tint = tint,
        flash = flash,
        compact = compact,
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
                    // Long-press timing is measured from touch-down.
                    var finished = false
                    withTimeoutOrNull((LONG_PRESS_MS - fire.elapsedMs).coerceAtLeast(1L)) {
                        waitForUpOrCancellation()
                        finished = true
                    }
                    if (!finished) {
                        // timed out (finger still down) -> long press, then wait for release
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
        if (meter != null) {
            BottomLevelBar(
                meter,
                Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(3.dp)
                    .padding(horizontal = if (compact) 4.dp else 8.dp)
            )
        }
        val ink = keyInk(fg, control.fg_argb != 0, tint)
        val resolved = if (control.icon_hash.isNotEmpty()) icons[control.icon_hash] else null
        // Multi-state keys swap icon_hash per state; hold the previous image until the new one is decoded.
        var held by remember { mutableStateOf<ImageBitmap?>(null) }
        if (control.icon_hash.isEmpty()) held = null else if (resolved != null) held = resolved
        val icon = resolved ?: held
        if (!compact && icon == null && control.title.isNotEmpty()) {
            Text(
                text = control.title.take(1).uppercase(),
                color = NdColors.Text.copy(alpha = 0.05f),
                style = NdType.Display.copy(fontSize = 96.sp, lineHeight = 96.sp),
                modifier = Modifier.align(Alignment.BottomEnd).padding(end = 10.dp, bottom = 0.dp)
            )
        }
        if (icon != null) {
            val iconClass = rememberIconClass(icon)
            val fitted = iconClass != IconClass.ARTWORK
            val hasTitle = !compact && control.title.isNotEmpty()
            if (fitted) {
                // GLYPH / CUTOUT: fitted above the title (or the whole face), never cropped, no scrim.
                Column(
                    Modifier.fillMaxSize().padding(
                        start = if (compact) 6.dp else 10.dp, end = if (compact) 6.dp else 10.dp,
                        top = if (compact) 6.dp else 10.dp, bottom = if (compact) 6.dp else if (hasTitle) 8.dp else 10.dp
                    ),
                    horizontalAlignment = Alignment.CenterHorizontally
                ) {
                    KeyIcon(icon, control.title, ink, Modifier.weight(1f).fillMaxWidth())
                    if (hasTitle) {
                        Text(
                            text = control.title,
                            color = ink,
                            style = NdType.Micro,
                            textAlign = TextAlign.Center,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.padding(top = 4.dp)
                        )
                    }
                }
            } else if (hasTitle) {
                // ARTWORK with a title: the image fills (crops) the face above the title row, so the
                // title never covers the picture (opaque logos were unreadable under a caption).
                Column(Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally) {
                    KeyIcon(icon, control.title, ink, Modifier.weight(1f).fillMaxWidth())
                    Text(
                        text = control.title,
                        color = ink,
                        style = NdType.Micro,
                        textAlign = TextAlign.Center,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.padding(start = 8.dp, end = 8.dp, top = 4.dp, bottom = 8.dp)
                    )
                }
            } else {
                // ARTWORK without a title: the whole key face, edge to edge.
                KeyIcon(icon, control.title, ink, Modifier.fillMaxSize())
            }
        } else if (compact) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text(
                    text = control.title.take(2),
                    color = ink,
                    style = NdType.KeyTitle.copy(fontSize = 16.sp, lineHeight = 18.sp),
                    maxLines = 1
                )
            }
        } else Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(horizontal = 8.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally
        ) {
            if (control.title.isNotEmpty()) {
                Text(
                    text = control.title,
                    color = ink,
                    style = if (icon != null) NdType.Micro else NdType.KeyTitle.copy(
                        fontSize = when {
                            control.title.length <= 6 -> 22.sp
                            control.title.length <= 10 -> 18.sp
                            else -> 15.sp
                        },
                        lineHeight = 22.sp
                    ),
                    textAlign = TextAlign.Center,
                    maxLines = if (icon != null) 1 else 2,
                    overflow = TextOverflow.Ellipsis
                )
            }
            if (control.subtitle.isNotEmpty() && icon == null) {
                Text(
                    text = control.subtitle,
                    color = NdColors.TextDim,
                    style = NdType.Micro,
                    textAlign = TextAlign.Center,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(top = 4.dp)
                )
            }
        }
    }
}

/**
 * A key/tile icon with its own class: GLYPH is tinted with [ink] and fitted, CUTOUT fitted as-is,
 * ARTWORK cropped. Crossfades between multi-state icons.
 */
@Composable
private fun KeyIcon(icon: ImageBitmap, description: String?, ink: Color, modifier: Modifier) {
    Crossfade(targetState = icon, animationSpec = tween(180), label = "keyIcon", modifier = modifier) { bmp ->
        val cls = rememberIconClass(bmp)
        Image(
            bitmap = bmp,
            contentDescription = description?.ifEmpty { null },
            contentScale = if (cls == IconClass.ARTWORK) ContentScale.Crop else ContentScale.Fit,
            colorFilter = if (cls == IconClass.GLYPH) ColorFilter.tint(ink) else null,
            modifier = Modifier.fillMaxSize()
        )
    }
}

@Composable
private fun TileControl(
    control: Control,
    enabled: Boolean,
    fg: Color,
    tint: Color?,
    icons: Map<String, ImageBitmap>,
    flash: Flash?,
    onInput: ControlInput,
    modifier: Modifier,
    meter: State<Float>? = null
) {
    var pressed by remember { mutableStateOf(false) }
    val swipeGate = LocalSwipeGate.current
    val hasGauge = control.value_ > 0f
    val level by animateFloatAsState(control.value_.coerceIn(0f, 1f), tween(300), label = "tileLevel")
    val icon = if (control.icon_hash.isNotEmpty()) icons[control.icon_hash] else null

    KeyCap(
        pressed = pressed,
        lit = false,
        tint = tint,
        flash = flash,
        modifier = modifier.then(
            if (!enabled) Modifier else Modifier.pointerInput(control.id) {
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    pressed = true
                    val outcome = awaitPressGate(down, swipeGate, viewConfiguration.touchSlop)
                    if (outcome is PressOutcome.Cancelled) {
                        pressed = false
                        return@awaitEachGesture
                    }
                    onInput(InputKind.INPUT_KIND_KEY_DOWN, 0, 0f)
                    if (!(outcome as PressOutcome.Fire).released) {
                        swipeGate.keyHeld = swipeGate.available
                        waitForUpOrCancellation()
                    }
                    pressed = false
                    onInput(InputKind.INPUT_KIND_KEY_UP, 0, 0f)
                    swipeGate.keyHeld = false
                }
            }
        )
    ) {
        if (icon != null) {
            if (rememberIconClass(icon) == IconClass.ARTWORK) {
                Image(
                    bitmap = icon, contentDescription = null, contentScale = ContentScale.Crop,
                    modifier = Modifier.fillMaxSize()
                )
            } else {
                KeyIcon(icon, null, keyInk(fg, control.fg_argb != 0, tint), Modifier.fillMaxSize().padding(14.dp))
            }
        }
        if (meter != null) {
            BottomLevelBar(
                meter,
                Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(3.dp).padding(horizontal = 8.dp)
            )
        }
        // An uploaded image is the entire tile face: no placeholder label or text over it.
        if (icon == null) Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(horizontal = 10.dp, vertical = 9.dp),
            verticalArrangement = Arrangement.SpaceBetween
        ) {
            Text(
                text = control.title.ifEmpty { "Tile" },
                color = NdColors.TextDim,
                style = NdType.Micro,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis
            )
            Column {
                if (hasGauge) {
                    Text(
                        text = "${(level * 100).roundToInt()}%",
                        color = fg,
                        style = NdType.Readout,
                        maxLines = 1
                    )
                } else if (control.subtitle.isNotEmpty()) {
                    Text(
                        text = control.subtitle,
                        color = fg,
                        style = NdType.KeyTitle,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis
                    )
                }
                if (hasGauge) {
                    if (control.subtitle.isNotEmpty()) {
                        Text(
                            text = control.subtitle,
                            color = NdColors.TextDim,
                            style = NdType.Micro,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.padding(top = 2.dp)
                        )
                    }
                    SegmentGauge(level, Modifier.padding(top = 6.dp))
                }
            }
        }
    }
}

/** Row of 16 fixed ticks, lit up to [level]. Reads like a VU strip. */
@Composable
private fun SegmentGauge(level: Float, modifier: Modifier = Modifier) {
    Canvas(modifier.fillMaxWidth().height(6.dp)) {
        val n = 16
        val gap = 2.dp.toPx()
        val w = (size.width - gap * (n - 1)) / n
        for (i in 0 until n) {
            val on = (i + 1) / n.toFloat() <= level + 0.001f
            val c = when {
                !on -> NdColors.Track
                i >= n - 3 -> NdColors.Alert
                else -> NdColors.Accent
            }
            drawRoundRect(c, Offset(i * (w + gap), 0f), Size(w, size.height), CornerRadius(w / 2f))
        }
    }
}

@Composable
private fun DialControl(
    control: Control,
    enabled: Boolean,
    fg: Color,
    onInput: ControlInput,
    modifier: Modifier,
    meter: State<Float>? = null
) {
    val haptic = LocalHapticFeedback.current
    val slop = LocalViewConfiguration.current.touchSlop
    var pressed by remember { mutableStateOf(false) }
    var dragging by remember { mutableStateOf(false) }
    val hostValue by rememberUpdatedState(control.value_.coerceIn(0f, 1f))
    var localValue by remember { mutableFloatStateOf(hostValue) }
    // follow host value whenever we are not mid-drag
    val shown = if (dragging) localValue else control.value_.coerceIn(0f, 1f)
    val animated by animateFloatAsState(shown, tween(if (dragging) 0 else 240), label = "dialValue")

    KeyCap(
        pressed = pressed,
        lit = false,
        tint = null,
        modifier = modifier.then(
            if (!enabled) Modifier else Modifier.pointerInput(control.id) {
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    val c = Offset(size.width / 2f, size.height / 2f)
                    fun angleOf(p: Offset): Float =
                        (atan2(p.y - c.y, p.x - c.x) * 180f / PI.toFloat())
                    var last = angleOf(down.position)
                    var accum = 0f
                    var moved = 0f
                    var rotating = false
                    pressed = true
                    do {
                        val ev = awaitPointerEvent()
                        val ch = ev.changes.first()
                        moved += (ch.position - ch.previousPosition).getDistance()
                        if (!rotating && moved > slop) {
                            rotating = true
                            dragging = true
                            localValue = hostValue
                        }
                        if (rotating) {
                            val now = angleOf(ch.position)
                            var d = now - last
                            if (d > 180f) d -= 360f
                            if (d <= -180f) d += 360f
                            last = now
                            accum += d
                            val detents = (accum / DIAL_DEGREES_PER_DETENT).toInt()
                            if (detents != 0) {
                                accum -= detents * DIAL_DEGREES_PER_DETENT
                                localValue = (localValue + detents * DIAL_STEP).coerceIn(0f, 1f)
                                haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                                onInput(InputKind.INPUT_KIND_DIAL_ROTATE, detents, 0f)
                            }
                            ch.consume()
                        }
                    } while (ev.changes.any { it.pressed })
                    pressed = false
                    dragging = false
                    if (!rotating) {
                        haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                        onInput(InputKind.INPUT_KIND_DIAL_PRESS, 0, 0f)
                        onInput(InputKind.INPUT_KIND_DIAL_RELEASE, 0, 0f)
                    }
                }
            }
        )
    ) {
        Column(
            Modifier
                .fillMaxSize()
                .padding(8.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.SpaceBetween
        ) {
            Text(
                control.title.ifEmpty { "Knob" },
                color = NdColors.TextDim, style = NdType.Micro,
                maxLines = 1, overflow = TextOverflow.Ellipsis
            )
            Box(
                Modifier
                    .weight(1f, fill = true)
                    .fillMaxSize()
                    .padding(vertical = 4.dp),
                contentAlignment = Alignment.Center
            ) {
                Canvas(Modifier.fillMaxSize()) {
                    val d = minOf(size.width, size.height)
                    val stroke = (d * 0.085f).coerceIn(3.dp.toPx(), 9.dp.toPx())
                    val r = d / 2f - stroke
                    val tl = Offset(center.x - r, center.y - r)
                    val sz = Size(r * 2, r * 2)
                    // track
                    drawArc(NdColors.Track, 135f, 270f, false, tl, sz, style = Stroke(stroke, cap = StrokeCap.Round))
                    // value
                    drawArc(
                        NdColors.Accent, 135f, 270f * animated, false, tl, sz,
                        style = Stroke(stroke, cap = StrokeCap.Round)
                    )
                    if (meter != null) {
                        drawDialRing(meter.value, center, r - stroke / 2f - 5.dp.toPx())
                    }
                    // knob index mark
                    val a = Math.toRadians((135f + 270f * animated).toDouble())
                    val inner = r * 0.38f
                    val outer = r * 0.72f
                    drawLine(
                        fg,
                        Offset(center.x + (inner * Math.cos(a)).toFloat(), center.y + (inner * Math.sin(a)).toFloat()),
                        Offset(center.x + (outer * Math.cos(a)).toFloat(), center.y + (outer * Math.sin(a)).toFloat()),
                        strokeWidth = 2.dp.toPx(), cap = StrokeCap.Round
                    )
                }
            }
            Text(
                "${(animated * 100).roundToInt()}",
                color = fg, style = NdType.Label
            )
        }
    }
}
