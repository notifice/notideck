// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.pairing

import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Base64

/** Runs against docs/pairing-vectors.json (copied into test resources by Gradle). */
class PairingVectorsTest {
    private val vectors: JSONObject by lazy {
        val stream = javaClass.classLoader!!.getResourceAsStream("pairing-vectors.json")
        assertNotNull("pairing-vectors.json missing from test resources", stream)
        JSONObject(stream!!.readBytes().toString(Charsets.UTF_8))
    }

    private fun b64(s: String) = Base64.getUrlDecoder().decode(s)

    @Test
    fun proofVectors() {
        val arr = vectors.getJSONArray("proofs")
        assertTrue(arr.length() >= 3)
        for (i in 0 until arr.length()) {
            val v = arr.getJSONObject(i)
            val clientFp = PairingCrypto.sha256(PairingCrypto.fromHex(v.getString("client_cert_der_hex")))
            val hostFp = PairingCrypto.sha256(PairingCrypto.fromHex(v.getString("host_cert_der_hex")))
            assertEquals(v.getString("client_fp_hex"), PairingCrypto.toHex(clientFp))
            assertEquals(v.getString("host_fp_hex"), PairingCrypto.toHex(hostFp))
            val proof = PairingCrypto.proof(b64(v.getString("secret_b64url")), clientFp, hostFp)
            assertEquals(v.getString("proof_hex"), PairingCrypto.toHex(proof))
            // order matters
            val swapped = PairingCrypto.proof(b64(v.getString("secret_b64url")), hostFp, clientFp)
            assertFalse(proof.contentEquals(swapped))
        }
    }

    @Test
    fun uriVectorsParseAndRoundTrip() {
        val arr = vectors.getJSONArray("uris")
        for (i in 0 until arr.length()) {
            val v = arr.getJSONObject(i)
            val uri = PairingUri.parse(v.getString("uri"))
            assertEquals(v.getString("host_id"), uri.hostId)
            assertEquals(v.getString("host_name"), uri.hostName)
            val addrs = v.getJSONArray("addrs")
            assertEquals((0 until addrs.length()).map { addrs.getString(it) }, uri.addrs)
            assertEquals(v.getInt("port"), uri.port)
            assertEquals(v.getString("host_fp_hex"), uri.hostFpHex)
            assertArrayEquals(b64(v.getString("secret_b64url")), uri.secret)
            assertEquals(v.getLong("expires_at"), uri.expiresAt)
            assertEquals(v.getString("uri"), uri.toUri())
            assertEquals(uri, PairingUri.parse(uri.toUri()))
        }
    }

    @Test
    fun invalidUrisAreRejected() {
        val arr = vectors.getJSONArray("invalid_uris")
        assertTrue(arr.length() >= 7)
        for (i in 0 until arr.length()) {
            val s = arr.getString(i)
            assertNull("should be rejected: $s", PairingUri.parseOrNull(s))
        }
    }

    @Test
    fun expiry() {
        val uri = PairingUri.parse(vectors.getJSONArray("uris").getJSONObject(0).getString("uri"))
        assertTrue(uri.isExpired(uri.expiresAt * 1000))
        assertTrue(uri.isExpired(uri.expiresAt * 1000 + 1))
        assertFalse(uri.isExpired(uri.expiresAt * 1000 - 1))
    }

    @Test
    fun paramOrderAndUnknownParamsAccepted() {
        val u = "notideck://pair?e=1791820800&x=1&s=AAECAwQFBgcICQoLDA0ODw&f=LJp3rex828XR1Vq5zK9MfA0jilnpX2WRomTz0OYnmYA&p=47800&a=192.168.1.20&n=PC&h=abc&v=1"
        val uri = PairingUri.parse(u)
        assertEquals("abc", uri.hostId)
    }
}
