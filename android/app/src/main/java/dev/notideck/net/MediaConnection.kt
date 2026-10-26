// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.Envelope
import dev.notideck.proto.v1.MediaAttach
import dev.notideck.proto.v1.MediaKind
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.IOException
import java.net.Socket

/**
 * The attach handshake shared by every media connection (ADR 0005): open the second TLS connection,
 * send `MediaAttach{session_id, kind}`, wait for `MediaAttached`. Afterwards the socket carries only
 * phone -> host media packets, written through [out].
 */
class MediaConnection private constructor(val socket: Socket, val out: DataOutputStream) {
    fun close() { try { socket.close() } catch (_: Exception) {} }

    companion object {
        /**
         * [open] must return a connected, handshaken (TLS) socket. [onSocket] is called with it before the
         * handshake starts so the caller can close it from another thread to abort a stuck attach.
         * Throws [IOException] on a refused or malformed attach (the socket is closed then).
         */
        fun attach(
            open: () -> Socket,
            sessionId: String,
            kind: MediaKind,
            onSocket: (Socket) -> Unit = {}
        ): MediaConnection {
            val s = open()
            onSocket(s)
            s.tcpNoDelay = true
            s.soTimeout = 5000
            val out = DataOutputStream(s.getOutputStream().buffered())
            val inp = DataInputStream(s.getInputStream().buffered())
            writeFrame(out, Envelope(seq = 1, media_attach = MediaAttach(session_id = sessionId, kind = kind)))
            val len = inp.readInt()
            if (len <= 0 || len > 64 * 1024) throw IOException("bad MediaAttached frame")
            val buf = ByteArray(len); inp.readFully(buf)
            val reply = Envelope.ADAPTER.decode(buf).media_attached ?: throw IOException("expected MediaAttached")
            if (!reply.ok) throw IOException("host refused media: ${reply.error}")
            s.soTimeout = 0
            return MediaConnection(s, out)
        }

        private fun writeFrame(out: DataOutputStream, e: Envelope) {
            val b = Envelope.ADAPTER.encode(e)
            out.writeInt(b.size); out.write(b); out.flush()
        }
    }
}
