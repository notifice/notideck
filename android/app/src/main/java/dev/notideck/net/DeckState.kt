// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.Control

/**
 * State of the currently active deck page.
 */
data class DeckState(
    val pageId: String = "",
    val cols: Int = 5,
    val rows: Int = 3,
    val controls: Map<String, Control> = emptyMap()
)
