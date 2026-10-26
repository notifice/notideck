// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.mic

import dev.notideck.camera.MediaPacket
import dev.notideck.proto.v1.MicStart
import dev.notideck.ui.planRail
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MicUnitTest {
    @Test fun audioHeaderHasNoFlagsAndBigEndianPtsAndLength() {
        val h = MediaPacket.header(0x0102030405L, config = false, keyFrame = false, len = 1920)
        assertArrayEquals(
            byteArrayOf(0, 0, 0, 1, 2, 3, 4, 5, 0, 0, 7, 0x80.toByte()),
            h
        )
        assertEquals(0, h[0].toInt() and 0xC0)
    }

    @Test fun frameMath() {
        assertEquals(960, MicAudio.frameSamples(48_000, 20))
        assertEquals(1920, MicAudio.frameBytes(48_000, 20))
        assertEquals(441, MicAudio.frameSamples(44_100, 10))
        assertEquals(640, MicAudio.frameSamples(16_000, 40))
        assertEquals(20_000L, MicAudio.samplesToUs(960, 48_000))
    }

    @Test fun frameMsClamps() {
        assertEquals(20, MicAudio.frameMs(0))
        assertEquals(10, MicAudio.frameMs(3))
        assertEquals(40, MicAudio.frameMs(500))
        assertEquals(30, MicAudio.frameMs(30))
    }

    @Test fun rateCandidatesFallBack() {
        assertEquals(listOf(48_000, 44_100, 16_000), MicAudio.rateCandidates(48_000))
        assertEquals(listOf(48_000, 44_100, 16_000), MicAudio.rateCandidates(0))
        assertEquals(listOf(22_050, 48_000, 44_100, 16_000), MicAudio.rateCandidates(22_050))
        assertEquals(listOf(44_100, 48_000, 16_000), MicAudio.rateCandidates(44_100))
    }

    @Test fun pcmIsLittleEndian() {
        val b = MicAudio.packPcm16(shortArrayOf(1, -1, 0x1234, Short.MIN_VALUE, 99), count = 4)
        assertArrayEquals(byteArrayOf(1, 0, -1, -1, 0x34, 0x12, 0, 0x80.toByte()), b)
    }

    @Test fun ptsTrackerSpacesFramesEvenlyAndResyncsAfterStall() {
        val t = PtsTracker(48_000)
        val first = t.next(1_000_000, 960)
        assertEquals(980_000, first)
        // jittery arrival times inside the tolerance do not disturb the spacing
        assertEquals(1_000_000, t.next(1_025_000, 960))
        assertEquals(1_020_000, t.next(1_038_000, 960))
        // a long stall re-anchors on the clock
        assertEquals(2_980_000, t.next(3_000_000, 960))
        assertEquals(3_000_000, t.next(3_020_000, 960))
    }

    private fun frame(pts: Long, ms: Int = 20) = AudioFrame(pts, ByteArray(4), ms * 1000L)

    @Test fun dropQueueKeepsOrderUnderTheLimit() {
        val q = AudioDropQueue(200_000)
        for (i in 0 until 10) q.offer(frame(i * 20_000L))
        assertEquals(10, q.size)
        assertEquals(0, q.dropped)
        assertEquals(0L, q.take()!!.ptsUs)
    }

    @Test fun dropQueueDropsOldestWhenBackedUp() {
        val q = AudioDropQueue(200_000)
        for (i in 0 until 14) q.offer(frame(i * 20_000L))
        assertEquals(10, q.size)
        assertEquals(4, q.dropped)
        assertEquals(80_000L, q.take()!!.ptsUs)   // frames 0..3 were dropped
    }

    @Test fun dropQueueKeepsAtLeastTheNewestFrame() {
        val q = AudioDropQueue(10_000)
        q.offer(frame(0)); q.offer(frame(20_000))
        assertEquals(1, q.size)
        assertEquals(20_000L, q.take()!!.ptsUs)
    }

    @Test fun dropQueueDrainsThenEndsAfterClose() {
        val q = AudioDropQueue()
        q.offer(frame(1))
        q.close()
        q.offer(frame(2))   // ignored once closed
        assertEquals(1L, q.take()!!.ptsUs)
        assertNull(q.take())
    }

    private val m = MicStart(sample_rate = 48_000, frame_ms = 20, voice_processing = true)

    @Test fun missingPermissionWhileVisibleAsksNow() {
        val l = MicRequestLogic()
        assertEquals(MicPlan.NeedPermission(true), l.onStart(m, granted = false, visible = true, canRunInBackground = false))
        assertEquals(m, l.pending)
    }

    @Test fun missingPermissionInBackgroundAsksOnNextResumeOnce() {
        val l = MicRequestLogic()
        assertEquals(MicPlan.NeedPermission(false), l.onStart(m, false, visible = false, canRunInBackground = false))
        assertEquals(MicVisibleAction(null, true), l.onVisible(granted = false))
        // dialog denied: dropped, no more asking
        assertNull(l.onPermissionResult(false))
        assertEquals(MicVisibleAction(null, false), l.onVisible(granted = false))
    }

    @Test fun grantedWhilePendingStartsIt() {
        val l = MicRequestLogic()
        l.onStart(m, false, true, false)
        assertEquals(m, l.onPermissionResult(true))
        assertNull(l.pending)
    }

    @Test fun grantedInSettingsStartsOnResume() {
        val l = MicRequestLogic()
        l.onStart(m, false, false, false)
        assertEquals(MicVisibleAction(m, false), l.onVisible(granted = true))
        assertNull(l.pending)
    }

    @Test fun backgroundWithoutMicServiceWaitsForTheApp() {
        val l = MicRequestLogic()
        assertEquals(MicPlan.NeedForeground, l.onStart(m, true, visible = false, canRunInBackground = false))
        assertEquals(MicVisibleAction(m, false), l.onVisible(true))
        assertEquals(MicPlan.Begin, l.onStart(m, true, visible = false, canRunInBackground = true))
    }

    @Test fun stopClearsPendingWork() {
        val l = MicRequestLogic()
        l.onStart(m, false, true, false)
        l.onStop()
        assertNull(l.onPermissionResult(true))
        assertEquals(MicVisibleAction(null, false), l.onVisible(true))
    }

    @Test fun railReservesRoomForTheMicChip() {
        // camera chip + 3 keys + Back + link = 338 dp; the 44 dp mic chip + 4 dp gap pushes the link out of 339..385
        assertTrue(planRail(340f, 3, true, true, true).showStatus)
        assertFalse(planRail(340f, 3, true, true, true, micLive = true).showStatus)
        assertTrue(planRail(390f, 3, true, true, true, micLive = true).showStatus)
        assertTrue(planRail(390f, 3, true, true, false, micLive = true).showStatus)
    }
}
