// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.ControlPatch
import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.PutIcon
import dev.notideck.proto.v1.SetLayout
import dev.notideck.proto.v1.UpdateControls
import kotlinx.coroutines.channels.Channel
import okio.ByteString.Companion.toByteString
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class IconPipelineTest {

    /** Fake "image" is just the raw bytes; empty/"bad" payloads fail to decode. */
    private fun cache(maxBytes: Long = 1024) = IconCache<ByteArray>(
        maxBytes = maxBytes,
        decoder = IconDecoder { if (it.contentEquals("bad".toByteArray())) null else it },
        sizeOf = { it.size }
    )

    @Test
    fun putStoresAndPublishesSnapshot() {
        val c = cache()
        assertTrue(c.put("h1", "image/png", ByteArray(10)))
        assertTrue(c.contains("h1"))
        assertEquals(setOf("h1"), c.icons.value.keys)
    }

    @Test
    fun decodeFailureIsNotCached() {
        val c = cache()
        assertFalse(c.put("h1", "image/png", "bad".toByteArray()))
        assertFalse(c.contains("h1"))
        assertFalse(c.put("", "image/png", ByteArray(4)))
        assertFalse(c.put("h2", "image/png", ByteArray(0)))
    }

    @Test
    fun evictsLeastRecentlyUsedOverBudget() {
        val c = cache(maxBytes = 25)
        c.put("a", "image/png", ByteArray(10))
        c.put("b", "image/png", ByteArray(10))
        c.put("a", "image/png", ByteArray(10)) // re-put refreshes recency
        c.put("c", "image/png", ByteArray(10)) // total 30 > 25, evict eldest = b
        assertTrue(c.contains("a"))
        assertFalse(c.contains("b"))
        assertTrue(c.contains("c"))
        assertEquals(20L, c.sizeBytes())
    }

    @Test
    fun newestEntryKeptEvenIfOversized() {
        val c = cache(maxBytes = 5)
        assertTrue(c.put("big", "image/png", ByteArray(10)))
        assertTrue(c.contains("big"))
    }

    @Test
    fun setLayoutRequestsMissingIconsBatchedAndDeduped() {
        val client = SessionClient()
        val c = cache()
        client.iconSink = c
        c.put("have", "image/png", ByteArray(4))
        val channel = Channel<Envelope>(Channel.BUFFERED)

        val layout = SetLayout(
            page_id = "p",
            cols = 5,
            rows = 3,
            controls = listOf(
                Control(id = "1", icon_hash = "have"),
                Control(id = "2", icon_hash = "x"),
                Control(id = "3", icon_hash = "y"),
                Control(id = "4", icon_hash = "x"),
                Control(id = "5")
            )
        )
        client.handleIncomingEnvelope(Envelope(set_layout = layout), channel)

        val req = channel.tryReceive().getOrNull()
        assertNotNull(req?.need_icon)
        assertEquals(listOf("x", "y"), req!!.need_icon!!.hashes)

        // Re-sending the same layout must not re-request pending hashes.
        client.handleIncomingEnvelope(Envelope(set_layout = layout), channel)
        assertNull(channel.tryReceive().getOrNull())
    }

    @Test
    fun updateControlsRequestsNewIcon() {
        val client = SessionClient()
        client.iconSink = cache()
        val channel = Channel<Envelope>(Channel.BUFFERED)
        client.handleIncomingEnvelope(
            Envelope(set_layout = SetLayout(page_id = "p", controls = listOf(Control(id = "1")))),
            channel
        )
        assertNull(channel.tryReceive().getOrNull())

        val update = UpdateControls(
            page_id = "p",
            patches = listOf(
                ControlPatch(id = "1", fields = listOf("icon_hash"), control = Control(id = "1", icon_hash = "new"))
            )
        )
        client.handleIncomingEnvelope(Envelope(update_controls = update), channel)
        assertEquals(listOf("new"), channel.tryReceive().getOrNull()?.need_icon?.hashes)
    }

    @Test
    fun putIconStoresAndClearsPending() {
        val client = SessionClient()
        val c = cache()
        client.iconSink = c
        val channel = Channel<Envelope>(Channel.BUFFERED)
        val layout = SetLayout(page_id = "p", controls = listOf(Control(id = "1", icon_hash = "x")))
        client.handleIncomingEnvelope(Envelope(set_layout = layout), channel)
        channel.tryReceive()

        val bytes = byteArrayOf(1, 2, 3)
        client.handleIncomingEnvelope(
            Envelope(put_icon = PutIcon(hash = "x", mime_type = "image/png", data_ = bytes.toByteString())),
            channel
        )
        assertTrue(c.contains("x"))

        // Once cached, a repeated layout needs nothing.
        client.handleIncomingEnvelope(Envelope(set_layout = layout), channel)
        assertNull(channel.tryReceive().getOrNull())
    }

    @Test
    fun noSinkMeansNoRequests() {
        val client = SessionClient()
        val channel = Channel<Envelope>(Channel.BUFFERED)
        val layout = SetLayout(page_id = "p", controls = listOf(Control(id = "1", icon_hash = "x")))
        client.handleIncomingEnvelope(Envelope(set_layout = layout), channel)
        assertNull(channel.tryReceive().getOrNull())
    }
}
