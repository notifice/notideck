// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.net.Inet4Address

/** A `_notideck._tcp` service resolved through mDNS. */
data class DiscoveredService(
    val serviceName: String,
    val hostId: String,
    val name: String,
    /** First 8 hex chars of the host cert fingerprint (TXT `fp8`). */
    val fp8: String,
    val addrs: List<String>,
    val port: Int
)

/** Finds PCs on the LAN. Abstracted so the manager can be tested without Android. */
interface HostDiscovery {
    val services: StateFlow<List<DiscoveredService>>
    fun start()
    fun stop()
}

class NoDiscovery : HostDiscovery {
    override val services: StateFlow<List<DiscoveredService>> = MutableStateFlow(emptyList())
    override fun start() {}
    override fun stop() {}
}

/** NsdManager based discovery (no Play Services). Resolves one service at a time (API 28 limit). */
class NsdDiscovery(context: Context) : HostDiscovery {
    private val app = context.applicationContext
    private val nsd = app.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val main = Handler(Looper.getMainLooper())
    private val _services = MutableStateFlow<List<DiscoveredService>>(emptyList())
    override val services: StateFlow<List<DiscoveredService>> = _services.asStateFlow()

    private var multicast: WifiManager.MulticastLock? = null
    private var listener: NsdManager.DiscoveryListener? = null
    private val queue = ArrayDeque<NsdServiceInfo>()
    private var resolving = false

    @Synchronized
    override fun start() {
        if (listener != null) return
        try {
            val wifi = app.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
            multicast = wifi?.createMulticastLock("notideck-nsd")?.apply { setReferenceCounted(false); acquire() }
        } catch (e: Exception) {
            Log.w(TAG, "multicast lock: $e")
        }
        val l = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(serviceType: String) {}
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.w(TAG, "discovery start failed: $errorCode")
                synchronized(this@NsdDiscovery) { listener = null }
            }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {}
            override fun onDiscoveryStopped(serviceType: String) {}
            override fun onServiceFound(info: NsdServiceInfo) = enqueue(info)
            override fun onServiceLost(info: NsdServiceInfo) {
                _services.value = _services.value.filter { it.serviceName != info.serviceName }
            }
        }
        listener = l
        try {
            nsd.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, l)
        } catch (e: Exception) {
            Log.w(TAG, "discoverServices: $e")
            listener = null
        }
    }

    @Synchronized
    override fun stop() {
        listener?.let { try { nsd.stopServiceDiscovery(it) } catch (_: Exception) {} }
        listener = null
        queue.clear()
        resolving = false
        multicast?.let { try { if (it.isHeld) it.release() } catch (_: Exception) {} }
        multicast = null
        _services.value = emptyList()
    }

    @Synchronized
    private fun enqueue(info: NsdServiceInfo) {
        queue.addLast(info)
        pump()
    }

    @Synchronized
    private fun pump() {
        if (resolving) return
        val next = queue.removeFirstOrNull() ?: return
        resolving = true
        @Suppress("DEPRECATION")
        nsd.resolveService(next, object : NsdManager.ResolveListener {
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                Log.w(TAG, "resolve failed: $errorCode")
                done()
            }
            override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
                toService(serviceInfo)?.let { s ->
                    _services.value = _services.value.filter { it.serviceName != s.serviceName } + s
                }
                done()
            }
        })
    }

    private fun done() {
        synchronized(this) { resolving = false }
        main.post { synchronized(this) { pump() } }
    }

    private fun toService(info: NsdServiceInfo): DiscoveredService? {
        val txt = info.attributes.mapValues { (_, v) -> v?.toString(Charsets.UTF_8) ?: "" }
        val host = info.host ?: return null
        val addr = host.hostAddress ?: return null
        // IPv6 link-local needs a scope id we don't have; only IPv4 and global IPv6.
        if (host.isLinkLocalAddress && host !is Inet4Address) return null
        return DiscoveredService(
            serviceName = info.serviceName,
            hostId = txt["id"] ?: "",
            name = txt["name"] ?: info.serviceName,
            fp8 = (txt["fp8"] ?: "").lowercase(),
            addrs = listOf(addr.substringBefore('%')),
            port = info.port
        )
    }

    private companion object {
        const val TAG = "NsdDiscovery"
        const val SERVICE_TYPE = "_notideck._tcp."
    }
}
