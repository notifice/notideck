// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.PlayStream

/**
 * One request to the built-in player, mapped from a host [PlayStream]. [id] changes on every request so
 * a repeat of the same URL (e.g. after the user closed the player) still restarts playback.
 */
data class PlayerRequest(
    val id: Long,
    val url: String,
    val title: String,
    val subtitle: String,
    /** Cap on the video height in px; 0 = adaptive, no cap. */
    val maxHeight: Int,
    /** Video only: volume 0, no audio focus (sound plays on the PC). */
    val muted: Boolean = false,
    /** > 0: keep playback this far behind the broadcast clock (PDT) using the host clock offset. */
    val syncDelayMs: Int = 0,
    /** Echoed in PlayerClosed when the user closes the player. */
    val sessionId: String = ""
) {
    companion object {
        /** Null for an empty/blank url (= close the player). Only http(s) URLs are accepted. */
        fun from(msg: PlayStream, id: Long): PlayerRequest? {
            val url = msg.url.trim()
            if (url.isEmpty()) return null
            val lower = url.lowercase()
            if (!lower.startsWith("https://") && !lower.startsWith("http://")) return null
            return PlayerRequest(id, url, msg.title, msg.subtitle, msg.max_height.coerceAtLeast(0),
                msg.muted, msg.sync_delay_ms.coerceAtLeast(0), msg.session_id)
        }
    }
}
