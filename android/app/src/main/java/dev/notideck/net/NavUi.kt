// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.net

import dev.notideck.proto.v1.NavAction
import dev.notideck.proto.v1.NavRequest
import dev.notideck.proto.v1.NavState

/** page_id used for rail keys in InputEvent and UpdateControls. */
const val RAIL_PAGE_ID = "__rail"

/** Side-rail state. The default (all false/zero) shows the Home button only. */
data class NavUi(
    val canGoBack: Boolean = false,
    val paused: Boolean = false,
    val onHome: Boolean = false,
    val profileName: String = "",
    val pageIndex: Int = 0,
    /** Top-level pages; 0 while a folder is open. Dots show only when > 1. */
    val pageCount: Int = 0
) {
    companion object {
        fun from(s: NavState) = NavUi(
            s.can_go_back, s.auto_switch_paused, s.on_home, s.profile_name,
            s.page_index, s.page_count
        )
    }
}

/** Request sent when page dot [index] is tapped. */
fun goToPageRequest(index: Int) =
    NavRequest(action = NavAction.NAV_ACTION_GO_TO_PAGE, page_index = index)
