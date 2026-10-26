// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.NotFoundException
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.common.HybridBinarizer
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdShapes
import dev.notideck.ui.theme.NdType
import kotlinx.coroutines.delay
import java.util.concurrent.Executors

/**
 * In-app QR scanner (CameraX + ZXing, no Play Services). [onCode] returns what happened with the
 * text; on [PairStart.Started] the caller leaves the screen, otherwise scanning continues and the
 * reason is shown.
 */
@Composable
fun ScannerScreen(onCode: (String) -> PairStart, onBack: () -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    var granted by remember {
        mutableStateOf(ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED)
    }
    var denied by remember { mutableStateOf(false) }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        granted = it
        denied = !it
    }

    Box(modifier.fillMaxSize().background(NdColors.Base)) {
        if (granted) {
            ScannerPreview(onCode, onBack)
        } else {
            Column(
                Modifier.fillMaxSize().padding(horizontal = 28.dp),
                verticalArrangement = Arrangement.Center
            ) {
                Text("Camera", style = NdType.Display.copy(fontSize = 30.sp, lineHeight = 30.sp), color = NdColors.Text)
                Spacer(Modifier.height(12.dp))
                Text(
                    "The camera is only used to read the pairing QR code shown on your PC. Nothing is recorded or sent.",
                    style = NdType.Micro, color = NdColors.TextDim
                )
                if (denied) {
                    Spacer(Modifier.height(8.dp))
                    Text(
                        "Permission denied. Enable the camera for Notideck in system settings, or open the QR with the stock camera app.",
                        style = NdType.Micro, color = NdColors.Alert
                    )
                }
                Spacer(Modifier.height(16.dp))
                Row {
                    NdButton("Allow camera", { launcher.launch(Manifest.permission.CAMERA) }, primary = true)
                    Spacer(Modifier.width(8.dp))
                    NdButton("Back", onBack, primary = false)
                }
            }
        }
    }
}


@Composable
private fun ScannerPreview(onCode: (String) -> PairStart, onBack: () -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val currentOnCode by rememberUpdatedState(onCode)
    var message by remember { mutableStateOf<String?>(null) }
    var lastRejected by remember { mutableStateOf<String?>(null) }
    val executor = remember { Executors.newSingleThreadExecutor() }
    val previewView = remember { PreviewView(context).apply { implementationMode = PreviewView.ImplementationMode.COMPATIBLE } }
    val handler = remember { android.os.Handler(android.os.Looper.getMainLooper()) }

    DisposableEffect(lifecycleOwner) {
        val future = ProcessCameraProvider.getInstance(context)
        var provider: ProcessCameraProvider? = null
        future.addListener({
            val p = future.get()
            provider = p
            val preview = Preview.Builder().build().also { it.setSurfaceProvider(previewView.surfaceProvider) }
            val analysis = ImageAnalysis.Builder()
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()
            analysis.setAnalyzer(executor, QrAnalyzer { text ->
                handler.post {
                    if (text == lastRejected) return@post
                    when (currentOnCode(text)) {
                        PairStart.Started -> Unit
                        PairStart.Expired -> { lastRejected = text; message = "Code expired. Make a new one on the PC." }
                        PairStart.Invalid -> { lastRejected = text; message = "Not a Notideck pairing code." }
                    }
                }
            })
            try {
                p.unbindAll()
                p.bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
            } catch (e: Exception) {
                message = "Camera unavailable: ${e.message}".take(80)
            }
        }, ContextCompat.getMainExecutor(context))
        onDispose {
            provider?.unbindAll()
            executor.shutdown()
        }
    }
    LaunchedEffect(message) {
        if (message != null) { delay(3000); message = null; lastRejected = null }
    }

    AndroidView(factory = { previewView }, modifier = Modifier.fillMaxSize())
    Column(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.SpaceBetween) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.background(NdColors.Base.copy(alpha = 0.8f), NdShapes.Panel).padding(horizontal = 14.dp, vertical = 10.dp)) {
                Text("Point at the QR code on the PC", style = NdType.Label, color = NdColors.Text)
            }
            Spacer(Modifier.weight(1f))
            NdButton("Back", onBack, primary = false)
        }
        message?.let {
            Box(Modifier.background(NdColors.Base.copy(alpha = 0.9f), NdShapes.Panel).padding(horizontal = 14.dp, vertical = 10.dp)) {
                Text(it, style = NdType.Label, color = NdColors.Alert)
            }
        }
    }
}

/** Decodes QR codes from the Y plane of camera frames. */
private class QrAnalyzer(private val onText: (String) -> Unit) : ImageAnalysis.Analyzer {
    private val reader = MultiFormatReader().apply {
        setHints(mapOf(DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE), DecodeHintType.TRY_HARDER to true))
    }

    override fun analyze(image: ImageProxy) {
        try {
            val plane = image.planes[0]
            val buf = plane.buffer
            val rowStride = plane.rowStride
            val data = ByteArray(buf.remaining())
            buf.get(data)
            val height = image.height
            val source = PlanarYUVLuminanceSource(data, rowStride, height, 0, 0, image.width, height, false)
            val result = reader.decodeWithState(BinaryBitmap(HybridBinarizer(source)))
            onText(result.text)
        } catch (_: NotFoundException) {
            // no code in this frame
        } catch (_: Exception) {
        } finally {
            reader.reset()
            image.close()
        }
    }
}
