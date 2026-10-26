// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.pairing

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

/** A PC this phone has paired with (or trusted on first use over USB). */
data class PairedHost(
    val hostId: String,
    val name: String,
    /** Lowercase hex SHA-256 of the PC's TLS certificate (the pin). */
    val fp: String,
    val addrs: List<String> = emptyList(),
    val port: Int = 47800,
    /** Epoch millis of the last completed session, 0 = never. */
    val lastConnected: Long = 0L
)

/** Persistent list of paired PCs. */
interface HostStore {
    fun all(): List<PairedHost>
    fun get(hostId: String): PairedHost? = all().firstOrNull { it.hostId == hostId }
    fun byFingerprint(fp: String): PairedHost? = all().firstOrNull { it.fp.equals(fp, ignoreCase = true) }
    fun put(host: PairedHost)
    fun remove(hostId: String)
}

class MemoryHostStore : HostStore {
    private val map = LinkedHashMap<String, PairedHost>()
    @Synchronized override fun all() = map.values.toList()
    @Synchronized override fun put(host: PairedHost) { map[host.hostId] = host }
    @Synchronized override fun remove(hostId: String) { map.remove(hostId) }
}

/** JSON in SharedPreferences. Small (a handful of PCs), so the whole list is rewritten on change. */
class PrefsHostStore(context: Context) : HostStore {
    private val prefs = context.getSharedPreferences("notideck_hosts", Context.MODE_PRIVATE)

    @Synchronized
    override fun all(): List<PairedHost> {
        val raw = prefs.getString(KEY, null) ?: return emptyList()
        return try {
            val arr = JSONArray(raw)
            (0 until arr.length()).map { i ->
                val o = arr.getJSONObject(i)
                PairedHost(
                    hostId = o.getString("host_id"),
                    name = o.optString("name"),
                    fp = o.getString("fp"),
                    addrs = o.optJSONArray("addrs")?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList(),
                    port = o.optInt("port", 47800),
                    lastConnected = o.optLong("last_connected", 0L)
                )
            }
        } catch (_: Exception) {
            emptyList()
        }
    }

    @Synchronized
    override fun put(host: PairedHost) {
        val list = all().filter { it.hostId != host.hostId } + host
        save(list)
    }

    @Synchronized
    override fun remove(hostId: String) = save(all().filter { it.hostId != hostId })

    private fun save(list: List<PairedHost>) {
        val arr = JSONArray()
        for (h in list) {
            arr.put(
                JSONObject()
                    .put("host_id", h.hostId).put("name", h.name).put("fp", h.fp)
                    .put("addrs", JSONArray(h.addrs)).put("port", h.port)
                    .put("last_connected", h.lastConnected)
            )
        }
        prefs.edit().putString(KEY, arr.toString()).apply()
    }

    private companion object { const val KEY = "paired_hosts" }
}
