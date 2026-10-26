// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.pairing.MemoryHostStore
import dev.notideck.pairing.PairedHost
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ConnectionPlanTest {
    private val fpA = "aa".repeat(32)
    private val fpB = "bb".repeat(32)
    private val a = PairedHost("id-a", "A", fpA, listOf("192.168.1.2", "10.0.0.2"), 47800, lastConnected = 100)
    private val b = PairedHost("id-b", "B", fpB, listOf("192.168.1.3"), 47801, lastConnected = 200)

    @Test
    fun usbFirstThenMostRecentHostThenDiscovery() {
        val svc = DiscoveredService("svc", "id-a", "A", "aaaaaaaa", listOf("192.168.1.99"), 47800)
        val plan = ConnectionPlan.plan(listOf(a, b), listOf(svc))
        assertEquals(Transport.USB, plan[0].transport)
        assertEquals(null, plan[0].pinFp)
        assertEquals(listOf("192.168.1.3", "192.168.1.2", "10.0.0.2", "192.168.1.99"), plan.drop(1).map { it.host })
        assertEquals(fpB, plan[1].pinFp)
        assertEquals(fpA, plan.last().pinFp)
    }

    @Test
    fun unpairedServicesAreNeverTried() {
        val svc = DiscoveredService("svc", "stranger", "X", "cccccccc", listOf("192.168.1.50"), 47800)
        val plan = ConnectionPlan.plan(listOf(a), listOf(svc))
        assertTrue(plan.none { it.host == "192.168.1.50" })
        assertNull(ConnectionPlan.classify(listOf(svc), listOf(a)).single().pairedHostId)
    }

    @Test
    fun matchesByFp8WhenIdMissingAndDedupes() {
        val svc = DiscoveredService("svc", "", "A", "aaaaaaaa", listOf("192.168.1.2"), 47800)
        assertEquals("id-a", ConnectionPlan.match(svc, listOf(a, b))?.hostId)
        val plan = ConnectionPlan.plan(listOf(a), listOf(svc))
        assertEquals(1, plan.count { it.host == "192.168.1.2" })
    }

    @Test
    fun hostStoreByFingerprint() {
        val s = MemoryHostStore()
        s.put(a); s.put(b)
        assertEquals("id-b", s.byFingerprint(fpB.uppercase())?.hostId)
        s.remove("id-b")
        assertEquals(listOf("id-a"), s.all().map { it.hostId })
    }
}
