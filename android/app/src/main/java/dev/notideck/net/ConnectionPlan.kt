// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.pairing.PairedHost

/** One place to try connecting to. [pinFp] == null means "accept any cert" (USB first contact). */
data class Endpoint(
    val host: String,
    val port: Int,
    val transport: Transport,
    val pinFp: String?,
    val hostId: String? = null
)

/** A discovered PC with whether we already trust it. */
data class DiscoveredHost(val service: DiscoveredService, val pairedHostId: String?) {
    val isPaired: Boolean get() = pairedHostId != null
}

object ConnectionPlan {
    const val USB_HOST = "127.0.0.1"
    const val USB_PORT = 47800

    /** The paired host an mDNS service belongs to: TXT id first, then the fp8 prefix. */
    fun match(service: DiscoveredService, hosts: List<PairedHost>): PairedHost? =
        hosts.firstOrNull { service.hostId.isNotEmpty() && it.hostId == service.hostId }
            ?: hosts.firstOrNull { service.fp8.length == 8 && it.fp.startsWith(service.fp8, ignoreCase = true) }

    fun usb() = Endpoint(USB_HOST, USB_PORT, Transport.USB, null)

    /**
     * ADR 0004 §8 order: USB, then each paired PC's stored addresses (most recently used PC first),
     * then mDNS-discovered addresses of paired PCs. Duplicates are dropped.
     */
    fun plan(hosts: List<PairedHost>, discovered: List<DiscoveredService>): List<Endpoint> {
        val out = LinkedHashSet<Endpoint>()
        out += usb()
        val sorted = hosts.sortedByDescending { it.lastConnected }
        for (h in sorted) for (a in h.addrs) out += Endpoint(a, h.port, Transport.LAN, h.fp, h.hostId)
        for (s in discovered) {
            val h = match(s, sorted) ?: continue
            for (a in s.addrs) out += Endpoint(a, s.port, Transport.LAN, h.fp, h.hostId)
        }
        return out.toList()
    }

    fun classify(discovered: List<DiscoveredService>, hosts: List<PairedHost>): List<DiscoveredHost> =
        discovered.map { DiscoveredHost(it, match(it, hosts)?.hostId) }
}
