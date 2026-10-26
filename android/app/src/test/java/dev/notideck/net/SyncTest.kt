// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.PlayStream
import dev.notideck.proto.v1.Pong
import kotlinx.coroutines.channels.Channel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class SyncTest {
    @Test fun offsetZeroUntilSample() {
        val e = HostClockEstimator()
        assertEquals(0L, e.offsetMs)
        e.onPingSent(1, 1000)
        e.onPong(1, 0, 1010) // host_time 0 ignored
        assertEquals(0L, e.offsetMs)
        e.onPong(99, 5000, 1010) // unknown nonce ignored
        assertEquals(0L, e.offsetMs)
    }

    @Test fun picksMinRttSample() {
        val e = HostClockEstimator()
        e.onPingSent(1, 1000); e.onPong(1, 6100, 1200)   // rtt 200, offset 6100-1100=5000
        assertEquals(5000L, e.offsetMs)
        e.onPingSent(2, 2000); e.onPong(2, 7010, 2020)   // rtt 20, offset 7010-2010=5000... use different
        assertEquals(5000L, e.offsetMs)
        e.onPingSent(3, 3000); e.onPong(3, 8100, 3400)   // rtt 400 worse -> ignored
        assertEquals(5000L, e.offsetMs)
        e.onPingSent(4, 4000); e.onPong(4, 9006, 4010)   // rtt 10, offset 9006-4005=5001
        assertEquals(5001L, e.offsetMs)
    }

    @Test fun windowKeepsLast16() {
        val e = HostClockEstimator()
        e.onPingSent(0, 0); e.onPong(0, 1000, 2) // rtt 2, offset 999
        for (i in 1..16) { e.onPingSent(i.toLong(), i * 100L); e.onPong(i.toLong(), i * 100L + 50 + 500, i * 100L + 100) }
        // first (best) sample evicted; remaining offsets all 500
        assertEquals(500L, e.offsetMs)
    }

    @Test fun clientUsesPong() {
        val c = SessionClient()
        var wall = 1000L
        c.wallClock = { wall }
        val ch = Channel<Envelope>(Channel.BUFFERED)
        // no ping recorded -> pong ignored
        c.handleIncomingEnvelope(Envelope(pong = Pong(nonce = 5, host_time_ms = 9999)), ch)
        assertEquals(0L, c.hostClockOffsetMs)
    }

    @Test fun controllerRules() {
        val c = SyncController()
        assertEquals(SyncController.Action.None, c.update(10))
        // ahead: hold the picture, then ignore a few ticks while the player settles
        assertEquals(SyncController.Action.Hold(1500), c.update(1500))
        repeat(SyncController.COOLDOWN_TICKS) { assertEquals(SyncController.Action.None, c.update(5000)) }
        // behind: jump forward, but only inside the buffer
        assertEquals(SyncController.Action.Seek(2000), c.update(-2000, headroomMs = 10_000))
        repeat(SyncController.COOLDOWN_TICKS) { c.update(0) }
        assertEquals(SyncController.Action.Seek(1300), c.update(-2000, headroomMs = 2000))
        repeat(SyncController.COOLDOWN_TICKS) { c.update(0) }
        // behind at the live edge: nothing to jump into, play on
        assertEquals(SyncController.Action.None, c.update(-5000, headroomMs = 500))
        val r = c.update(400) as SyncController.Action.Rate
        assertEquals(0.95f, r.rate, 1e-6f) // clamped to 0.95
        assertEquals(SyncController.Action.None, c.update(30)) // hysteresis keeps rate
        assertEquals(0.95f, c.rate, 1e-6f)
        assertEquals(SyncController.Action.Rate(1f), c.update(5))
        val s = c.update(-100) as SyncController.Action.Rate
        assertEquals(1.025f, s.rate, 1e-6f)
        val m = c.update(80) as SyncController.Action.Rate
        assertEquals(0.98f, m.rate, 1e-6f)
        // no speed-up with a thin buffer (it would drain and rebuffer)
        assertEquals(SyncController.Action.Rate(1f), c.update(-100, headroomMs = 1000))
        assertTrue(c.update(-1001) is SyncController.Action.Seek)
        assertEquals(1f, c.rate, 0f)
    }

    @Test fun requestMapsSyncFieldsAndPlayerClosed() {
        val r = PlayerRequest.from(PlayStream(url = "https://x/y.m3u8", muted = true, sync_delay_ms = 8000, session_id = "demo/1"), 1)!!
        assertTrue(r.muted); assertEquals(8000, r.syncDelayMs); assertEquals("demo/1", r.sessionId)
        val c = SessionClient()
        val ch = Channel<Envelope>(Channel.BUFFERED)
        c.handleIncomingEnvelope(Envelope(play_stream = PlayStream(url = "https://x/y.m3u8", session_id = "demo/1")), ch)
        c.userClosedPlayer() // no active session channel: dropped silently
        assertEquals(null, c.playRequest.value)
    }
}
