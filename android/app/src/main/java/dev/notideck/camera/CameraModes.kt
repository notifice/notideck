// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import dev.notideck.proto.v1.CameraMode
import dev.notideck.proto.v1.Lens
import kotlin.math.abs
import kotlin.math.ln

/** A landscape size the camera and the encoder both support, with the usable frame rates. */
data class SizeCaps(val width: Int, val height: Int, val fps: List<Int>)

/** Pure helpers for building and searching the mode list, kept free of Android types for unit tests. */
object CameraModes {
    const val MAX_W = 1920
    const val MAX_H = 1080
    const val MAX_MODES_PER_LENS = 12
    val FPS_CANDIDATES = listOf(60, 30, 24, 15)

    /** Sizes we always want in the list when the hardware has them (or something close). */
    private val ANCHORS = listOf(1920 to 1080, 1280 to 720, 640 to 360)

    /**
     * Filters (landscape, <= 1920x1080, has fps), dedupes, and trims to [MAX_MODES_PER_LENS]
     * entries: the anchor sizes first, then the largest remaining sizes.
     */
    fun select(sizes: List<SizeCaps>, max: Int = MAX_MODES_PER_LENS): List<SizeCaps> {
        val ok = sizes
            .filter { it.width >= it.height && it.width <= MAX_W && it.height <= MAX_H && it.fps.isNotEmpty() }
            .groupBy { it.width to it.height }
            .map { (k, v) -> SizeCaps(k.first, k.second, v.flatMap { it.fps }.distinct().sortedDescending()) }
        if (ok.size <= max) return ok.sortedByDescending { it.width * it.height }
        val chosen = LinkedHashSet<SizeCaps>()
        for ((aw, ah) in ANCHORS) {
            // exact 16:9 anchor, else the closest "class" size by area among 16:9-ish ones
            val exact = ok.firstOrNull { it.width == aw && it.height == ah }
            val near = exact ?: ok.filter { it.width.toDouble() / it.height > 1.5 }
                .minByOrNull { abs(ln(it.width.toDouble() * it.height / (aw * ah))) }
            if (near != null) chosen.add(near)
        }
        for (s in ok.sortedByDescending { it.width * it.height }) {
            if (chosen.size >= max) break
            chosen.add(s)
        }
        return chosen.sortedByDescending { it.width * it.height }
    }

    fun toProto(lens: Lens, sizes: List<SizeCaps>): List<CameraMode> =
        select(sizes).map { CameraMode(lens = lens, width = it.width, height = it.height, fps = it.fps) }

    /** What we actually run: a lens plus a mode from the list. */
    data class Choice(val lens: Lens, val width: Int, val height: Int, val fps: Int)

    /**
     * Closest supported mode to the request. Prefers the requested lens (falls back to any lens that
     * has modes). Size cost: log area ratio plus aspect mismatch; fps: closest, lower on ties.
     */
    fun pickClosest(width: Int, height: Int, fps: Int, lens: Lens, modes: List<CameraMode>): Choice? {
        if (modes.isEmpty()) return null
        val wantLens = if (lens == Lens.LENS_UNSPECIFIED) Lens.LENS_BACK else lens
        val pool = modes.filter { it.lens == wantLens && it.fps.isNotEmpty() }
            .ifEmpty { modes.filter { it.fps.isNotEmpty() } }
        if (pool.isEmpty()) return null
        val w = if (width > 0) width else 1280
        val h = if (height > 0) height else 720
        val want = w.toDouble() / h
        val best = pool.minWith(
            compareBy<CameraMode> {
                abs(ln(it.width.toDouble() * it.height / (w.toDouble() * h))) +
                    2.0 * abs(ln((it.width.toDouble() / it.height) / want))
            }.thenBy { -(it.width * it.height) }
        )
        val f = if (fps > 0) fps else 30
        val bestFps = best.fps.minWith(compareBy<Int> { abs(it - f) }.thenBy { it })
        return Choice(best.lens, best.width, best.height, bestFps)
    }

    /** Thermal step-down target: at most 1280x720 at 24 fps (or the closest the lens has). */
    fun throttled(c: Choice, modes: List<CameraMode>): Choice {
        val w = if (c.width * c.height > 1280 * 720) 1280 else c.width
        val h = if (c.width * c.height > 1280 * 720) 720 else c.height
        val picked = pickClosest(w, h, minOf(c.fps, 24), c.lens, modes.filter { it.lens == c.lens }) ?: return c
        // pickClosest may round the size up past the cap; take the best mode inside the cap instead.
        if (picked.width * picked.height <= 1280 * 720) return picked
        val smaller = modes.filter { it.lens == c.lens && it.width * it.height <= 1280 * 720 }
        return pickClosest(1280, 720, 24, c.lens, smaller) ?: picked
    }
}

/** Heat policy (spec: >= 42 C throttle, >= 46 C stop). */
object ThermalPolicy {
    const val THROTTLE_C = 42f
    const val STOP_C = 46f

    enum class Action { None, Throttle, Stop }

    /** [thermalStatus] is PowerManager.THERMAL_STATUS_* (or -1 unknown). Severe ~ 42 C, critical ~ 46 C. */
    fun decide(tempC: Float, thermalStatus: Int, alreadyThrottled: Boolean): Action = when {
        tempC >= STOP_C || thermalStatus >= 4 -> Action.Stop
        (tempC >= THROTTLE_C || thermalStatus == 3) && !alreadyThrottled -> Action.Throttle
        else -> Action.None
    }

    const val RECOVER_C = 40f
    const val RECOVER_MS = 5 * 60_000L

    /**
     * Step-up hysteresis: after a throttle, the requested mode comes back only once the phone has
     * stayed below [RECOVER_C] (and the OS reports no severe thermal status) for [RECOVER_MS].
     */
    class StepUpTracker(private val clock: () -> Long = System::currentTimeMillis) {
        private var coolSince = -1L
        fun reset() { coolSince = -1L }

        /** Feed a reading while throttled; true once it is time to go back to the requested mode. */
        fun update(tempC: Float, thermalStatus: Int): Boolean {
            if (tempC >= RECOVER_C || thermalStatus >= 3) { coolSince = -1L; return false }
            val now = clock()
            if (coolSince < 0) { coolSince = now; return false }
            return now - coolSince >= RECOVER_MS
        }
    }
}
