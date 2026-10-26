// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import kotlin.math.ceil

/** Pure geometry for the channel-strip fader; no Compose, so it is unit-testable. */
internal object FaderGeometry {
    /** Distance from the low end of the track to the thumb centre. The thumb never leaves [0, length]. */
    fun thumbCenter(value: Float, length: Float, thumb: Float): Float {
        val travel = (length - thumb).coerceAtLeast(0f)
        return (length - travel) / 2f + value.coerceIn(0f, 1f) * travel
    }

    /** Inverse of [thumbCenter]: value for a touch [posFromLow] px from the low end. */
    fun valueAt(posFromLow: Float, length: Float, thumb: Float): Float {
        val travel = length - thumb
        if (travel <= 0f) return 0f
        return ((posFromLow - thumb / 2f) / travel).coerceIn(0f, 1f)
    }

    /** Number of LED segments that fit in [length] (segment + gap stride), clamped to a sane range. */
    fun ledCount(length: Float, segment: Float, gap: Float): Int =
        if (length <= 0f) MIN_LEDS else (length / (segment + gap)).toInt().coerceIn(MIN_LEDS, MAX_LEDS)

    /** Lit segments for a 0..1 level. Any audible level lights at least the first segment. */
    fun ledLit(level: Float, count: Int): Int {
        val l = level.coerceIn(0f, 1f)
        if (l < 0.01f) return 0
        return ceil(l * count - 0.35f).toInt().coerceIn(1, count)
    }

    enum class Zone { GREEN, AMBER, RED }

    /** Colour zone of segment [index] (0 = bottom) in a ladder of [count]. */
    fun zone(index: Int, count: Int): Zone {
        val f = (index + 0.5f) / count
        return if (f < GREEN_TO) Zone.GREEN else if (f < AMBER_TO) Zone.AMBER else Zone.RED
    }

    const val MIN_LEDS = 8
    const val MAX_LEDS = 40
    const val GREEN_TO = 0.7f
    const val AMBER_TO = 0.9f
}
