// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Receives icon bytes pushed by the host and answers cache lookups. */
interface IconSink {
    fun contains(hash: String): Boolean

    /** Returns true if the icon was decoded and stored. */
    fun put(hash: String, mimeType: String, data: ByteArray): Boolean
}

/** Decodes encoded image bytes (PNG/WebP) into a platform image. Returns null on failure. */
fun interface IconDecoder<T : Any> {
    fun decode(data: ByteArray): T?
}

/**
 * Content-addressed LRU icon cache bounded by an approximate byte budget.
 *
 * Pure JVM: decoding is delegated to [decoder] and sizing to [sizeOf], so it can be unit tested
 * without Android. [icons] publishes an immutable snapshot after every change.
 */
class IconCache<T : Any>(
    private val maxBytes: Long = DEFAULT_MAX_BYTES,
    private val decoder: IconDecoder<T>,
    private val sizeOf: (T) -> Int
) : IconSink {

    companion object {
        const val DEFAULT_MAX_BYTES = 16L * 1024 * 1024
    }

    private val lock = Any()
    private val entries = LinkedHashMap<String, T>(16, 0.75f, true)
    private var totalBytes = 0L

    private val _icons = MutableStateFlow<Map<String, T>>(emptyMap())
    val icons: StateFlow<Map<String, T>> = _icons.asStateFlow()

    override fun contains(hash: String): Boolean = synchronized(lock) { entries.containsKey(hash) }

    override fun put(hash: String, mimeType: String, data: ByteArray): Boolean {
        if (hash.isEmpty() || data.isEmpty()) return false
        val decoded = try {
            decoder.decode(data)
        } catch (_: Exception) {
            null
        } ?: return false

        synchronized(lock) {
            entries.remove(hash)?.let { totalBytes -= sizeOf(it) }
            entries[hash] = decoded
            totalBytes += sizeOf(decoded)
            // Evict least recently used, but always keep the newest entry.
            val it = entries.entries.iterator()
            while (totalBytes > maxBytes && entries.size > 1 && it.hasNext()) {
                val eldest = it.next()
                if (eldest.key == hash) continue
                totalBytes -= sizeOf(eldest.value)
                it.remove()
            }
            _icons.value = LinkedHashMap(entries)
        }
        return true
    }

    fun clear() {
        synchronized(lock) {
            entries.clear()
            totalBytes = 0
            _icons.value = emptyMap()
        }
    }

    fun sizeBytes(): Long = synchronized(lock) { totalBytes }
}
