// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import dev.notideck.proto.v1.ControlFace
import dev.notideck.proto.v1.ControlKind
import kotlinx.coroutines.launch
import kotlin.math.abs
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.unit.dp
import dev.notideck.net.Flash
import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.InputKind
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdShapes

/**
 * Responsive grid of deck controls. Empty cells are drawn as recessed wells with a
 * registration crosshair, so the panel reads as a machined plate rather than blank glass.
 */
@Composable
fun DeckGrid(
    cols: Int,
    rows: Int,
    controls: Map<String, Control>,
    onInput: (controlId: String, kind: InputKind, delta: Int, value: Float) -> Unit,
    enabled: Boolean = true,
    icons: Map<String, ImageBitmap> = emptyMap(),
    flashes: Map<String, Flash> = emptyMap(),
    pageIndex: Int = 0,
    pageCount: Int = 0,
    onGoToPage: (Int) -> Unit = {},
    modifier: Modifier = Modifier
) {
    val safeCols = if (cols > 0) cols else 5
    val safeRows = if (rows > 0) rows else 3
    val spacing = 12.dp

    val density = LocalDensity.current.density
    val haptic = LocalContext.current
    val slide = remember { Animatable(0f) }
    val scope = rememberCoroutineScope()
    val latest = rememberUpdatedState(Triple(controls, pageIndex to pageCount, enabled))
    val swipeGate = remember { SwipeGate() }
    swipeGate.available = PageSwipe.available(enabled, pageCount)
    val latestGoTo = rememberUpdatedState(onGoToPage)

    BoxWithConstraints(
        modifier = modifier
            .fillMaxSize()
            .padding(horizontal = 14.dp, vertical = 12.dp)
            .pointerInput(safeCols, safeRows) {
                // Observe-only (Initial pass, nothing consumed): children keep every gesture. A swipe that
                // starts on a slider, dial or media progress band is ignored; those controls own horizontal drags.
                val spacingPx = spacing.toPx()
                val bandPx = MediaBand.HEIGHT_DP * density
                val followMax = 40.dp.toPx()
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
                    val (ctrls, pages, en) = latest.value
                    if (!PageSwipe.available(en, pages.second)) return@awaitEachGesture
                    val cellW = (size.width - spacingPx * (safeCols - 1)) / safeCols
                    val cellH = (size.height - spacingPx * (safeRows - 1)) / safeRows
                    if (startsOnSwipeBlocker(down.position, ctrls.values, safeCols, safeRows, cellW, cellH, spacingPx, bandPx)) {
                        return@awaitEachGesture
                    }
                    val tracker = VelocityTracker()
                    tracker.addPosition(down.uptimeMillis, down.position)
                    val id = down.id
                    var dx = 0f
                    var dy = 0f
                    var cancelled = false
                    var ended = false
                    while (!ended) {
                        val ev = awaitPointerEvent(PointerEventPass.Initial)
                        if (ev.changes.count { it.pressed } > 1) cancelled = true
                        val ch = ev.changes.firstOrNull { it.id == id } ?: break
                        tracker.addPosition(ch.uptimeMillis, ch.position)
                        dx = ch.position.x - down.position.x
                        dy = ch.position.y - down.position.y
                        if (!ch.pressed) ended = true
                        else if (!cancelled && abs(dx) > viewConfiguration.touchSlop && abs(dx) > PageSwipe.DIRECTION_BIAS * abs(dy)) {
                            if (!swipeGate.keyHeld) scope.launch { slide.snapTo(PageSwipe.followOffset(dx, followMax)) }
                        }
                    }
                    if (!ended || cancelled) {
                        scope.launch { slide.animateTo(0f, tween(120)) }
                        return@awaitEachGesture
                    }
                    val v = tracker.calculateVelocity()
                    // A key that already fired (held past the press gate) keeps the page where it is.
                    val delta = if (swipeGate.keyHeld) 0 else PageSwipe.decide(dx, dy, v.x, v.y, size.width.toFloat(), density, latest.value.second.first, latest.value.second.second)
                    if (delta == 0) {
                        scope.launch { slide.animateTo(0f, tween(120)) }
                    } else {
                        Haptics.vibrateClick(haptic)
                        latestGoTo.value(latest.value.second.first + delta)
                        // New page enters from the swipe direction.
                        scope.launch {
                            slide.snapTo(delta * 56.dp.toPx())
                            slide.animateTo(0f, tween(200))
                        }
                    }
                }
            }
    ) {
        CompositionLocalProvider(LocalSwipeGate provides swipeGate) {
        Box(Modifier.fillMaxSize().graphicsLayer { translationX = slide.value; alpha = 1f - 0.5f * (abs(slide.value) / 56.dp.toPx()).coerceIn(0f, 1f) }) {
        val cellWidth = (this@BoxWithConstraints.maxWidth - spacing * (safeCols - 1)) / safeCols
        val cellHeight = (this@BoxWithConstraints.maxHeight - spacing * (safeRows - 1)) / safeRows

        // 1. Recessed wells for every free slot (cells under a control stay clear, so a dimmed control
        //    doesn't show the "+" markers through it).
        val covered = HashSet<Int>()
        for (control in controls.values) {
            val c0 = control.col.coerceIn(0, safeCols - 1)
            val r0 = control.row.coerceIn(0, safeRows - 1)
            val w = (if (control.span_w > 0) control.span_w else 1).coerceAtMost(safeCols - c0)
            val h = (if (control.span_h > 0) control.span_h else 1).coerceAtMost(safeRows - r0)
            for (dc in 0 until w) for (dr in 0 until h) covered += (r0 + dr) * safeCols + (c0 + dc)
        }
        for (c in 0 until safeCols) {
            for (r in 0 until safeRows) {
                if (r * safeCols + c in covered) continue
                Canvas(
                    modifier = Modifier
                        .offset(x = (cellWidth + spacing) * c, y = (cellHeight + spacing) * r)
                        .size(width = cellWidth, height = cellHeight)
                        .clip(NdShapes.Slot)
                        .border(1.dp, NdColors.Hairline.copy(alpha = 0.05f), NdShapes.Slot)
                ) {
                    drawRect(NdColors.Well)
                    val arm = 4.dp.toPx()
                    val p = Offset(size.width / 2f, size.height / 2f)
                    val col = NdColors.TextFaint.copy(alpha = 0.5f)
                    drawLine(col, Offset(p.x - arm, p.y), Offset(p.x + arm, p.y), 1.dp.toPx())
                    drawLine(col, Offset(p.x, p.y - arm), Offset(p.x, p.y + arm), 1.dp.toPx())
                }
            }
        }

        // 2. Controls positioned by col/row/span
        for (control in controls.values) {
            val c = control.col.coerceIn(0, safeCols - 1)
            val r = control.row.coerceIn(0, safeRows - 1)
            val spanW = (if (control.span_w > 0) control.span_w else 1).coerceAtMost(safeCols - c)
            val spanH = (if (control.span_h > 0) control.span_h else 1).coerceAtMost(safeRows - r)

            Box(
                modifier = Modifier
                    .offset(x = (cellWidth + spacing) * c, y = (cellHeight + spacing) * r)
                    .size(
                        width = cellWidth * spanW + spacing * (spanW - 1),
                        height = cellHeight * spanH + spacing * (spanH - 1)
                    )
            ) {
                DeckControlItem(
                    control = control,
                    enabled = enabled,
                    icons = icons,
                    flash = flashes[control.id],
                    onInput = { kind, delta, value -> onInput(control.id, kind, delta, value) }
                )
            }
        }
        }
        }
    }
}

/**
 * True when a touch at [p] (grid-local px) lands on a control that owns horizontal drags: a slider, a dial,
 * or the progress band of a media tile that has a player.
 */
private fun startsOnSwipeBlocker(
    p: Offset, controls: Collection<Control>, cols: Int, rows: Int,
    cellW: Float, cellH: Float, spacing: Float, bandPx: Float
): Boolean {
    for (control in controls) {
        val isMedia = control.kind == ControlKind.CONTROL_KIND_TILE && control.face == ControlFace.CONTROL_FACE_MEDIA
        if (control.kind != ControlKind.CONTROL_KIND_SLIDER && control.kind != ControlKind.CONTROL_KIND_DIAL && !isMedia) continue
        if (isMedia && control.dimmed) continue
        val c = control.col.coerceIn(0, cols - 1)
        val r = control.row.coerceIn(0, rows - 1)
        val sw = (if (control.span_w > 0) control.span_w else 1).coerceAtMost(cols - c)
        val sh = (if (control.span_h > 0) control.span_h else 1).coerceAtMost(rows - r)
        val left = (cellW + spacing) * c
        val top = (cellH + spacing) * r
        val w = cellW * sw + spacing * (sw - 1)
        val h = cellH * sh + spacing * (sh - 1)
        if (p.x < left || p.x > left + w || p.y < top || p.y > top + h) continue
        if (!isMedia || MediaBand.hit(p.y - top, h, bandPx)) return true
    }
    return false
}
