// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/** One media packet, ready for the wire (ADR 0005, `nd_proto::media`). Payload is H.264 Annex-B. */
class MediaPacket(val ptsUs: Long, val config: Boolean, val keyFrame: Boolean, val data: ByteArray) {
    companion object {
        const val HEADER_LEN = 12
        private const val FLAG_CONFIG = 1L shl 63
        private const val FLAG_KEY_FRAME = 1L shl 62
        private const val PTS_MASK = FLAG_KEY_FRAME - 1

        /** 12-byte big-endian header: u64 (bit63 CONFIG, bit62 KEY_FRAME, low 62 bits PTS us) then u32 length. */
        fun header(ptsUs: Long, config: Boolean, keyFrame: Boolean, len: Int): ByteArray {
            var v = ptsUs and PTS_MASK
            if (config) v = v or FLAG_CONFIG
            if (keyFrame) v = v or FLAG_KEY_FRAME
            val out = ByteArray(HEADER_LEN)
            for (i in 0 until 8) out[i] = (v ushr (56 - 8 * i)).toByte()
            for (i in 0 until 4) out[8 + i] = (len ushr (24 - 8 * i)).toByte()
            return out
        }
    }

    fun header(): ByteArray = header(ptsUs, config, keyFrame, data.size)
}

/**
 * Latest-wins outgoing queue between the encoder thread and the socket writer.
 *
 * Policy: when the queued frames span more than [maxQueuedUs] of video (or [maxBytes]), the network
 * cannot keep up. New delta frames are dropped, and so is everything until the next key frame
 * (later deltas would reference dropped frames). [offer] reports `true` when the caller should ask
 * the encoder for a sync frame. A key frame arriving while backed up replaces the stale queue,
 * keeping only CONFIG packets. The queue never grows beyond the limits.
 */
class MediaBackpressureQueue(
    private val maxQueuedUs: Long = 100_000,
    private val maxBytes: Int = 2 * 1024 * 1024
) {
    private val lock = ReentrantLock()
    private val notEmpty = lock.newCondition()
    private val items = ArrayDeque<MediaPacket>()
    private var bytes = 0
    private var waitingForKey = false
    private var closed = false

    /** Number of frames dropped so far (diagnostics and tests). */
    var dropped = 0
        private set

    sealed class Offer {
        object Queued : Offer()
        /** Dropped; [requestSync] is true the first time after the queue backed up. */
        data class Dropped(val requestSync: Boolean) : Offer()
    }

    private fun spanUs(): Long {
        val frames = items.filter { !it.config }
        if (frames.size < 2) return 0
        return frames.last().ptsUs - frames.first().ptsUs
    }

    fun offer(p: MediaPacket): Offer = lock.withLock {
        if (closed) return Offer.Dropped(false)
        if (p.config) {
            push(p)
            return Offer.Queued
        }
        val backedUp = spanUs() > maxQueuedUs || bytes + p.data.size > maxBytes
        if (p.keyFrame) {
            if (backedUp || waitingForKey) {
                // Everything queued is stale: the key frame is a clean restart point.
                val keep = items.filter { it.config }
                dropped += items.size - keep.size
                items.clear(); items.addAll(keep)
                bytes = keep.sumOf { it.data.size }
            }
            waitingForKey = false
            push(p)
            return Offer.Queued
        }
        if (waitingForKey) {
            dropped++
            return Offer.Dropped(false)
        }
        if (backedUp) {
            waitingForKey = true
            dropped++
            return Offer.Dropped(true)
        }
        push(p)
        Offer.Queued
    }

    private fun push(p: MediaPacket) {
        items.addLast(p); bytes += p.data.size
        notEmpty.signal()
    }

    /** Blocks until a packet is available; null once [close]d and drained. */
    fun take(): MediaPacket? = lock.withLock {
        while (items.isEmpty()) {
            if (closed) return null
            notEmpty.await()
        }
        val p = items.removeFirst()
        bytes -= p.data.size
        p
    }

    fun close() = lock.withLock { closed = true; notEmpty.signalAll() }

    val size: Int get() = lock.withLock { items.size }

    /** (queued frames, ms span, dropped total) for diagnostics. */
    fun depth(): Triple<Int, Long, Int> = lock.withLock { Triple(items.count { !it.config }, spanUs() / 1000, dropped) }
}
