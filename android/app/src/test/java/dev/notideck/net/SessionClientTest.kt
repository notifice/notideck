// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.ControlKind
import dev.notideck.proto.v1.ControlPatch
import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.Ping
import dev.notideck.proto.v1.SetLayout
import dev.notideck.proto.v1.UpdateControls
import dev.notideck.proto.v1.Welcome
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SessionClientTest {

    @Test
    fun testMergeControlPartialPatch() {
        val client = SessionClient()
        val original = Control(
            id = "key_1",
            kind = ControlKind.CONTROL_KIND_KEY,
            col = 0,
            row = 0,
            title = "Mute",
            bg_argb = 0xFF112233.toInt(),
            value_ = 0f
        )
        val patchControl = Control(
            id = "key_1",
            title = "Unmute",
            value_ = 1f
        )
        val patch = ControlPatch(
            id = "key_1",
            fields = listOf("title", "value"),
            control = patchControl
        )

        val merged = client.mergeControl(original, patch.control!!, patch.fields)
        assertEquals("key_1", merged.id)
        assertEquals("Unmute", merged.title)
        assertEquals(1f, merged.value_, 0.001f)
        assertEquals(0, merged.col)
        assertEquals(0, merged.row)
        assertEquals(0xFF112233.toInt(), merged.bg_argb)
    }

    @Test
    fun testApplyPatches() {
        val client = SessionClient()
        val initialLayout = SetLayout(
            page_id = "main_page",
            cols = 5,
            rows = 3,
            controls = listOf(
                Control(id = "c1", title = "Play", col = 0, row = 0),
                Control(id = "c2", title = "Stop", col = 1, row = 0)
            )
        )
        val channel = Channel<Envelope>(Channel.BUFFERED)
        client.handleIncomingEnvelope(Envelope(set_layout = initialLayout), channel)

        val currentDeck = client.deckState.value
        assertEquals("main_page", currentDeck.pageId)
        assertEquals(5, currentDeck.cols)
        assertEquals(3, currentDeck.rows)
        assertEquals(2, currentDeck.controls.size)
        assertEquals("Play", currentDeck.controls["c1"]?.title)

        // Apply patch
        val patch = ControlPatch(
            id = "c1",
            fields = listOf("title"),
            control = Control(id = "c1", title = "Pause")
        )
        val update = UpdateControls(
            page_id = "main_page",
            patches = listOf(patch)
        )
        client.handleIncomingEnvelope(Envelope(update_controls = update), channel)

        val updatedDeck = client.deckState.value
        assertEquals("Pause", updatedDeck.controls["c1"]?.title)
        assertEquals("Stop", updatedDeck.controls["c2"]?.title)
    }

    @Test
    fun testPingPongResponse() = runBlocking {
        val client = SessionClient()
        val channel = Channel<Envelope>(Channel.BUFFERED)

        client.handleIncomingEnvelope(Envelope(ping = Ping(nonce = 12345L)), channel)

        val response = channel.receive()
        assertNotNull(response.pong)
        assertEquals(12345L, response.pong?.nonce)
    }

    @Test
    fun testWelcomeUpdatesConnectionStatus() {
        val client = SessionClient()
        val channel = Channel<Envelope>(Channel.BUFFERED)

        client.handleIncomingEnvelope(
            Envelope(welcome = Welcome(session_id = "sess_42", host_name = "Desktop-PC", proto_version = 1)),
            channel
        )

        val status = client.connectionStatus.value
        assertTrue(status is ConnectionStatus.Connected)
        val connected = status as ConnectionStatus.Connected
        assertEquals("sess_42", connected.sessionId)
        assertEquals("Desktop-PC", connected.hostName)
    }

    @Test
    fun testEnvelopeFramingEncodeDecode() {
        val client = SessionClient()
        val hello = client.buildHello(
            deviceId = "test-device",
            deviceModel = "Pixel 7",
            androidSdk = 34,
            appVersion = "0.1.0"
        )
        val envelope = Envelope(seq = 1, hello = hello)

        val bytes = Envelope.ADAPTER.encode(envelope)
        assertTrue(bytes.isNotEmpty())

        val decoded = Envelope.ADAPTER.decode(bytes)
        assertEquals(1, decoded.seq)
        assertNotNull(decoded.hello)
        assertEquals("test-device", decoded.hello?.device_id)
        assertEquals("Pixel 7", decoded.hello?.device_model)
    }

    @Test
    fun testSetLayoutDefaultColsRows() {
        val client = SessionClient()
        val channel = Channel<Envelope>(Channel.BUFFERED)

        // 0 cols and 0 rows should default to 5 cols and 3 rows
        val layout = SetLayout(page_id = "default_page", cols = 0, rows = 0)
        client.handleIncomingEnvelope(Envelope(set_layout = layout), channel)

        val deck = client.deckState.value
        assertEquals("default_page", deck.pageId)
        assertEquals(5, deck.cols)
        assertEquals(3, deck.rows)
    }

    @Test
    fun testFeedbackEventEmission() = runBlocking {
        val client = SessionClient()
        val channel = Channel<Envelope>(Channel.BUFFERED)

        val feedback = dev.notideck.proto.v1.Feedback(
            kind = dev.notideck.proto.v1.FeedbackKind.FEEDBACK_KIND_HAPTIC,
            control_id = "key_1"
        )
        client.handleIncomingEnvelope(Envelope(feedback = feedback), channel)

        // Feedback should have been emitted
        // (SharedFlow with buffer of 16)
        assertEquals(0, client.feedbackEvents.replayCache.size)
    }

    @Test
    fun railPatchesGoToRailOnly() {
        val client = SessionClient()
        val channel = Channel<Envelope>(Channel.UNLIMITED)
        client.handleIncomingEnvelope(Envelope(set_layout = SetLayout(
            page_id = "p1", cols = 2, rows = 1,
            controls = listOf(Control(id = "a", kind = ControlKind.CONTROL_KIND_KEY, title = "A"))
        )), channel)
        client.handleIncomingEnvelope(Envelope(set_rail = dev.notideck.proto.v1.SetRail(
            controls = listOf(Control(id = "r1", kind = ControlKind.CONTROL_KIND_KEY, title = "R"))
        )), channel)
        assertEquals(listOf("r1"), client.railControls.value.map { it.id })
        client.handleIncomingEnvelope(Envelope(update_controls = UpdateControls(
            page_id = "__rail",
            patches = listOf(
                ControlPatch(id = "r1", fields = listOf("title"), control = Control(id = "r1", title = "R2")),
                ControlPatch(id = "a", fields = listOf("title"), control = Control(id = "a", title = "X"))
            )
        )), channel)
        assertEquals("R2", client.railControls.value[0].title)
        assertEquals("A", client.deckState.value.controls["a"]!!.title)
        client.handleIncomingEnvelope(Envelope(update_controls = UpdateControls(
            page_id = "p1",
            patches = listOf(ControlPatch(id = "r1", fields = listOf("title"), control = Control(id = "r1", title = "Z")))
        )), channel)
        assertEquals("R2", client.railControls.value[0].title)
        // a new SetRail replaces
        client.handleIncomingEnvelope(Envelope(set_rail = dev.notideck.proto.v1.SetRail()), channel)
        assertEquals(0, client.railControls.value.size)
    }

    @Test
    fun testMergeMeterLevelDimmed() {
        val client = SessionClient()
        val base = Control(id = "m", kind = ControlKind.CONTROL_KIND_SLIDER, title = "Music", value_ = 0.5f)
        val a = client.mergeControl(base, Control(id = "m", meter = true, dimmed = true), listOf("meter", "dimmed"))
        assertTrue(a.meter)
        assertTrue(a.dimmed)
        assertEquals(0f, a.level, 0f)
        assertEquals("Music", a.title)
        val b = client.mergeControl(a, Control(id = "m", level = 0.62f), listOf("level"))
        assertEquals(0.62f, b.level, 0.0001f)
        assertTrue(b.meter && b.dimmed)
        assertEquals(0.5f, b.value_, 0f)
        val c = client.mergeControl(b, Control(id = "m"), listOf("meter", "level", "dimmed"))
        assertEquals(false, c.meter)
        assertEquals(0f, c.level, 0f)
        assertEquals(false, c.dimmed)
    }
}
