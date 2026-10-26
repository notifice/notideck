// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import kotlin.math.exp

/**
 * Peak-meter ballistics: instant attack, exponential release.
 * [releaseTauMs] = 100 gives ~5% of the original after 300 ms.
 */
class LevelSmoother(private val releaseTauMs: Float = 100f, private val floor: Float = 0.004f) {
    var current: Float = 0f
        private set

    /** Advance by [dtMs] toward [target] (0..1) and return the displayed level. */
    fun step(target: Float, dtMs: Long): Float {
        val t = if (target.isNaN()) 0f else target.coerceIn(0f, 1f)
        current = if (t >= current) {
            t
        } else {
            val v = t + (current - t) * exp(-dtMs.coerceAtLeast(0L) / releaseTauMs)
            if (v - t < floor) t else v
        }
        return current
    }

    fun reset() {
        current = 0f
    }
}
