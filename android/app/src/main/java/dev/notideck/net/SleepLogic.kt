// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.DeckSleep

/** Why the host wants a sleeping deck awake. */
enum class WakeReason { CameraOrMic, Player }

/** Deck sleep settings from the host's Appearance. [idleS] is already clamped. */
data class SleepConfig(
    val enabled: Boolean,
    val idleS: Int,
    val wakeOnCameraMic: Boolean,
    val showClock: Boolean
) {
    companion object {
        fun from(p: DeckSleep) = SleepConfig(p.enabled, SleepLogic.clampIdleS(p.idle_s), p.wake_on_camera_mic, p.show_clock)
    }
}

/** Pure sleep rules; the UI feeds it touches and inhibitors. */
object SleepLogic {
    const val DEFAULT_IDLE_S = 120
    const val MIN_IDLE_S = 15
    const val MAX_IDLE_S = 3600

    /** 0 = default; otherwise clamped to 15..3600 s. */
    fun clampIdleS(raw: Int): Int =
        if (raw == 0) DEFAULT_IDLE_S else raw.coerceIn(MIN_IDLE_S, MAX_IDLE_S)

    /** The deck may go to sleep at all: sleep is on and nothing keeps it awake. */
    fun maySleep(config: SleepConfig?, inhibited: Boolean): Boolean = config != null && config.enabled && !inhibited

    /**
     * Top-left of the drifting clock inside a [w] x [h] area: [fx], [fy] in 0..1 pick the spot among the
     * positions that keep a [margin] to every edge (a clock bigger than the room sticks to the margin).
     */
    fun clockOffset(fx: Float, fy: Float, w: Int, h: Int, cw: Int, ch: Int, margin: Int): Pair<Int, Int> {
        val rx = (w - cw - 2 * margin).coerceAtLeast(0)
        val ry = (h - ch - 2 * margin).coerceAtLeast(0)
        return Pair(margin + (fx.coerceIn(0f, 1f) * rx).toInt(), margin + (fy.coerceIn(0f, 1f) * ry).toInt())
    }
}

/** Idle timer over an injectable millisecond clock. */
class SleepTimer(private val now: () -> Long) {
    private var lastTouch = now()

    fun touch() { lastTouch = now() }

    /** Milliseconds until the deck should sleep (0 = now); null when it may not sleep. */
    fun remainingMs(config: SleepConfig?, inhibited: Boolean): Long? {
        if (!SleepLogic.maySleep(config, inhibited)) return null
        return (lastTouch + config!!.idleS * 1000L - now()).coerceAtLeast(0L)
    }

    fun shouldSleep(config: SleepConfig?, inhibited: Boolean): Boolean = remainingMs(config, inhibited) == 0L
}
