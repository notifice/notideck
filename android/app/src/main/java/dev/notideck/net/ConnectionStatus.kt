// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

/** How the current session reaches the PC. */
enum class Transport { USB, LAN }

/**
 * Connection lifecycle states for the control session (also drives the connection overlay).
 */
sealed class ConnectionStatus {
    object Disconnected : ConnectionStatus()

    /** Looking for a PC: trying USB, the paired PCs' addresses and mDNS. */
    object Connecting : ConnectionStatus()

    data class Connected(
        val sessionId: String,
        val hostName: String,
        val protoVersion: Int = 2,
        val transport: Transport = Transport.USB
    ) : ConnectionStatus()

    /** Nothing reachable; waiting [nextRetryMs] before the next round. */
    data class Reconnecting(
        val attempt: Int,
        val nextRetryMs: Long,
        val lastError: String? = null
    ) : ConnectionStatus()

    /** PairingPending: the PC shows "Allow / Deny" for this phone. */
    data class WaitingApproval(val hostName: String) : ConnectionStatus()

    /** QR pairing in progress (proof sent, waiting for PairResult). */
    data class Pairing(val hostName: String) : ConnectionStatus()

    /** PairResult ok; the session starts right after. */
    data class Paired(val hostName: String) : ConnectionStatus()

    /** A message the user should read (denied, removed, expired code, version mismatch, ...). */
    data class Notice(val message: String, val isError: Boolean = true) : ConnectionStatus()
}
