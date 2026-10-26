// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import kotlin.math.abs

/**
 * Pure drift controller for synced playback (SPEC round 21). Feed it `err = shown - target` in ms
 * (positive = ahead of the target) every 500 ms, plus how much is buffered ahead of the playhead,
 * and it says what to do. It never asks for a move that would need the network (that rebuffers):
 * ahead → hold (pause) the picture, behind → jump forward only inside the buffer.
 */
class SyncController {
    sealed class Action {
        /** Nothing to change. */
        object None : Action()
        /** Set the playback rate. */
        data class Rate(val rate: Float) : Action()
        /** Seek forward by [deltaMs] (inside the buffer) and reset the rate to 1.0. */
        data class Seek(val deltaMs: Long) : Action()
        /** Pause for [ms] (we are ahead) and reset the rate to 1.0. */
        data class Hold(val ms: Long) : Action()
    }

    companion object {
        const val SEEK_THRESHOLD_MS = 1000L
        const val RATE_ON_MS = 40L
        const val RATE_OFF_MS = 20L
        const val MAX_RATE_DELTA = 0.05f
        const val RATE_DIVISOR = 4000f
        /** Buffer kept in front of the playhead after a forward jump. */
        const val HEADROOM_MARGIN_MS = 700L
        /** Smaller forward jumps are not worth a decoder flush. */
        const val MIN_JUMP_MS = 300L
        /** Speeding up with less than this buffered ahead would drain it and rebuffer. */
        const val RATE_UP_MIN_HEADROOM_MS = 1500L
        /** Ticks ignored after a seek / hold while the player settles. */
        const val COOLDOWN_TICKS = 3
        /** Longest single hold; a bigger error (bad clock offset) is worked off over several holds. */
        const val MAX_HOLD_MS = 8000L
    }

    var rate: Float = 1f
        private set
    private var cooldown = 0

    fun update(errMs: Long, headroomMs: Long = Long.MAX_VALUE): Action {
        if (cooldown > 0) { cooldown--; return Action.None }
        val a = abs(errMs)
        return when {
            a > SEEK_THRESHOLD_MS && errMs > 0 -> { rate = 1f; cooldown = COOLDOWN_TICKS; Action.Hold(minOf(errMs, MAX_HOLD_MS)) }
            a > SEEK_THRESHOLD_MS -> {
                val jump = minOf(-errMs, headroomMs - HEADROOM_MARGIN_MS)
                if (jump >= MIN_JUMP_MS) {
                    rate = 1f; cooldown = COOLDOWN_TICKS; Action.Seek(jump)
                } else rateTo(1f) // at the live edge: the stream is later than the target, play as close as we can
            }
            a > RATE_ON_MS -> {
                var r = 1f - (errMs / RATE_DIVISOR).coerceIn(-MAX_RATE_DELTA, MAX_RATE_DELTA)
                if (r > 1f && headroomMs < RATE_UP_MIN_HEADROOM_MS) r = 1f
                rateTo(r)
            }
            a < RATE_OFF_MS -> rateTo(1f)
            else -> Action.None
        }
    }

    private fun rateTo(r: Float): Action = if (r == rate) Action.None else { rate = r; Action.Rate(r) }

    fun reset() { rate = 1f; cooldown = 0 }
}
