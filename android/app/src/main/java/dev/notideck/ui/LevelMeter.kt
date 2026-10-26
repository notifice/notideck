// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.foundation.Canvas
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.unit.dp
import dev.notideck.ui.theme.NdColors

/** Fraction of the scale where the meter turns hot. */
internal const val METER_HOT_FROM = 0.85f

/**
 * Smoothed display level. Frames are only requested while the displayed level differs from the
 * target, so an idle (silent or steady) meter costs nothing. Reading the returned State inside a
 * draw lambda keeps level changes out of composition.
 */
@Composable
fun rememberMeterLevel(target: Float, active: Boolean): State<Float> {
    val shown = remember { mutableFloatStateOf(0f) }
    val smoother = remember { LevelSmoother() }
    LaunchedEffect(target, active) {
        val t = if (active) target else 0f
        shown.floatValue = smoother.step(t, 0)
        if (smoother.current == t.coerceIn(0f, 1f)) return@LaunchedEffect
        var last = withFrameNanos { it }
        while (true) {
            val now = withFrameNanos { it }
            val v = smoother.step(t, (now - last) / 1_000_000L)
            last = now
            shown.floatValue = v
            if (v == t.coerceIn(0f, 1f)) break
        }
    }
    return shown
}

/** Draws a straight bar of [level] over [length] from (x, y) with a hot tail; vertical bars fill from the bottom. */
internal fun DrawScope.drawLevelBar(level: Float, x: Float, y: Float, length: Float, thickness: Float, horizontal: Boolean) {
    if (level <= 0.001f) return
    val r = CornerRadius(thickness / 2f)
    val cool = level.coerceAtMost(METER_HOT_FROM)
    fun seg(from: Float, to: Float, c: Color) {
        if (to - from <= 0f) return
        if (horizontal) {
            drawRoundRect(c, Offset(x + length * from, y), Size(length * (to - from), thickness), r)
        } else {
            // vertical: fills from the bottom; (x, y) is top-left of the full bar
            drawRoundRect(c, Offset(x, y + length * (1f - to)), Size(thickness, length * (to - from)), r)
        }
    }
    seg(0f, cool, NdColors.Accent)
    seg(METER_HOT_FROM, level, if (level > 0.95f) NdColors.Alert else NdColors.Hot)
}

/** Thin bar along the bottom edge of a key / tile. Reads [level] only while drawing. */
@Composable
fun BottomLevelBar(level: State<Float>, modifier: Modifier = Modifier) {
    Canvas(modifier) {
        val v = level.value
        val t = 3.dp.toPx()
        drawLevelBar(v, 0f, size.height - t, size.width, t, horizontal = true)
    }
}

/** Thin inner ring for a dial, just inside the value arc. */
internal fun DrawScope.drawDialRing(level: Float, center: Offset, radius: Float) {
    if (level <= 0.001f) return
    val t = 3.dp.toPx()
    val tl = Offset(center.x - radius, center.y - radius)
    val sz = Size(radius * 2, radius * 2)
    val cool = level.coerceAtMost(METER_HOT_FROM)
    drawArc(NdColors.Accent, 135f, 270f * cool, false, tl, sz, style = Stroke(t))
    if (level > METER_HOT_FROM) {
        drawArc(
            if (level > 0.95f) NdColors.Alert else NdColors.Hot,
            135f + 270f * METER_HOT_FROM, 270f * (level - METER_HOT_FROM), false, tl, sz, style = Stroke(t)
        )
    }
}
