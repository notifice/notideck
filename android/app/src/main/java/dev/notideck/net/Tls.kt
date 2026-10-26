// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.pairing.PairingCrypto
import org.conscrypt.Conscrypt
import java.net.InetSocketAddress
import java.security.Security
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.KeyManager
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import javax.net.ssl.X509KeyManager
import javax.net.ssl.X509TrustManager

/** An established TLS 1.3 link plus the server certificate fingerprint it presented. */
class TlsLink(val socket: SSLSocket, val serverFp: String)

/**
 * Trust manager that accepts exactly one pinned certificate fingerprint, or, with [pin] == null,
 * any certificate (USB first contact, TOFU). The seen fingerprint is always recorded.
 */
class PinTrustManager(private val pin: String?) : X509TrustManager {
    @Volatile var seenFp: String? = null
        private set

    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
        val leaf = chain?.firstOrNull() ?: throw CertificateException("no server certificate")
        val fp = PairingCrypto.fingerprintHex(leaf.encoded)
        seenFp = fp
        if (pin != null && !pin.equals(fp, ignoreCase = true)) {
            throw CertificateException("server certificate does not match the pinned fingerprint")
        }
    }

    /** Server-side use in tests: accept any client cert and remember it. */
    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
        val leaf = chain?.firstOrNull() ?: throw CertificateException("no client certificate")
        seenFp = PairingCrypto.fingerprintHex(leaf.encoded)
    }

    override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
}

object Tls {
    private val installLock = Any()
    @Volatile private var installed = false

    /** Installs Conscrypt as the highest-priority provider (Android 9 has no platform TLS 1.3). */
    fun installConscrypt() {
        if (installed) return
        synchronized(installLock) {
            if (installed) return
            Security.insertProviderAt(Conscrypt.newProvider(), 1)
            installed = true
        }
    }

    fun context(keyManager: X509KeyManager?, trust: X509TrustManager): SSLContext {
        installConscrypt()
        val ctx = SSLContext.getInstance("TLSv1.3", Security.getProvider("Conscrypt"))
        ctx.init(keyManager?.let { arrayOf<KeyManager>(it) }, arrayOf(trust), null)
        return ctx
    }

    /** Connects, handshakes TLS 1.3 only, and returns the link. Closes the socket on failure. */
    fun connect(
        host: String,
        port: Int,
        pin: String?,
        identity: ClientIdentity,
        connectTimeoutMs: Int,
        handshakeTimeoutMs: Int = 5000
    ): TlsLink {
        val trust = PinTrustManager(pin)
        val ctx = context(identity.keyManager(), trust)
        val socket = ctx.socketFactory.createSocket() as SSLSocket
        try {
            socket.tcpNoDelay = true
            socket.enabledProtocols = arrayOf("TLSv1.3")
            socket.connect(InetSocketAddress(host, port), connectTimeoutMs)
            socket.soTimeout = handshakeTimeoutMs
            socket.startHandshake()
            val fp = trust.seenFp ?: throw CertificateException("handshake finished without a server certificate")
            return TlsLink(socket, fp)
        } catch (e: Throwable) {
            try { socket.close() } catch (_: Exception) {}
            throw e
        }
    }
}
