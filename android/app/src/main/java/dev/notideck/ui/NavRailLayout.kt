// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

/** Fixed sizes (dp) the side rail lays out with; shared by the composable and the fit decision. */
internal object RailDims {
    const val PAD = 8f
    const val GROUP_GAP = 6f
    const val HOME = 48f
    const val BACK = 48f
    const val KEY = 44f
    const val KEY_GAP = 4f
    /** In-use chip (camera / mic) side and the gap between two chips. */
    const val CHIP = 44f
    const val CHIP_GAP = 4f
    const val LINK = 18f
    const val CLOCK = 14f
}

/** Which rail items fit. Home is always shown. */
data class RailPlan(
    val showCameraChip: Boolean,
    val showMicChip: Boolean,
    val pinnedShown: Int,
    val showBack: Boolean,
    val showStatus: Boolean,
    val showClock: Boolean
)

/**
 * Decides what fits in [availableDp] of rail height. Priority: Home > in-use chips (camera, then mic) >
 * pinned keys > Back > link status > clock. Items are added in that order and the first one that does not
 * fit ends the list, so a lower-priority item never appears while a higher one is missing.
 */
fun planRail(
    availableDp: Float,
    pinned: Int,
    canGoBack: Boolean,
    hasLink: Boolean,
    cameraLive: Boolean,
    micLive: Boolean = false
): RailPlan {
    var used = RailDims.PAD * 2 + RailDims.HOME
    var open = true
    fun take(h: Float, gap: Float): Boolean {
        if (!open) return false
        if (used + gap + h > availableDp) { open = false; return false }
        used += gap + h
        return true
    }

    var chips = 0
    val cameraChip = cameraLive && take(RailDims.CHIP, RailDims.GROUP_GAP).also { if (it) chips++ }
    val micChip = micLive && take(RailDims.CHIP, if (chips > 0) RailDims.CHIP_GAP else RailDims.GROUP_GAP)
    var keys = 0
    for (i in 0 until pinned) {
        if (take(RailDims.KEY, if (i == 0) RailDims.GROUP_GAP else RailDims.KEY_GAP)) keys++ else break
    }
    val back = canGoBack && take(RailDims.BACK, RailDims.GROUP_GAP)
    val status = hasLink && take(RailDims.LINK, RailDims.GROUP_GAP)
    val clock = take(RailDims.CLOCK, RailDims.GROUP_GAP)
    return RailPlan(cameraChip, micChip, keys, back, status, clock)
}
