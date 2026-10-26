// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

/**
 * Estimates `hostWallMs - phoneWallMs` from Ping/Pong round trips (SPEC round 21 sync algorithm).
 * The phone records its wall time when it sends each Ping (keyed by nonce) and again on the Pong;
 * sample = host_time - (s + r) / 2, RTT = r - s. The sample with the smallest RTT among the last
 * [WINDOW] is used. Offset is 0 until a sample exists. Not thread-safe by itself; callers synchronize.
 */
class HostClockEstimator {
    companion object {
        const val WINDOW = 16
        private const val MAX_PENDING = 64
    }

    private class Sample(val offsetMs: Long, val rttMs: Long)

    private val pending = LinkedHashMap<Long, Long>()
    private val samples = ArrayDeque<Sample>()

    @get:Synchronized
    var offsetMs: Long = 0L
        private set

    @Synchronized
    fun onPingSent(nonce: Long, phoneWallMs: Long) {
        pending[nonce] = phoneWallMs
        while (pending.size > MAX_PENDING) pending.remove(pending.keys.first())
    }

    @Synchronized
    fun onPong(nonce: Long, hostTimeMs: Long, phoneWallMs: Long) {
        val s = pending.remove(nonce) ?: return
        if (hostTimeMs == 0L) return
        val rtt = (phoneWallMs - s).coerceAtLeast(0L)
        samples.addLast(Sample(hostTimeMs - (s + phoneWallMs) / 2, rtt))
        while (samples.size > WINDOW) samples.removeFirst()
        offsetMs = samples.minByOrNull { it.rttMs }!!.offsetMs
    }

    @Synchronized
    fun reset() {
        pending.clear(); samples.clear(); offsetMs = 0L
    }
}
