// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui.theme

import android.content.Context
import android.provider.Settings
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.tween
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalInspectionMode
import dev.notideck.net.AppTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Colour tokens for one phone theme. */
data class NdPalette(
    val base: Color,
    val well: Color,
    val panel: Color,
    val keyFace: Color,
    val keyFacePressed: Color,
    val keyEdgeHi: Color,
    val keyEdgeLo: Color,
    val hairline: Color,
    val track: Color,
    val text: Color,
    val textDim: Color,
    val textFaint: Color,
    val accent: Color,
    val accentDeep: Color,
    val alert: Color,
    /** Light background: dark text, light status bar, `lightColorScheme`. */
    val light: Boolean = false
) {
    fun lerpTo(o: NdPalette, f: Float) = NdPalette(
        lerp(base, o.base, f), lerp(well, o.well, f), lerp(panel, o.panel, f),
        lerp(keyFace, o.keyFace, f), lerp(keyFacePressed, o.keyFacePressed, f),
        lerp(keyEdgeHi, o.keyEdgeHi, f), lerp(keyEdgeLo, o.keyEdgeLo, f),
        lerp(hairline, o.hairline, f), lerp(track, o.track, f), lerp(text, o.text, f),
        lerp(textDim, o.textDim, f), lerp(textFaint, o.textFaint, f), lerp(accent, o.accent, f),
        lerp(accentDeep, o.accentDeep, f), lerp(alert, o.alert, f),
        if (f < 0.5f) light else o.light
    )

    // Derived tokens (computed from the fields above, so they follow lerpTo for free).

    /** Text/glyph colour to draw on an [accent] fill. */
    val onAccent: Color get() = if (light) text else base

    /** Accent used as ink (text, thin glyphs, selected labels) directly on [base]/[keyFace]. */
    val accentInk: Color get() = if (light) accentDeep else accent

    /** Drop-shadow colour (apply low alpha): warm ink on light themes, black on dark ones. */
    val shadow: Color get() = if (light) text else Color.Black

    /** Meter "hot" band: soft amber/peach between accent and alert. */
    val hot: Color get() = if (light) Color(0xFFE2A765) else Color(0xFFF0C27B)

    /** Muted fader fill. */
    val mutedFill: Color get() = textFaint
}

object NdPalettes {
    private val Sage = Color(0xFFA8C478)
    private val SageDeep = Color(0xFF5E7040)
    private val Muted = Color(0xFFD0716B)

    /** AMOLED: pure black background so unlit pixels are off. */
    val Black = NdPalette(
        base = Color(0xFF000000), well = Color(0xFF000000), panel = Color(0xFF0E0F11),
        keyFace = Color(0xFF0E0F11), keyFacePressed = Color(0xFF0A0B0C),
        keyEdgeHi = Color(0xFF1B1D21), keyEdgeLo = Color(0xFF000000),
        hairline = Color(0x1AFFFFFF), track = Color(0xFF1A1C20),
        text = Color(0xFFD9DCE0), textDim = Color(0xFF9AA1AB), textFaint = Color(0xFF5A6068),
        accent = Sage, accentDeep = SageDeep, alert = Muted
    )

    /** Soft dark grey, matching the desktop app. */
    val Graphite = NdPalette(
        base = Color(0xFF121417), well = Color(0xFF0F1114), panel = Color(0xFF181B1F),
        keyFace = Color(0xFF1F2328), keyFacePressed = Color(0xFF181B1F),
        keyEdgeHi = Color(0xFF2A2F36), keyEdgeLo = Color(0xFF14171A),
        hairline = Color(0x1FFFFFFF), track = Color(0xFF2A2F36),
        text = Color(0xFFE3E6EA), textDim = Color(0xFF9AA1AB), textFaint = Color(0xFF5E656E),
        accent = Sage, accentDeep = SageDeep, alert = Muted
    )

    /** Light: warm cream, peach accent. */
    val Cream = NdPalette(
        base = Color(0xFFF6F1EA), well = Color(0xFFEFE8DE), panel = Color(0xFFFBF7F2),
        keyFace = Color(0xFFFFFDFA), keyFacePressed = Color(0xFFF1EAE0),
        keyEdgeHi = Color(0xFFFFFFFF), keyEdgeLo = Color(0xFFE3D9CC),
        hairline = Color(0x1F3A332C), track = Color(0xFFE6DDD1),
        text = Color(0xFF3A332C), textDim = Color(0xFF7A7066), textFaint = Color(0xFFB2A89C),
        accent = Color(0xFFE9A88B), accentDeep = Color(0xFFB9705A), alert = Color(0xFFD98A8A), light = true
    )

    /** Light: pale lilac, lavender accent. */
    val Lavender = NdPalette(
        base = Color(0xFFEFEDF7), well = Color(0xFFE7E4F2), panel = Color(0xFFF7F5FC),
        keyFace = Color(0xFFFCFBFF), keyFacePressed = Color(0xFFECE8F6),
        keyEdgeHi = Color(0xFFFFFFFF), keyEdgeLo = Color(0xFFD9D4EA),
        hairline = Color(0x1F2F2A40), track = Color(0xFFE0DBEE),
        text = Color(0xFF2F2A40), textDim = Color(0xFF6E6884), textFaint = Color(0xFFABA5C0),
        accent = Color(0xFFA99BE0), accentDeep = Color(0xFF6E5FB0), alert = Color(0xFFE59AA8), light = true
    )

    /** Light: pale mint, mint accent. */
    val Mint = NdPalette(
        base = Color(0xFFEDF5F1), well = Color(0xFFE4EFE9), panel = Color(0xFFF6FBF8),
        keyFace = Color(0xFFFCFFFD), keyFacePressed = Color(0xFFE5F0EA),
        keyEdgeHi = Color(0xFFFFFFFF), keyEdgeLo = Color(0xFFD0E2D8),
        hairline = Color(0x1F24352D), track = Color(0xFFDAE9E1),
        text = Color(0xFF24352D), textDim = Color(0xFF63786D), textFaint = Color(0xFFA2B6AB),
        accent = Color(0xFF86C9A8), accentDeep = Color(0xFF3F8A68), alert = Color(0xFFE8A0A0), light = true
    )

    /** Dark: soft plum, lilac accent. */
    val Dusk = NdPalette(
        base = Color(0xFF1E1B26), well = Color(0xFF1A1721), panel = Color(0xFF25212F),
        keyFace = Color(0xFF2C2738), keyFacePressed = Color(0xFF25212F),
        keyEdgeHi = Color(0xFF383247), keyEdgeLo = Color(0xFF17141E),
        hairline = Color(0x1FFFFFFF), track = Color(0xFF383247),
        text = Color(0xFFECE6F5), textDim = Color(0xFFABA2BD), textFaint = Color(0xFF6C6480),
        accent = Color(0xFFC7B3F0), accentDeep = Color(0xFF7D68B8), alert = Color(0xFFF0A6B8)
    )

    /** Dark: slate blue, powder-blue accent. */
    val Tide = NdPalette(
        base = Color(0xFF172027), well = Color(0xFF141C22), panel = Color(0xFF1C2730),
        keyFace = Color(0xFF222F39), keyFacePressed = Color(0xFF1C2730),
        keyEdgeHi = Color(0xFF2E3E4A), keyEdgeLo = Color(0xFF11181D),
        hairline = Color(0x1FFFFFFF), track = Color(0xFF2E3E4A),
        text = Color(0xFFE4EEF2), textDim = Color(0xFF9DB1BA), textFaint = Color(0xFF5D717B),
        accent = Color(0xFF9FD4E0), accentDeep = Color(0xFF4F8D9C), alert = Color(0xFFF2A9A0)
    )

    fun of(theme: AppTheme) = when (theme) {
        AppTheme.Black -> Black
        AppTheme.Graphite -> Graphite
        AppTheme.Cream -> Cream
        AppTheme.Lavender -> Lavender
        AppTheme.Mint -> Mint
        AppTheme.Dusk -> Dusk
        AppTheme.Tide -> Tide
    }
}

/**
 * The palette currently on screen (possibly mid-crossfade). Backed by snapshot state, so
 * reads in composition and in draw lambdas both invalidate when it changes.
 */
object NdThemeState {
    var palette by mutableStateOf(NdPalettes.of(AppTheme.DEFAULT))
        internal set
}

/** Call sites read colours through these live getters. */
object NdColors {
    val Base get() = NdThemeState.palette.base
    val Well get() = NdThemeState.palette.well
    val Panel get() = NdThemeState.palette.panel
    val KeyFace get() = NdThemeState.palette.keyFace
    val KeyFacePressed get() = NdThemeState.palette.keyFacePressed
    val KeyEdgeHi get() = NdThemeState.palette.keyEdgeHi
    val KeyEdgeLo get() = NdThemeState.palette.keyEdgeLo
    val Hairline get() = NdThemeState.palette.hairline
    val Track get() = NdThemeState.palette.track
    val Text get() = NdThemeState.palette.text
    val TextDim get() = NdThemeState.palette.textDim
    val TextFaint get() = NdThemeState.palette.textFaint
    val Accent get() = NdThemeState.palette.accent
    val AccentDeep get() = NdThemeState.palette.accentDeep
    val Alert get() = NdThemeState.palette.alert
    val OnAccent get() = NdThemeState.palette.onAccent
    val AccentInk get() = NdThemeState.palette.accentInk
    val Shadow get() = NdThemeState.palette.shadow
    val Hot get() = NdThemeState.palette.hot
    val MutedFill get() = NdThemeState.palette.mutedFill
    val Light get() = NdThemeState.palette.light
}

object NdShapes {
    val Tray = RoundedCornerShape(26.dp)
    val Key = RoundedCornerShape(22.dp)
    val KeyInner = RoundedCornerShape(12.dp)
    val Slot = RoundedCornerShape(26.dp)
    val Panel = RoundedCornerShape(16.dp)
    val Tag = RoundedCornerShape(10.dp)
    val Pill = RoundedCornerShape(50)
}

/** Rounded-friendly sans (system default); medium weights, sentence case, little tracking. */
object NdType {
    val Sans = FontFamily.SansSerif

    /** 11sp secondary labels. */
    val Micro = TextStyle(
        fontFamily = Sans, fontWeight = FontWeight.Medium,
        fontSize = 11.sp, lineHeight = 14.sp, letterSpacing = 0.2.sp
    )
    val Label = TextStyle(
        fontFamily = Sans, fontWeight = FontWeight.Medium,
        fontSize = 13.sp, lineHeight = 16.sp, letterSpacing = 0.1.sp
    )
    /** Big numerals for tiles / dial readouts. */
    val Readout = TextStyle(
        fontFamily = Sans, fontWeight = FontWeight.SemiBold,
        fontSize = 28.sp, lineHeight = 30.sp, letterSpacing = (-0.5).sp
    )
    val KeyTitle = TextStyle(
        fontFamily = Sans, fontWeight = FontWeight.SemiBold,
        fontSize = 14.sp, lineHeight = 16.sp, letterSpacing = 0.sp
    )
    val Display = TextStyle(
        fontFamily = Sans, fontWeight = FontWeight.SemiBold,
        fontSize = 44.sp, lineHeight = 46.sp, letterSpacing = (-1).sp
    )
}

private fun schemeOf(p: NdPalette) = if (p.light) lightColorScheme(
    primary = p.accentDeep,
    onPrimary = p.base,
    primaryContainer = p.accent,
    onPrimaryContainer = p.onAccent,
    background = p.base,
    onBackground = p.text,
    surface = p.panel,
    onSurface = p.text,
    surfaceVariant = p.keyFace,
    onSurfaceVariant = p.textDim,
    outline = p.hairline,
    error = p.alert
) else darkColorScheme(
    primary = p.accent,
    onPrimary = p.onAccent,
    background = p.base,
    onBackground = p.text,
    surface = p.panel,
    onSurface = p.text,
    surfaceVariant = p.keyFace,
    onSurfaceVariant = p.textDim,
    outline = p.hairline,
    error = p.alert
)

private val NdTypography = Typography(
    titleMedium = NdType.KeyTitle,
    labelMedium = NdType.Label,
    labelSmall = NdType.Micro,
    bodySmall = NdType.Micro.copy(fontWeight = FontWeight.Normal),
    bodyMedium = NdType.Label.copy(fontWeight = FontWeight.Normal)
)

private val NdMaterialShapes = Shapes(
    small = NdShapes.Tag, medium = NdShapes.Panel, large = NdShapes.Key
)

const val THEME_FADE_MS = 250

/** Crossfade duration honouring the system animator scale (0 = instant). */
private fun fadeMillis(context: Context): Int {
    val scale = try {
        Settings.Global.getFloat(context.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f)
    } catch (_: Throwable) { 1f }
    return if (scale <= 0f) 0 else THEME_FADE_MS
}

@Composable
fun NotideckTheme(theme: AppTheme = AppTheme.DEFAULT, content: @Composable () -> Unit) {
    val context = LocalContext.current
    val inPreview = LocalInspectionMode.current
    if (inPreview) NdThemeState.palette = NdPalettes.of(theme)
    val first = remember { booleanArrayOf(true) }
    LaunchedEffect(theme) {
        val from = NdThemeState.palette
        val to = NdPalettes.of(theme)
        val ms = if (inPreview || first[0]) 0 else fadeMillis(context)
        first[0] = false
        if (ms == 0) {
            NdThemeState.palette = to
        } else {
            val anim = Animatable(0f)
            anim.animateTo(1f, tween(ms, easing = LinearEasing)) {
                NdThemeState.palette = from.lerpTo(to, value)
            }
            NdThemeState.palette = to
        }
    }
    val palette = NdThemeState.palette
    MaterialTheme(
        colorScheme = schemeOf(palette),
        typography = NdTypography,
        shapes = NdMaterialShapes,
        content = content
    )
}
