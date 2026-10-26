// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import dev.notideck.pairing.PairingCrypto
import java.math.BigInteger
import java.net.Socket
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.Principal
import java.security.PrivateKey
import java.security.cert.X509Certificate
import java.security.spec.ECGenParameterSpec
import java.util.Date
import javax.net.ssl.X509KeyManager
import javax.security.auth.x500.X500Principal

/**
 * The phone's TLS identity (ADR 0004 §2): an ECDSA P-256 key generated inside the AndroidKeyStore
 * (non-exportable) with the self-signed certificate the KeyStore creates alongside it.
 *
 * Digests: SHA-256, and NONE because Conscrypt signs TLS handshakes through `NONEwithECDSA`
 * on a pre-hashed digest when the key is not extractable.
 */
class ClientIdentity(private val alias: String = DEFAULT_ALIAS) {

    private val keyStore: KeyStore get() = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

    @Synchronized
    fun ensure(): X509Certificate {
        val ks = keyStore
        (ks.getCertificate(alias) as? X509Certificate)?.let { return it }
        val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
            .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
            .setDigests(KeyProperties.DIGEST_SHA256, KeyProperties.DIGEST_NONE)
            .setCertificateSubject(X500Principal("CN=notideck-phone"))
            .setCertificateSerialNumber(BigInteger.valueOf(System.currentTimeMillis()))
            .setCertificateNotBefore(Date(System.currentTimeMillis() - 24L * 3600 * 1000))
            .setCertificateNotAfter(Date(System.currentTimeMillis() + 30L * 365 * 24 * 3600 * 1000))
            .build()
        KeyPairGenerator.getInstance("EC", "AndroidKeyStore").apply { initialize(spec) }.generateKeyPair()
        return ks.getCertificate(alias) as X509Certificate
    }

    /** Lowercase hex SHA-256 of the certificate DER. */
    fun fingerprintHex(): String = PairingCrypto.fingerprintHex(ensure().encoded)

    fun privateKey(): PrivateKey = (keyStore.getKey(alias, null) as PrivateKey).also { ensure() }

    /** Presents this identity as the TLS client (or server, in tests) certificate. */
    fun keyManager(): X509KeyManager {
        val cert = ensure()
        val key = privateKey()
        val a = alias
        return object : X509KeyManager {
            override fun chooseClientAlias(keyType: Array<out String>?, issuers: Array<out Principal>?, socket: Socket?) = a
            override fun chooseServerAlias(keyType: String?, issuers: Array<out Principal>?, socket: Socket?) = a
            override fun getClientAliases(keyType: String?, issuers: Array<out Principal>?) = arrayOf(a)
            override fun getServerAliases(keyType: String?, issuers: Array<out Principal>?) = arrayOf(a)
            override fun getCertificateChain(alias: String?) = arrayOf(cert)
            override fun getPrivateKey(alias: String?) = key
        }
    }

    companion object {
        const val DEFAULT_ALIAS = "notideck-client"
    }
}
