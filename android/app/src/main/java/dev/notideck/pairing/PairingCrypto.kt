// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.pairing

import java.security.MessageDigest
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/** Hex and hashing helpers plus the pairing proof (ADR 0004 §4). Mirrors nd-transport `pairing.rs`. */
object PairingCrypto {
    private val CONTEXT = "notideck-pair-v1".toByteArray(Charsets.US_ASCII)

    fun sha256(data: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(data)

    /** Lowercase hex SHA-256 of a DER certificate: the identity of a peer. */
    fun fingerprintHex(certDer: ByteArray): String = toHex(sha256(certDer))

    /** `HMAC-SHA256(secret, "notideck-pair-v1" || client_fp || host_fp)`; fps are the 32 raw bytes. */
    fun proof(secret: ByteArray, clientFp: ByteArray, hostFp: ByteArray): ByteArray {
        require(clientFp.size == 32 && hostFp.size == 32) { "fingerprints must be 32 bytes" }
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(SecretKeySpec(secret, "HmacSHA256"))
        mac.update(CONTEXT)
        mac.update(clientFp)
        mac.update(hostFp)
        return mac.doFinal()
    }

    fun proof(secret: ByteArray, clientFpHex: String, hostFpHex: String): ByteArray =
        proof(secret, fromHex(clientFpHex), fromHex(hostFpHex))

    fun toHex(bytes: ByteArray): String {
        val sb = StringBuilder(bytes.size * 2)
        for (b in bytes) {
            val v = b.toInt() and 0xff
            sb.append("0123456789abcdef"[v ushr 4]).append("0123456789abcdef"[v and 15])
        }
        return sb.toString()
    }

    fun fromHex(s: String): ByteArray {
        require(s.length % 2 == 0) { "odd hex length" }
        return ByteArray(s.length / 2) { i ->
            val hi = Character.digit(s[2 * i], 16)
            val lo = Character.digit(s[2 * i + 1], 16)
            require(hi >= 0 && lo >= 0) { "bad hex" }
            ((hi shl 4) or lo).toByte()
        }
    }

    fun isFingerprintHex(s: String): Boolean =
        s.length == 64 && s.all { it in '0'..'9' || it in 'a'..'f' }
}
