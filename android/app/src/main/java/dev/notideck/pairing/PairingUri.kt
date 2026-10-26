// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.pairing

import java.io.ByteArrayOutputStream
import java.util.Base64

/** Thrown by [PairingUri.parse]. [message] is short and developer-facing. */
class PairingUriException(message: String) : Exception(message)

/**
 * `notideck://pair?v=1&h=<host_id>&n=<host_name>&a=<ip,ip>&p=<port>&f=<fp_b64url>&s=<secret_b64url>&e=<unix_expiry>`
 * (ADR 0004 §4). Mirrors the host parser in nd-transport `pairing.rs`: any parameter order is
 * accepted, unknown parameters are ignored, [toUri] writes the canonical order.
 */
data class PairingUri(
    val hostId: String,
    val hostName: String,
    val addrs: List<String>,
    val port: Int,
    val hostFp: ByteArray,
    val secret: ByteArray,
    /** Unix seconds. */
    val expiresAt: Long
) {
    val hostFpHex: String get() = PairingCrypto.toHex(hostFp)

    fun isExpired(nowMs: Long = System.currentTimeMillis()): Boolean = nowMs / 1000 >= expiresAt

    fun toUri(): String =
        "$PREFIX" + "v=1&h=${pctEncode(hostId)}&n=${pctEncode(hostName)}" +
            "&a=${addrs.joinToString(",") { pctEncode(it) }}&p=$port" +
            "&f=${b64.encodeToString(hostFp)}&s=${b64.encodeToString(secret)}&e=$expiresAt"

    override fun equals(other: Any?): Boolean =
        other is PairingUri && toUri() == other.toUri()

    override fun hashCode(): Int = toUri().hashCode()

    override fun toString(): String = "PairingUri(host=$hostName, id=$hostId, addrs=$addrs, port=$port)" // never print the secret

    companion object {
        const val PREFIX = "notideck://pair?"
        private val b64 = Base64.getUrlEncoder().withoutPadding()
        private val b64d = Base64.getUrlDecoder()

        fun parseOrNull(uri: String): PairingUri? = try { parse(uri) } catch (_: PairingUriException) { null }

        fun parse(uri: String): PairingUri {
            val query = uri.trim().takeIf { it.startsWith(PREFIX) }?.removePrefix(PREFIX)
                ?: throw PairingUriException("not a notideck://pair link")
            val params = HashMap<String, String>()
            for (pair in query.split('&')) {
                if (pair.isEmpty()) continue
                val i = pair.indexOf('=')
                val k = if (i < 0) pair else pair.substring(0, i)
                val v = if (i < 0) "" else pair.substring(i + 1)
                params.putIfAbsent(k, v)
            }
            fun req(k: String) = params[k] ?: throw PairingUriException("missing parameter `$k`")
            fun bad(k: String): Nothing = throw PairingUriException("bad parameter `$k`")

            val v = req("v")
            if (v != "1") throw PairingUriException("unsupported pairing link version $v")
            val hostId = pctDecode(req("h")) ?: bad("h")
            val hostName = pctDecode(req("n")) ?: bad("n")
            val addrs = req("a").split(',').filter { it.isNotEmpty() }.map { raw ->
                val a = pctDecode(raw) ?: bad("a")
                if (!isIp(a)) bad("a")
                a
            }
            val port = req("p").takeIf { it.isNotEmpty() && it.length <= 5 && it.all(Char::isDigit) }
                ?.toInt()?.takeIf { it in 1..65535 } ?: bad("p")
            val fp = decodeB64(req("f"))?.takeIf { it.size == 32 } ?: bad("f")
            val secret = decodeB64(req("s"))?.takeIf { it.size == 16 } ?: bad("s")
            val exp = req("e").takeIf { it.isNotEmpty() && it.all(Char::isDigit) }?.toLongOrNull() ?: bad("e")
            if (hostId.isEmpty()) bad("h")
            return PairingUri(hostId, hostName, addrs, port, fp, secret, exp)
        }

        private fun decodeB64(s: String): ByteArray? =
            try { b64d.decode(s) } catch (_: IllegalArgumentException) { null }

        // Literal IPs only (never a hostname, so no DNS lookup happens on attacker input).
        internal fun isIp(s: String): Boolean = isIpv4(s) || isIpv6(s)

        private fun isIpv4(s: String): Boolean {
            val parts = s.split('.')
            return parts.size == 4 && parts.all { it.isNotEmpty() && it.length <= 3 && it.all(Char::isDigit) && it.toInt() <= 255 }
        }

        private fun isIpv6(s: String): Boolean {
            if (s.count { it == ':' } !in 2..7) return false
            if (s.any { Character.digit(it, 16) < 0 && it != ':' }) return false
            val groups = s.split(':')
            return groups.size <= 8 && groups.all { it.length <= 4 } && s.indexOf("::") == s.lastIndexOf("::")
        }

        fun pctEncode(s: String): String {
            val sb = StringBuilder()
            for (b in s.toByteArray(Charsets.UTF_8)) {
                val c = b.toInt() and 0xff
                if ((c in 'a'.code..'z'.code) || (c in 'A'.code..'Z'.code) || (c in '0'.code..'9'.code) ||
                    c == '-'.code || c == '.'.code || c == '_'.code || c == '~'.code
                ) sb.append(c.toChar()) else sb.append('%').append("0123456789ABCDEF"[c ushr 4]).append("0123456789ABCDEF"[c and 15])
            }
            return sb.toString()
        }

        fun pctDecode(s: String): String? {
            val out = ByteArrayOutputStream()
            var i = 0
            while (i < s.length) {
                val ch = s[i]
                when {
                    ch == '%' -> {
                        if (i + 3 > s.length) return null
                        val hi = Character.digit(s[i + 1], 16)
                        val lo = Character.digit(s[i + 2], 16)
                        if (hi < 0 || lo < 0) return null
                        out.write((hi shl 4) or lo)
                        i += 3
                    }
                    ch == '+' -> { out.write(' '.code); i++ }
                    else -> { out.write(ch.toString().toByteArray(Charsets.UTF_8)); i++ }
                }
            }
            return try {
                Charsets.UTF_8.newDecoder().decode(java.nio.ByteBuffer.wrap(out.toByteArray())).toString()
            } catch (_: java.nio.charset.CharacterCodingException) { null }
        }
    }
}
