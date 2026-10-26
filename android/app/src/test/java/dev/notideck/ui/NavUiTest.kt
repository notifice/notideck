// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import dev.notideck.net.NavUi
import dev.notideck.net.goToPageRequest
import dev.notideck.proto.v1.NavAction
import dev.notideck.proto.v1.NavState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

class NavUiTest {
    @Test fun defaultIsAllFalse() {
        val d = NavUi()
        assertFalse(d.canGoBack || d.paused || d.onHome)
        assertEquals("", d.profileName)
    }

    @Test fun mapsNavState() {
        val u = NavUi.from(NavState(can_go_back = true, auto_switch_paused = true, on_home = false, profile_name = "Main"))
        assertEquals(NavUi(canGoBack = true, paused = true, onHome = false, profileName = "Main"), u)
    }

    @Test fun pressClassification() {
        assertEquals(NavPress.Tap, classifyNavPress(true))
        assertEquals(NavPress.Long, classifyNavPress(false))
    }

    @Test fun mapsPageFields() {
        val u = NavUi.from(NavState(page_index = 2, page_count = 3))
        assertEquals(2, u.pageIndex)
        assertEquals(3, u.pageCount)
        assertEquals(0, NavUi().pageCount)
    }

    @Test fun dotTapMakesGoToPageRequest() {
        val r = goToPageRequest(1)
        assertEquals(NavAction.NAV_ACTION_GO_TO_PAGE, r.action)
        assertEquals(1, r.page_index)
    }

    @Test fun xperiaHeightFitsEverything() {
        // 64 home + (6+44) camera + (4+44) mic + keys (6+44+4+44+4+44) = 308; +6+48 back = 362; +6+18 link = 386; +6+14 clock = 406
        val p = planRail(411f, pinned = 3, canGoBack = true, hasLink = true, cameraLive = true, micLive = true)
        assertEquals(RailPlan(true, true, 3, true, true, true), p)
        assertEquals(RailPlan(false, false, 3, false, true, true), planRail(395f, 3, false, true, false))
    }

    @Test fun shortRailDropsLowestPriorityFirst() {
        // chips before pinned keys before Back before link before clock
        assertEquals(RailPlan(true, true, 0, false, false, false), planRail(165f, 3, true, true, true, true))
        assertEquals(RailPlan(true, true, 1, false, false, false), planRail(214f, 3, true, true, true, true))
        assertEquals(RailPlan(true, true, 3, false, false, false), planRail(310f, 3, true, true, true, true))
        assertEquals(RailPlan(true, true, 3, true, false, false), planRail(365f, 3, true, true, true, true))
        assertEquals(RailPlan(true, true, 3, true, true, false), planRail(390f, 3, true, true, true, true))
        assertEquals(RailPlan(true, true, 3, true, true, true), planRail(410f, 3, true, true, true, true))
    }

    @Test fun chipsOutrankPinnedKeys() {
        // 64 + 50 camera = 114: room for the chip, not a key (+50)
        assertEquals(RailPlan(true, false, 0, false, false, false), planRail(120f, 3, true, true, true, false))
        // no chips: the same height shows a key
        assertEquals(RailPlan(false, false, 1, false, false, false), planRail(120f, 3, true, true, false, false))
        // mic alone sits right under Home
        assertEquals(RailPlan(false, true, 0, false, false, false), planRail(120f, 3, true, true, false, true))
    }

    @Test fun homeAlwaysAndNothingElseWhenTiny() {
        assertEquals(RailPlan(false, false, 0, false, false, false), planRail(60f, 3, true, true, true, true))
    }

    @Test fun noStatusWithoutLink() {
        assertEquals(false, planRail(400f, 0, false, false, false).showStatus)
    }
}
