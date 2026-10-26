// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.Capability
import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.PlayStream
import kotlinx.coroutines.channels.Channel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PlayerRequestTest {
    @Test fun mapsFields() {
        val r = PlayerRequest.from(PlayStream(url = "https://x/y.m3u8", title = "chan", subtitle = "t", max_height = 480), 7)!!
        assertEquals(PlayerRequest(7, "https://x/y.m3u8", "chan", "t", 480), r)
    }

    @Test fun emptyOrNonHttpUrlIsNull() {
        assertNull(PlayerRequest.from(PlayStream(url = ""), 1))
        assertNull(PlayerRequest.from(PlayStream(url = "  "), 1))
        assertNull(PlayerRequest.from(PlayStream(url = "file:///etc/passwd"), 1))
    }

    @Test fun clientTracksPlayStreamAndClose() {
        val client = SessionClient()
        val ch = Channel<Envelope>(Channel.BUFFERED)
        assertNull(client.playRequest.value)
        client.handleIncomingEnvelope(Envelope(play_stream = PlayStream(url = "https://a/1.m3u8", title = "A")), ch)
        val first = client.playRequest.value!!
        assertEquals("A", first.title)
        // Same URL again is a fresh request (restart).
        client.handleIncomingEnvelope(Envelope(play_stream = PlayStream(url = "https://a/1.m3u8", title = "A")), ch)
        assertNotEquals(first.id, client.playRequest.value!!.id)
        // Empty url closes.
        client.handleIncomingEnvelope(Envelope(play_stream = PlayStream(url = "")), ch)
        assertNull(client.playRequest.value)
        client.handleIncomingEnvelope(Envelope(play_stream = PlayStream(url = "https://a/2.m3u8")), ch)
        client.closePlayer()
        assertNull(client.playRequest.value)
    }

    @Test fun helloAdvertisesPlayer() {
        assertTrue(SessionClient().capabilities.contains(Capability.CAPABILITY_PLAYER))
    }
}
