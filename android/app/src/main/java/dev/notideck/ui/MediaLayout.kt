// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

/** Which arrangement the media tile face uses. */
internal enum class MediaLayout { Wide, Tall, Mini }

/**
 * Picks the media face layout from the grid span and the measured tile size.
 * Single-cell tiles show art only; tall (span_h >= 2, roughly square or taller) stacks art above text;
 * everything else with room for text is the wide row layout.
 */
internal fun mediaLayoutFor(spanW: Int, spanH: Int, widthDp: Float, heightDp: Float): MediaLayout {
    val w = maxOf(spanW, 1)
    val h = maxOf(spanH, 1)
    return when {
        w < 2 && h < 2 -> MediaLayout.Mini
        h >= 2 && widthDp > 0f && heightDp >= widthDp * 0.9f -> MediaLayout.Tall
        w >= 2 -> MediaLayout.Wide
        else -> MediaLayout.Tall
    }
}
