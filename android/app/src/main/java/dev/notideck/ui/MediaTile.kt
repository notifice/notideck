// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.State
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInRoot
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.delay
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.notideck.net.Flash
import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.InputKind
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdShapes
import dev.notideck.ui.theme.NdType

/** Now-playing tile face (CONTROL_FACE_MEDIA). See the ControlFace comment in control.proto. */
@Composable
internal fun MediaTile(
    control: Control,
    enabled: Boolean,
    tint: Color?,
    icons: Map<String, ImageBitmap>,
    flash: Flash?,
    onInput: ControlInput,
    modifier: Modifier
) {
    var pressed by remember { mutableStateOf(false) }
    val idle = control.dimmed
    val playing = !idle && control.state_index == 1
    val art = if (!idle && control.icon_hash.isNotEmpty()) icons[control.icon_hash] else null
    // Host updates arrive about once a second; a linear tween of the same length reads as continuous.
    val progress by animateFloatAsState(
        if (idle) 0f else control.value_.coerceIn(0f, 1f),
        tween(if (playing) 1000 else 200, easing = LinearEasing),
        label = "mediaProgress"
    )
    // Local scrubbing: scrub = finger fraction while down; held = released position shown until the host
    // reports a new value (or 1.5 s). Host progress is ignored for display in both states.
    var scrub by remember { mutableStateOf<Float?>(null) }
    var held by remember { mutableStateOf<Float?>(null) }
    var heldHostValue by remember { mutableStateOf(0f) }
    LaunchedEffect(held) {
        if (held != null) { delay(1500); held = null }
    }
    LaunchedEffect(control.value_) {
        if (held != null && control.value_ != heldHostValue) held = null
    }
    if (idle && (scrub != null || held != null)) { scrub = null; held = null }
    val shown: () -> Float = { scrub ?: held ?: progress }
    val barExtent = remember { floatArrayOf(Float.NaN, Float.NaN, Float.NaN) } // tileX, barLeft, barWidth (root px)
    val context = LocalContext.current
    val swipeGate = LocalSwipeGate.current
    val desc = if (idle) "Nothing playing" else listOf(control.title, control.subtitle).filter { it.isNotEmpty() }.joinToString(", ")

    KeyCap(
        pressed = pressed,
        lit = false,
        tint = tint,
        flash = flash,
        modifier = modifier
            .semantics { contentDescription = desc }
            .onGloballyPositioned { barExtent[0] = it.positionInRoot().x }
            .then(
                if (!enabled) Modifier else Modifier.pointerInput(control.id, idle) {
                    awaitEachGesture {
                        val down = awaitFirstDown(requireUnconsumed = false)
                        val bandPx = MediaBand.HEIGHT_DP.dp.toPx()
                        if (!idle && MediaBand.hit(down.position.y, size.height.toFloat(), bandPx)) {
                            // Progress-band seek: no KEY_DOWN/KEY_UP, one SLIDER_VALUE on release.
                            val left = if (barExtent[1].isNaN()) 8.dp.toPx() else barExtent[1] - barExtent[0]
                            val width = if (barExtent[2].isNaN()) size.width - 16.dp.toPx() else barExtent[2]
                            fun fracAt(x: Float) = MediaBand.fraction(x, left, width)
                            Haptics.vibrateClick(context)
                            held = null
                            scrub = fracAt(down.position.x)
                            down.consume()
                            var up = false
                            while (true) {
                                val ev = awaitPointerEvent()
                                val ch = ev.changes.firstOrNull { it.id == down.id } ?: break
                                scrub = fracAt(ch.position.x)
                                ch.consume()
                                if (!ch.pressed) { up = true; break }
                            }
                            val f = scrub
                            scrub = null
                            if (up && f != null) {
                                heldHostValue = control.value_
                                held = f
                                onInput(InputKind.INPUT_KIND_SLIDER_VALUE, 0, f)
                            }
                            return@awaitEachGesture
                        }
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
        BoxWithConstraints(Modifier.fillMaxSize()) {
            val layout = mediaLayoutFor(control.span_w, control.span_h, maxWidth.value, maxHeight.value)
            when (layout) {
                MediaLayout.Mini -> {
                    ArtSquare(art, Modifier.fillMaxSize(), rounded = false)
                    if (!idle) ProgressBar(
                        shown, scrub != null,
                        Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(3.dp).padding(horizontal = 6.dp),
                        barExtent
                    )
                }
                MediaLayout.Wide -> Row(Modifier.fillMaxSize().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    ArtSquare(art, Modifier.fillMaxHeight().aspectRatio(1f))
                    Spacer(Modifier.width(10.dp))
                    MediaText(control, idle, playing, shown, scrub != null, barExtent, Modifier.weight(1f))
                }
                MediaLayout.Tall -> Column(Modifier.fillMaxSize().padding(8.dp)) {
                    ArtSquare(art, Modifier.fillMaxWidth().aspectRatio(1f))
                    Spacer(Modifier.height(8.dp))
                    MediaText(control, idle, playing, shown, scrub != null, barExtent, Modifier.weight(1f))
                }
            }
        }
    }
}

@Composable
private fun MediaText(
    control: Control,
    idle: Boolean,
    playing: Boolean,
    progress: () -> Float,
    scrubbing: Boolean,
    barExtent: FloatArray,
    modifier: Modifier
) {
    Column(modifier, verticalArrangement = androidx.compose.foundation.layout.Arrangement.Center) {
        if (idle) {
            Text("Nothing playing", color = NdColors.TextDim, style = NdType.KeyTitle, maxLines = 2, overflow = TextOverflow.Ellipsis)
            return@Column
        }
        Row(verticalAlignment = Alignment.Top) {
            Text(
                control.title, color = NdColors.Text, style = NdType.KeyTitle,
                maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f)
            )
            Spacer(Modifier.width(6.dp))
            Equalizer(playing, Modifier.padding(top = 3.dp))
        }
        if (control.subtitle.isNotEmpty()) {
            Text(
                control.subtitle, color = NdColors.TextDim, style = NdType.Label,
                maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(top = 1.dp)
            )
        }
        if (control.detail.isNotEmpty()) {
            Text(
                control.detail, color = NdColors.TextDim, style = NdType.Micro,
                maxLines = 1, overflow = TextOverflow.Clip, modifier = Modifier.padding(top = 3.dp)
            )
        }
        ProgressBar(progress, scrubbing, Modifier.padding(top = 5.dp).fillMaxWidth().height(3.dp), barExtent)
    }
}

/** Album art in a rounded square; a note glyph on a recessed square when there is none. */
@Composable
private fun ArtSquare(art: ImageBitmap?, modifier: Modifier, rounded: Boolean = true) {
    val shape = if (rounded) NdShapes.KeyInner else androidx.compose.ui.graphics.RectangleShape
    if (art != null) {
        Image(art, null, contentScale = ContentScale.Crop, modifier = modifier.clip(shape))
    } else {
        Canvas(modifier.clip(shape).background(NdColors.Well)) {
            val s = minOf(size.width, size.height)
            val u = s / 10f
            val ox = (size.width - s) / 2f
            val oy = (size.height - s) / 2f
            val c = NdColors.TextFaint
            // Beamed-note glyph: two heads, two stems, one beam.
            val sw = u * 0.8f
            drawOval(c, Offset(ox + 2.6f * u, oy + 6.2f * u), Size(2.4f * u, 1.8f * u))
            drawOval(c, Offset(ox + 5.8f * u, oy + 5.4f * u), Size(2.4f * u, 1.8f * u))
            drawRect(c, Offset(ox + 4.8f * u, oy + 2.4f * u), Size(sw * 0.7f, 4.6f * u))
            drawRect(c, Offset(ox + 8.0f * u, oy + 1.8f * u), Size(sw * 0.7f, 4.5f * u))
            val beam = Path().apply {
                moveTo(ox + 4.8f * u, oy + 2.4f * u)
                lineTo(ox + 8.6f * u, oy + 1.8f * u)
                lineTo(ox + 8.6f * u, oy + 3.0f * u)
                lineTo(ox + 4.8f * u, oy + 3.6f * u)
                close()
            }
            drawPath(beam, c)
        }
    }
}

/** Thin recessed track with an accent fill. Progress is read in the draw phase only. */
@Composable
private fun ProgressBar(progress: () -> Float, scrubbing: Boolean, modifier: Modifier, barExtent: FloatArray) {
    Canvas(modifier.onGloballyPositioned {
        barExtent[1] = it.positionInRoot().x
        barExtent[2] = it.size.width.toFloat()
    }) {
        val r = size.height / 2f
        drawRoundRect(NdColors.Track, size = size, cornerRadius = androidx.compose.ui.geometry.CornerRadius(r, r))
        val w = size.width * progress().coerceIn(0f, 1f)
        if (w > 0f) drawRoundRect(NdColors.Accent, size = Size(maxOf(w, size.height), size.height), cornerRadius = androidx.compose.ui.geometry.CornerRadius(r, r))
        if (scrubbing) drawCircle(NdColors.Text, 6.dp.toPx(), Offset(w, size.height / 2f))
    }
}

private val EQ_BASE = floatArrayOf(0.35f, 0.7f, 0.5f)
private val EQ_MS = intArrayOf(520, 380, 450)

/** Three-bar equalizer. The infinite transition only exists while [playing]; paused draws static bars. */
@Composable
private fun Equalizer(playing: Boolean, modifier: Modifier) {
    val bars: List<State<Float>>? = if (playing) {
        val t = rememberInfiniteTransition(label = "eq")
        List(3) { i ->
            t.animateFloat(
                0.2f, 1f,
                infiniteRepeatable(tween(EQ_MS[i], easing = LinearEasing), RepeatMode.Reverse),
                label = "eq$i"
            )
        }
    } else null
    Canvas(modifier.size(width = 14.dp, height = 12.dp)) {
        val gap = 2.dp.toPx()
        val bw = (size.width - gap * 2) / 3f
        for (i in 0 until 3) {
            val f = bars?.get(i)?.value ?: EQ_BASE[i]
            val h = size.height * f
            drawRect(if (playing) NdColors.Accent else NdColors.TextFaint, Offset(i * (bw + gap), size.height - h), Size(bw, h))
        }
    }
}
