// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.mic

import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/** Pure helpers for microphone capture; no Android types so they run in JVM tests. */
object MicAudio {
    const val DEFAULT_RATE = 48_000
    const val DEFAULT_FRAME_MS = 20
    const val MIN_FRAME_MS = 10
    const val MAX_FRAME_MS = 40
    const val MAX_QUEUED_US = 200_000L
    private val FALLBACK_RATES = listOf(48_000, 44_100, 16_000)

    /** Requested frame length clamped to 10..40 ms; 0 (unset) means the 20 ms default. */
    fun frameMs(requested: Int): Int =
        if (requested <= 0) DEFAULT_FRAME_MS else requested.coerceIn(MIN_FRAME_MS, MAX_FRAME_MS)

    /** Samples (mono) in one frame of [frameMs] at [rate]. */
    fun frameSamples(rate: Int, frameMs: Int): Int = rate * frameMs / 1000

    fun frameBytes(rate: Int, frameMs: Int): Int = frameSamples(rate, frameMs) * 2

    /** Duration of [samples] mono samples at [rate], in microseconds. */
    fun samplesToUs(samples: Long, rate: Int): Long = samples * 1_000_000L / rate

    /** Rates to try in order: the requested one (0 = 48000), then 48000, 44100 and 16000. */
    fun rateCandidates(requested: Int): List<Int> =
        (listOf(if (requested > 0) requested else DEFAULT_RATE) + FALLBACK_RATES).distinct()

    /** s16 samples to little-endian bytes (the wire payload). */
    fun packPcm16(samples: ShortArray, count: Int = samples.size): ByteArray {
        val out = ByteArray(count * 2)
        for (i in 0 until count) {
            val v = samples[i].toInt()
            out[2 * i] = v.toByte()
            out[2 * i + 1] = (v shr 8).toByte()
        }
        return out
    }
}

/**
 * Presentation times for consecutive frames. Counting samples gives an even spacing; the clock is
 * only used to anchor the first frame and to re-anchor after a stall (drift beyond [resyncUs]).
 */
class PtsTracker(private val rate: Int, private val resyncUs: Long = 100_000) {
    private var anchorUs = 0L
    private var samples = 0L
    private var started = false

    /** [endUs] is the clock time at which a frame of [frameSamples] samples finished being captured. */
    fun next(endUs: Long, frameSamples: Int): Long {
        val startUs = endUs - MicAudio.samplesToUs(frameSamples.toLong(), rate)
        if (!started) { started = true; anchorUs = startUs; samples = 0 }
        var pts = anchorUs + MicAudio.samplesToUs(samples, rate)
        if (kotlin.math.abs(startUs - pts) > resyncUs) {
            anchorUs = startUs; samples = 0; pts = startUs
        }
        samples += frameSamples
        return pts
    }
}

/** One captured frame ready for the wire. [durationUs] is what its samples span. */
class AudioFrame(val ptsUs: Long, val data: ByteArray, val durationUs: Long)

/**
 * Outgoing queue between the capture thread and the socket writer. When more than [maxQueuedUs] of
 * audio is waiting the oldest frames are dropped, so latency stays bounded when the link stalls.
 */
class AudioDropQueue(private val maxQueuedUs: Long = MicAudio.MAX_QUEUED_US) {
    private val lock = ReentrantLock()
    private val notEmpty = lock.newCondition()
    private val items = ArrayDeque<AudioFrame>()
    private var queuedUs = 0L
    private var closed = false

    /** Frames dropped so far. */
    var dropped = 0
        private set

    fun offer(f: AudioFrame) = lock.withLock {
        if (closed) return@withLock
        items.addLast(f); queuedUs += f.durationUs
        while (queuedUs > maxQueuedUs && items.size > 1) {
            queuedUs -= items.removeFirst().durationUs
            dropped++
        }
        notEmpty.signal()
    }

    /** Blocks until a frame is available; null once [close]d and drained. */
    fun take(): AudioFrame? = lock.withLock {
        while (items.isEmpty()) {
            if (closed) return null
            notEmpty.await()
        }
        val f = items.removeFirst()
        queuedUs -= f.durationUs
        f
    }

    fun close() = lock.withLock { closed = true; notEmpty.signalAll() }

    val size: Int get() = lock.withLock { items.size }
}
