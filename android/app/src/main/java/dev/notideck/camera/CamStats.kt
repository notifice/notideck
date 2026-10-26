// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.camera

import java.util.concurrent.atomic.AtomicLong

/** Streaming counters for the `NdCamStats` log line. No allocation on the hot path. */
class CamStats {
    val encoded = AtomicLong()
    val keyframes = AtomicLong()
    val bytesSent = AtomicLong()
    private val latSumUs = AtomicLong()
    private val latCount = AtomicLong()
    private val latMaxUs = AtomicLong()   // single writer (drain thread)

    fun encodeLatency(us: Long) {
        latSumUs.addAndGet(us); latCount.incrementAndGet()
        if (us > latMaxUs.get()) latMaxUs.set(us)
    }

    class Snapshot(val encoded: Long, val keys: Long, val bytes: Long, val avgLatMs: Float, val maxLatMs: Float)

    fun drain(): Snapshot {
        val n = latCount.getAndSet(0)
        val sum = latSumUs.getAndSet(0)
        return Snapshot(
            encoded.getAndSet(0), keyframes.getAndSet(0), bytesSent.getAndSet(0),
            if (n > 0) sum / n / 1000f else 0f, latMaxUs.getAndSet(0) / 1000f
        )
    }
}
