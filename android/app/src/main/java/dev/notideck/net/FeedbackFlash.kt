// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.Feedback
import dev.notideck.proto.v1.FeedbackKind

enum class FlashKind { OK, ALERT }

/** A pending flash on one control. [seq] changes on every event so repeats retrigger the animation. */
data class Flash(val kind: FlashKind, val seq: Long)

/** Pure reducer turning host Feedback messages into per-control flashes. */
object FeedbackFlashes {
    /**
     * Applies [feedback] to [current]. SHOW_OK / SHOW_ALERT target [Feedback.control_id], or every
     * control in [controlIds] when it is empty. Unknown ids and other kinds leave [current] unchanged.
     */
    fun apply(
        current: Map<String, Flash>,
        feedback: Feedback,
        controlIds: Collection<String>,
        seq: Long
    ): Map<String, Flash> {
        val kind = when (feedback.kind) {
            FeedbackKind.FEEDBACK_KIND_SHOW_OK -> FlashKind.OK
            FeedbackKind.FEEDBACK_KIND_SHOW_ALERT -> FlashKind.ALERT
            else -> return current
        }
        val targets = if (feedback.control_id.isEmpty()) controlIds.toList()
        else controlIds.filter { it == feedback.control_id }
        if (targets.isEmpty()) return current
        val flash = Flash(kind, seq)
        return current + targets.associateWith { flash }
    }
}
