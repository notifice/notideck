// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck

import android.content.Intent
import android.os.Bundle
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Surface
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.Modifier
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.notideck.ui.DeckScreen
import dev.notideck.ui.DeckViewModel
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NotideckTheme

class MainActivity : ComponentActivity() {
    /** A `notideck://pair` link that arrived before the UI was ready. */
    private var pairingLink: String? = null
    private var linkSink: ((String) -> Unit)? = null

    private fun Intent.pairingLink(): String? =
        if (action == Intent.ACTION_VIEW) data?.toString()?.takeIf { it.startsWith("notideck://") } else null

    override fun onStart() {
        super.onStart()
        AppRuntime.get(application).onVisible()
    }

    override fun onStop() {
        AppRuntime.peek()?.onHidden() ?: run { dev.notideck.camera.CameraService.appVisible = false }
        super.onStop()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        intent.pairingLink()?.let { link -> linkSink?.invoke(link) ?: run { pairingLink = link } }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null) pairingLink = intent?.pairingLink()

        // Keep screen on for continuous appliance/deck operation
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

        // Immersive fullscreen mode: hide system navigation and status bars
        WindowCompat.setDecorFitsSystemWindows(window, false)
        val insetsController = WindowCompat.getInsetsController(window, window.decorView)
        insetsController.systemBarsBehavior =
            WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        insetsController.hide(WindowInsetsCompat.Type.systemBars())

        setContent {
            val viewModel: DeckViewModel = viewModel()
            LaunchedEffect(Unit) { pairingLink?.let { viewModel.openPairingLink(it); pairingLink = null } }
            DisposableEffect(Unit) {
                linkSink = { viewModel.openPairingLink(it) }
                onDispose { linkSink = null }
            }
            val theme by viewModel.theme.collectAsState()
            val bar = NdColors.Base.toArgb()
            val lightBars = NdColors.Light
            SideEffect {
                window.statusBarColor = bar
                window.navigationBarColor = bar
                // Dark system-bar icons on light themes (bars are hidden but swipe-revealed).
                val c = WindowCompat.getInsetsController(window, window.decorView)
                c.isAppearanceLightStatusBars = lightBars
                c.isAppearanceLightNavigationBars = lightBars
            }
            NotideckTheme(theme) {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = NdColors.Base
                ) {
                    DeckScreen(viewModel = viewModel, icons = viewModel.icons.collectAsState().value)
                }
            }
        }
    }
}
