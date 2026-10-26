// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import kotlin.math.abs
import kotlin.math.max

/** Pure decision logic for swiping between deck pages; no Compose, so it is unit-testable. */
internal object PageSwipe {
    const val MIN_TRAVEL_DP = 48f
    const val TRAVEL_FRACTION = 0.18f
    const val FLING_DP_PER_S = 1000f
    /** A fling still needs this much travel so a twitch does not turn the page. */
    const val FLING_MIN_TRAVEL_DP = 24f
    const val DIRECTION_BIAS = 1.5f

    /** Swiping is possible: connected, several top-level pages (pageCount is 0 inside a folder). */
    fun available(enabled: Boolean, pageCount: Int): Boolean = enabled && pageCount > 1

    /**
     * Page delta for a finished gesture: +1 (swipe left, next page), -1 (swipe right, previous), 0 none.
     * Distances in px, velocities in px/s, [density] converts dp to px. No wrap-around.
     */
    fun decide(
        dx: Float, dy: Float, vx: Float, vy: Float,
        gridWidth: Float, density: Float,
        pageIndex: Int, pageCount: Int
    ): Int {
        if (pageCount <= 1) return 0
        val dir = if (dx < 0f) 1 else -1
        val ax = abs(dx)
        val byDistance = ax >= max(MIN_TRAVEL_DP * density, TRAVEL_FRACTION * gridWidth) &&
            ax > DIRECTION_BIAS * abs(dy)
        val byFling = ax >= FLING_MIN_TRAVEL_DP * density &&
            abs(vx) >= FLING_DP_PER_S * density && abs(vx) > DIRECTION_BIAS * abs(vy) &&
            (vx < 0f) == (dx < 0f) && ax > DIRECTION_BIAS * abs(dy)
        if (!byDistance && !byFling) return 0
        val target = pageIndex + dir
        return if (target in 0 until pageCount) dir else 0
    }

    /** Finger-follow offset while dragging: a damped, clamped fraction of the travel. */
    fun followOffset(dx: Float, maxPx: Float): Float = (dx * 0.25f).coerceIn(-maxPx, maxPx)
}

/** Geometry of the media tile's seekable progress band. */
internal object MediaBand {
    const val HEIGHT_DP = 28f

    /** True when a touch at [y] (px from the tile top) is inside the bottom band of a tile [tileHeight] tall. */
    fun hit(y: Float, tileHeight: Float, bandHeight: Float): Boolean =
        tileHeight > 0f && y >= tileHeight - bandHeight && y <= tileHeight

    /** Fraction 0..1 of [x] across a bar starting at [barLeft] and [barWidth] wide. */
    fun fraction(x: Float, barLeft: Float, barWidth: Float): Float =
        if (barWidth <= 0f) 0f else ((x - barLeft) / barWidth).coerceIn(0f, 1f)
}

/**
 * Decides when a key press may be sent while page swiping is possible: KEY_DOWN is held back briefly so a
 * swipe that starts on a key never presses it. Pure; see [PressGesture] for the pointer plumbing.
 */
internal object PressGate {
    const val HOLD_MS = 80L

    enum class Result { Pending, Fire, Cancel }

    /** Horizontally dominant movement beyond touch slop. */
    fun isSwipeMove(dx: Float, dy: Float, slop: Float): Boolean = abs(dx) > slop && abs(dx) > abs(dy)

    /**
     * [elapsedMs] since touch-down, [dx]/[dy] pointer travel, [released] when the finger lifted.
     * Fire = send KEY_DOWN now (then KEY_UP at once if [released]); Cancel = drop the press entirely.
     */
    fun evaluate(available: Boolean, elapsedMs: Long, dx: Float, dy: Float, slop: Float, released: Boolean): Result = when {
        !available -> Result.Fire
        isSwipeMove(dx, dy, slop) -> Result.Cancel
        released -> Result.Fire
        elapsedMs >= HOLD_MS -> Result.Fire
        else -> Result.Pending
    }
}
