// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.ControlKind
import dev.notideck.proto.v1.ControlPatch
import dev.notideck.proto.v1.Feedback
import dev.notideck.proto.v1.FeedbackKind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Test

class FeedbackFlashTest {
    private val ids = listOf("a", "b")

    @Test
    fun okTargetsMatchingControl() {
        val r = FeedbackFlashes.apply(emptyMap(), Feedback(kind = FeedbackKind.FEEDBACK_KIND_SHOW_OK, control_id = "a"), ids, 1)
        assertEquals(mapOf("a" to Flash(FlashKind.OK, 1)), r)
    }

    @Test
    fun alertWithEmptyIdHitsAll() {
        val r = FeedbackFlashes.apply(emptyMap(), Feedback(kind = FeedbackKind.FEEDBACK_KIND_SHOW_ALERT), ids, 2)
        assertEquals(setOf("a", "b"), r.keys)
        assertEquals(FlashKind.ALERT, r["b"]!!.kind)
    }

    @Test
    fun repeatBumpsSeqAndOtherKindsIgnored() {
        val first = FeedbackFlashes.apply(emptyMap(), Feedback(kind = FeedbackKind.FEEDBACK_KIND_SHOW_OK, control_id = "a"), ids, 1)
        val second = FeedbackFlashes.apply(first, Feedback(kind = FeedbackKind.FEEDBACK_KIND_SHOW_ALERT, control_id = "a"), ids, 2)
        assertEquals(Flash(FlashKind.ALERT, 2), second["a"])
        assertSame(second, FeedbackFlashes.apply(second, Feedback(kind = FeedbackKind.FEEDBACK_KIND_HAPTIC, control_id = "a"), ids, 3))
        assertSame(second, FeedbackFlashes.apply(second, Feedback(kind = FeedbackKind.FEEDBACK_KIND_SHOW_OK, control_id = "zzz"), ids, 4))
    }

    @Test
    fun statePatchSwapsStateAndIcon() {
        val client = SessionClient()
        val base = Control(id = "k", kind = ControlKind.CONTROL_KIND_KEY, icon_hash = "off", state_index = 0, title = "Mic")
        val patch = Control(id = "k", icon_hash = "on", state_index = 1)
        val merged = client.mergeControl(base, patch, listOf("state_index", "icon_hash"))
        assertEquals(1, merged.state_index)
        assertEquals("on", merged.icon_hash)
        assertEquals("Mic", merged.title)
    }
}
