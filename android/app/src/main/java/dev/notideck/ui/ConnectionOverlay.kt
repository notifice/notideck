// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.keyframes
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.notideck.net.ConnectionStatus
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NdShapes
import dev.notideck.ui.theme.NdType
import kotlin.math.cos
import kotlin.math.sin

private const val TARGET = "USB 127.0.0.1:47800  /  LAN paired PCs"

private fun headline(status: ConnectionStatus): String = when (status) {
    is ConnectionStatus.Reconnecting -> "No PC found. Retrying."
    is ConnectionStatus.Connecting -> "Searching for your PC."
    is ConnectionStatus.WaitingApproval -> "Waiting for approval on ${status.hostName}\u2026"
    is ConnectionStatus.Pairing -> "Pairing with ${status.hostName}\u2026"
    is ConnectionStatus.Paired -> "Paired \u2713  ${status.hostName}"
    is ConnectionStatus.Notice -> status.message
    else -> "Waiting for host."
}

private fun isAlert(status: ConnectionStatus) =
    status is ConnectionStatus.Reconnecting || (status is ConnectionStatus.Notice && status.isError)

/** Overlay for connecting / reconnecting / waiting for the host. */
@Composable
fun ConnectionOverlay(
    status: ConnectionStatus,
    hasLayout: Boolean,
    onOpenPcs: () -> Unit = {},
    onScanQr: () -> Unit = {},
    modifier: Modifier = Modifier
) {
    val isVisible = status !is ConnectionStatus.Connected

    AnimatedVisibility(
        visible = isVisible,
        enter = fadeIn(tween(300)),
        exit = fadeOut(tween(300))
    ) {
        if (!hasLayout) {
            WaitingScreen(status, onOpenPcs, onScanQr, modifier)
        } else {
            Box(modifier.fillMaxSize().background(NdColors.Base.copy(alpha = 0.55f))) {
                LinkLostStrip(status, onOpenPcs, Modifier.align(Alignment.BottomCenter))
            }
        }
    }
}

@Composable
private fun WaitingScreen(
    status: ConnectionStatus,
    onOpenPcs: () -> Unit = {},
    onScanQr: () -> Unit = {},
    modifier: Modifier = Modifier
) {
    BoxWithConstraints(
        modifier
            .fillMaxSize()
            .background(NdColors.Base)
    ) {
        val wide = maxWidth > maxHeight
        val content: @Composable () -> Unit = {
            Column(
                Modifier
                    .padding(horizontal = 28.dp, vertical = 24.dp),
                verticalArrangement = Arrangement.Center
            ) {
                StatusTag(status)
                Spacer(Modifier.height(14.dp))
                Text("Noti\ndeck", style = NdType.Display, color = NdColors.Text)
                Spacer(Modifier.height(16.dp))
                Text(
                    text = headline(status),
                    style = NdType.Label,
                    color = if (status is ConnectionStatus.Paired) NdColors.AccentInk else NdColors.TextDim
                )
                if (status is ConnectionStatus.Reconnecting && status.lastError != null) {
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "Error: ${status.lastError}".take(80),
                        style = NdType.Micro,
                        color = NdColors.Alert,
                        maxLines = 2
                    )
                }
            }
        }
        val rack: @Composable () -> Unit = {
            Column(
                Modifier.padding(horizontal = 28.dp, vertical = 24.dp),
                verticalArrangement = Arrangement.Center
            ) {
                Radar(Modifier.size(if (wide) 120.dp else 96.dp).align(Alignment.CenterHorizontally))
                Spacer(Modifier.height(20.dp))
                LinkRow("1", "USB", "cable, USB debugging", primary = true)
                Spacer(Modifier.height(8.dp))
                LinkRow("2", "Wi-Fi", "paired PC, same network", primary = false)
                Spacer(Modifier.height(12.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    NdButton("PCs", onOpenPcs, primary = false, modifier = Modifier.weight(1f))
                    NdButton("Scan QR", onScanQr, primary = true, modifier = Modifier.weight(1f))
                }
            }
        }
        if (wide) {
            Row(Modifier.fillMaxSize(), verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.weight(1f)) { content() }
                Box(Modifier.width(1.dp).fillMaxSize().padding(vertical = 24.dp).background(NdColors.Hairline))
                Box(Modifier.weight(1f)) { rack() }
            }
        } else {
            Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.Center) {
                content()
                Box(Modifier.fillMaxWidth().height(1.dp).padding(horizontal = 28.dp).background(NdColors.Hairline))
                rack()
            }
        }
    }
}

/** Soft pill button: pastel accent fill when primary, quiet panel otherwise. 48 dp minimum height. */
@Composable
fun NdButton(label: String, onClick: () -> Unit, primary: Boolean, modifier: Modifier = Modifier) {
    val shape = NdShapes.Panel
    Box(
        modifier
            .heightIn(min = 48.dp)
            .clip(shape)
            .background(if (primary) NdColors.Accent else NdColors.Panel)
            .border(1.dp, if (primary) Color.Transparent else NdColors.Hairline, shape)
            .clickable(onClick = onClick)
            .padding(horizontal = 18.dp, vertical = 10.dp),
        contentAlignment = Alignment.Center
    ) {
        Text(label, style = NdType.Label, color = if (primary) NdColors.OnAccent else NdColors.Text)
    }
}

@Composable
private fun StatusTag(status: ConnectionStatus) {
    val t = rememberInfiniteTransition(label = "tag")
    val blink by t.animateFloat(
        initialValue = 1f, targetValue = 1f,
        animationSpec = infiniteRepeatable(keyframes {
            durationMillis = 1100
            1f at 0; 1f at 550; 0.15f at 560; 0.15f at 1100
        }),
        label = "blink"
    )
    val label = when (status) {
        is ConnectionStatus.Reconnecting -> "Retry ${status.attempt}"
        is ConnectionStatus.Connecting -> "Searching"
        is ConnectionStatus.WaitingApproval -> "Pending"
        is ConnectionStatus.Pairing -> "Pairing"
        is ConnectionStatus.Paired -> "Paired"
        is ConnectionStatus.Notice -> "Notice"
        else -> "Standby"
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(
            Modifier
                .size(8.dp)
                .background(
                    (if (isAlert(status)) NdColors.Alert else NdColors.Accent)
                        .copy(alpha = blink),
                    CircleShape
                )
        )
        Spacer(Modifier.width(8.dp))
        Text(label, style = NdType.Micro, color = NdColors.TextDim)
    }
}

@Composable
private fun LinkRow(index: String, name: String, hint: String, primary: Boolean) {
    Row(
        Modifier
            .fillMaxWidth()
            .background(NdColors.Panel, NdShapes.Panel)
            .border(1.dp, if (primary) NdColors.Accent.copy(alpha = 0.7f) else NdColors.Hairline, NdShapes.Panel)
            .padding(horizontal = 14.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Text(index, style = NdType.Micro, color = NdColors.TextFaint)
        Spacer(Modifier.width(12.dp))
        Text(name, style = NdType.Label, color = if (primary) NdColors.AccentInk else NdColors.Text)
        Spacer(Modifier.width(12.dp))
        Text(hint, style = NdType.Micro, color = NdColors.TextDim, maxLines = 1)
    }
}

/** Slow radar sweep: static rings, a rotating wedge, one blip. Transform/draw-only animation. */
@Composable
private fun Radar(modifier: Modifier = Modifier) {
    val t = rememberInfiniteTransition(label = "radar")
    val angle by t.animateFloat(
        0f, 360f,
        infiniteRepeatable(tween(3200, easing = LinearEasing), RepeatMode.Restart),
        label = "sweep"
    )
    Canvas(modifier) {
        val r = size.minDimension / 2f
        val ring = Stroke(1.dp.toPx())
        for (k in 1..3) {
            drawCircle(NdColors.Hairline.copy(alpha = 0.18f), r * k / 3f, style = ring)
        }
        drawLine(NdColors.Hairline.copy(alpha = 0.18f), Offset(center.x - r, center.y), Offset(center.x + r, center.y), 1.dp.toPx())
        drawLine(NdColors.Hairline.copy(alpha = 0.18f), Offset(center.x, center.y - r), Offset(center.x, center.y + r), 1.dp.toPx())
        drawArc(
            color = NdColors.Accent.copy(alpha = 0.14f),
            startAngle = angle - 90f, sweepAngle = 90f, useCenter = true,
            topLeft = Offset(center.x - r, center.y - r), size = androidx.compose.ui.geometry.Size(r * 2, r * 2)
        )
        val a = Math.toRadians((angle).toDouble())
        drawLine(
            NdColors.Accent,
            center,
            Offset(center.x + r * cos(a).toFloat(), center.y + r * sin(a).toFloat()),
            1.5.dp.toPx()
        )
        drawCircle(NdColors.Accent, 2.5.dp.toPx(), center)
    }
}

@Composable
private fun LinkLostStrip(status: ConnectionStatus, onOpenPcs: () -> Unit = {}, modifier: Modifier = Modifier) {
    val t = rememberInfiniteTransition(label = "strip")
    val blink by t.animateFloat(
        0.2f, 1f,
        infiniteRepeatable(tween(700, easing = LinearEasing), RepeatMode.Reverse),
        label = "stripBlink"
    )
    Row(
        modifier
            .padding(bottom = 18.dp)
            .background(NdColors.Panel, NdShapes.Panel)
            .border(1.dp, NdColors.Alert.copy(alpha = 0.6f), NdShapes.Panel)
            .padding(horizontal = 14.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Box(Modifier.size(8.dp).background(NdColors.Alert.copy(alpha = blink), CircleShape))
        Spacer(Modifier.width(10.dp))
        Text(
            when (status) {
                is ConnectionStatus.Reconnecting -> "Link lost  \u00b7  retry ${status.attempt}"
                is ConnectionStatus.Connecting -> "Reconnecting"
                is ConnectionStatus.Disconnected -> "Offline"
                else -> headline(status)
            },
            style = NdType.Label, color = NdColors.Text
        )
        Spacer(Modifier.width(12.dp))
        NdButton("PCs", onOpenPcs, primary = false)
    }
}

@Preview(widthDp = 760, heightDp = 360)
@Composable
private fun WaitingPreview() {
    dev.notideck.ui.theme.NotideckTheme { WaitingScreen(ConnectionStatus.Reconnecting(3, 2000, "connection refused")) }
}

@Preview(widthDp = 760, heightDp = 360)
@Composable
private fun StripPreview() {
    dev.notideck.ui.theme.NotideckTheme {
        Box(Modifier.fillMaxSize().background(NdColors.Base)) {
            LinkLostStrip(ConnectionStatus.Reconnecting(2, 2000), modifier = Modifier.align(Alignment.BottomCenter))
        }
    }
}
