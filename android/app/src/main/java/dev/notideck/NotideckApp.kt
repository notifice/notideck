// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck

import android.app.Application
import dev.notideck.net.Tls

class NotideckApp : Application() {
    override fun onCreate() {
        super.onCreate()
        // TLS 1.3 on Android 9 comes from Conscrypt, installed ahead of the platform provider.
        Tls.installConscrypt()
    }
}
