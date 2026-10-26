// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.layout
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.notideck.net.SleepLogic
import kotlinx.coroutines.delay
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.random.Random

private const val CLOCK_MOVE_MS = 60_000L

/** The hosting activity, if any (previews have none). */
private fun Context.findActivity(): Activity? {
    var c = this
    while (c is ContextWrapper) {
        if (c is Activity) return c
        c = c.baseContext
    }
    return null
}

/** While [asleep] the window backlight override is 0.0f; the previous value comes back on wake or leaving. */
@Composable
fun SleepBrightness(asleep: Boolean) {
    val window = LocalContext.current.findActivity()?.window
    DisposableEffect(asleep, window) {
        if (!asleep || window == null) return@DisposableEffect onDispose {}
        val attrs = window.attributes
        val previous = attrs.screenBrightness
        attrs.screenBrightness = 0f
        window.attributes = attrs
        onDispose {
            val back = window.attributes
            back.screenBrightness = previous
            window.attributes = back
        }
    }
}

/**
 * Black cover over the whole deck while [asleep]. The first touch-down calls [onWake] and the rest of that
 * gesture (until every finger is up) is consumed here, so nothing underneath ever sees it; the cover stays
 * until then.
 */
@Composable
fun SleepOverlay(asleep: Boolean, showClock: Boolean, onWake: () -> Unit, modifier: Modifier = Modifier) {
    var holding by remember { mutableStateOf(false) }
    if (!asleep && !holding) return
    Box(
        modifier
            .fillMaxSize()
            .background(Color.Black)
            .pointerInput(Unit) {
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
                    down.consume()
                    holding = true
                    onWake()
                    do {
                        val event = awaitPointerEvent(PointerEventPass.Initial)
                        event.changes.forEach { it.consume() }
                    } while (event.changes.any { it.pressed })
                    holding = false
                }
            }
    ) {
        if (asleep && showClock) DriftingClock()
    }
}

/** Faint HH:mm that jumps to a random spot (kept off the edges) every minute. */
@Composable
private fun DriftingClock() {
    val fmt = remember { SimpleDateFormat("HH:mm", Locale.getDefault()) }
    var time by remember { mutableStateOf(fmt.format(Date())) }
    var fx by remember { mutableStateOf(Random.nextFloat()) }
    var fy by remember { mutableStateOf(Random.nextFloat()) }
    LaunchedEffect(Unit) {
        var sinceMove = 0L
        while (true) {
            val now = System.currentTimeMillis()
            val step = (60_000L - now % 60_000L).coerceAtLeast(1_000L)
            delay(step)
            time = fmt.format(Date())
            sinceMove += step
            if (sinceMove >= CLOCK_MOVE_MS) {
                sinceMove = 0
                fx = Random.nextFloat()
                fy = Random.nextFloat()
            }
        }
    }
    val margin = with(androidx.compose.ui.platform.LocalDensity.current) { 24.dp.roundToPx() }
    Text(
        time,
        style = TextStyle(color = Color.White.copy(alpha = 0.18f), fontSize = 28.sp),
        modifier = Modifier.layout { measurable, constraints ->
            val p = measurable.measure(constraints.copy(minWidth = 0, minHeight = 0))
            layout(constraints.maxWidth, constraints.maxHeight) {
                val (x, y) = SleepLogic.clockOffset(fx, fy, constraints.maxWidth, constraints.maxHeight, p.width, p.height, margin)
                p.place(x, y)
            }
        }
    )
}
