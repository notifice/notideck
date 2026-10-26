// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.mic

import dev.notideck.proto.v1.MicStart

/** What to do with a MicStart. */
sealed class MicPlan {
    /** Open the recorder now. */
    object Begin : MicPlan()
    /** RECORD_AUDIO is missing: report NO_PERMISSION; [askNow] when the app is visible so the system dialog can show. */
    data class NeedPermission(val askNow: Boolean) : MicPlan()
    /** The app is in the background and not holding a microphone-type foreground service (Android 11+). */
    object NeedForeground : MicPlan()
}

/** What to do when the app comes to the front. */
data class MicVisibleAction(val retry: MicStart?, val askPermission: Boolean)

/**
 * Permission and foreground bookkeeping for the microphone, kept free of Android types.
 * A start waiting for the permission is asked about once per visible occasion: a denial drops it
 * (status stays NO_PERMISSION) and only a new MicStart from the PC asks again.
 */
class MicRequestLogic {
    var pending: MicStart? = null
        private set
    var deferred: MicStart? = null
        private set

    fun onStart(m: MicStart, granted: Boolean, visible: Boolean, canRunInBackground: Boolean): MicPlan {
        pending = null; deferred = null
        if (!granted) { pending = m; return MicPlan.NeedPermission(visible) }
        if (!visible && !canRunInBackground) { deferred = m; return MicPlan.NeedForeground }
        return MicPlan.Begin
    }

    fun onStop() { pending = null; deferred = null }

    /** The system dialog answered. Returns the start to run (granted only); a denial drops it. */
    fun onPermissionResult(granted: Boolean): MicStart? {
        val p = pending
        pending = null
        return if (granted) p else null
    }

    /** The app became visible. */
    fun onVisible(granted: Boolean): MicVisibleAction {
        val d = deferred
        if (d != null) { deferred = null; return MicVisibleAction(d, false) }
        val p = pending ?: return MicVisibleAction(null, false)
        if (granted) { pending = null; return MicVisibleAction(p, false) }
        return MicVisibleAction(null, true)
    }
}
