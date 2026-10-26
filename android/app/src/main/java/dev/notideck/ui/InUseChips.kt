// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

internal enum class InUseKind { Camera, Mic }

/** Same colours in every theme: camera red, mic amber. */
internal val CameraChipRed = Color(0xFFE5484D)
internal val MicChipAmber = Color(0xFFF5A524)

/** Pulse period of the in-use chips (alpha 1.0 <-> 0.65 and back). */
private const val CHIP_PULSE_MS = 1600
private const val CHIP_PULSE_MIN = 0.65f

/**
 * Rail chip that shows the phone camera / microphone is being used by the PC: 44 dp rounded square filled
 * with a strong colour, white 24 dp glyph, slow pulse. [onClick] null = display only.
 */
@Composable
internal fun InUseChip(kind: InUseKind, onClick: (() -> Unit)?) {
    val pulse = rememberInfiniteTransition(label = "chipPulse").animateFloat(
        initialValue = 1f, targetValue = CHIP_PULSE_MIN,
        animationSpec = infiniteRepeatable(tween(CHIP_PULSE_MS / 2, easing = LinearEasing), RepeatMode.Reverse),
        label = "chipAlpha"
    )
    val shape = RoundedCornerShape(12.dp)
    Box(
        Modifier
            .size(RailDims.CHIP.dp)
            .graphicsLayer { alpha = pulse.value }
            .clip(shape)
            .background(if (kind == InUseKind.Camera) CameraChipRed else MicChipAmber, shape)
            .semantics { contentDescription = if (kind == InUseKind.Camera) "Camera in use" else "Microphone in use" }
            .then(
                if (onClick == null) Modifier
                else Modifier.clickable(
                    interactionSource = remember { MutableInteractionSource() }, indication = null,
                    role = Role.Button, onClick = onClick
                )
            ),
        contentAlignment = Alignment.Center
    ) {
        Canvas(Modifier.size(24.dp)) {
            if (kind == InUseKind.Camera) drawCameraGlyph(Color.White) else drawMicGlyph(Color.White)
        }
    }
}

/** Camera glyph: body outline with a viewfinder bump and a lens ring. Stroke scales with the canvas. */
internal fun DrawScope.drawCameraGlyph(c: Color) {
    val w = size.width; val h = size.height
    val sw = w * 0.085f
    val body = Path().apply {
        moveTo(w * 0.10f, h * 0.30f)
        lineTo(w * 0.30f, h * 0.30f)
        lineTo(w * 0.38f, h * 0.18f)
        lineTo(w * 0.62f, h * 0.18f)
        lineTo(w * 0.70f, h * 0.30f)
        lineTo(w * 0.90f, h * 0.30f)
        lineTo(w * 0.90f, h * 0.80f)
        lineTo(w * 0.10f, h * 0.80f)
        close()
    }
    drawPath(body, c, style = Stroke(width = sw, cap = StrokeCap.Round, join = StrokeJoin.Round))
    drawCircle(c, w * 0.17f, Offset(w * 0.5f, h * 0.55f), style = Stroke(width = sw))
}

/** Microphone glyph: capsule, pickup arc and stand. Stroke scales with the canvas. */
internal fun DrawScope.drawMicGlyph(c: Color) {
    val w = size.width; val h = size.height
    val sw = w * 0.085f
    drawRoundRect(
        c, Offset(w * 0.37f, h * 0.06f), Size(w * 0.26f, h * 0.50f),
        CornerRadius(w * 0.13f, w * 0.13f)
    )
    drawArc(
        c, startAngle = 0f, sweepAngle = 180f, useCenter = false,
        topLeft = Offset(w * 0.22f, h * 0.22f), size = Size(w * 0.56f, h * 0.50f),
        style = Stroke(width = sw, cap = StrokeCap.Round)
    )
    drawLine(c, Offset(w * 0.5f, h * 0.72f), Offset(w * 0.5f, h * 0.92f), sw, StrokeCap.Round)
}
