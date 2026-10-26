// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.ui.input.pointer.AwaitPointerEventScope
import androidx.compose.ui.input.pointer.PointerInputChange
import kotlinx.coroutines.withTimeoutOrNull

/** Shared between DeckGrid (writer of [available], reader of [keyHeld]) and the keys/tiles inside it. */
internal class SwipeGate {
    /** Page swiping is possible, so key presses go through [PressGate]. */
    @Volatile var available = false
    /** A key already sent KEY_DOWN for the current gesture; the grid must not turn it into a page swipe. */
    @Volatile var keyHeld = false
}

internal val LocalSwipeGate = compositionLocalOf { SwipeGate() }

internal sealed interface PressOutcome {
    /** Cancelled by a swipe-like move: nothing was sent. */
    data object Cancelled : PressOutcome
    /** KEY_DOWN is due now. [released] = the finger is already up; [elapsedMs] since touch-down. */
    data class Fire(val released: Boolean, val elapsedMs: Long) : PressOutcome
}

/** Waits (at most [PressGate.HOLD_MS]) for the pointer of [down] to decide whether the press is a tap or a swipe start. */
internal suspend fun AwaitPointerEventScope.awaitPressGate(
    down: PointerInputChange, gate: SwipeGate, slop: Float
): PressOutcome {
    if (!gate.available) return PressOutcome.Fire(false, 0L)
    var outcome: PressOutcome? = null
    withTimeoutOrNull(PressGate.HOLD_MS) {
        while (outcome == null) {
            val ev = awaitPointerEvent()
            val ch = ev.changes.firstOrNull { it.id == down.id }
            if (ch == null) { outcome = PressOutcome.Fire(true, PressGate.HOLD_MS); break }
            val elapsed = ch.uptimeMillis - down.uptimeMillis
            val released = !ch.pressed
            val dx = ch.position.x - down.position.x
            val dy = ch.position.y - down.position.y
            outcome = when (PressGate.evaluate(true, elapsed, dx, dy, slop, released)) {
                PressGate.Result.Cancel -> PressOutcome.Cancelled
                PressGate.Result.Fire -> PressOutcome.Fire(released, elapsed)
                PressGate.Result.Pending -> null
            }
        }
    }
    return outcome ?: PressOutcome.Fire(false, PressGate.HOLD_MS)
}
