// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.luminance
import dev.notideck.ui.theme.NdColors

/** How a key/tile/slider icon is drawn; decided once per bitmap by [IconStyle.classify]. */
internal enum class IconClass {
    /** Monochrome light or dark strokes on transparency: recoloured with the key's ink, fitted. */
    GLYPH,
    /** Transparent margin but coloured or mid-tone (app icons, logos): drawn as-is, fitted, never cropped. */
    CUTOUT,
    /** Opaque picture (photo, album art, still): full bleed. */
    ARTWORK
}

/** Which ink a key needs; resolved to a colour by [resolveInk]. Pure, for tests. */
internal enum class InkTone {
    /** The theme text colour already contrasts with the face. */
    THEME,
    /** Fixed dark ink (dark theme with a light tinted face). */
    DARK,
    /** Fixed light ink (light theme with a dark tinted face). */
    LIGHT
}

/**
 * Pure icon classifier and ink choice shared by keys, tiles and sliders. No Compose, so it is
 * unit-testable. The rules mirror the config UI (`iconview.js`); see docs/PLAN.md "Round 24".
 */
internal object IconStyle {
    /** At least this share of pixels must be (near) transparent for GLYPH/CUTOUT. */
    const val MIN_TRANSPARENT = 0.20f
    /** Alpha below this is transparent; at or above [OPAQUE_ALPHA] is opaque; in between is ignored. */
    const val TRANSPARENT_ALPHA = 16
    const val OPAQUE_ALPHA = 128
    /** Opaque pixels with a larger channel spread count as coloured. */
    const val MAX_CHROMA = 40
    /** Opaque pixels at or above / at or below these lumas (0..255) count as light / dark ink. */
    const val LIGHT_LUMA = 160
    const val DARK_LUMA = 96
    /** Share of opaque pixels allowed to be coloured or off-tone; also the share that must be on-tone is 1 - this. */
    const val TOLERANCE = 0.05f
    /** Face luminance above this wants dark ink. */
    const val FACE_LIGHT = 0.40f

    val DarkInk = Color(0xFF16181B)
    val LightInk = Color(0xFFF7F5F2)

    /** Classifies [argb] (ARGB_8888 pixels). */
    fun classify(argb: IntArray): IconClass {
        if (argb.isEmpty()) return IconClass.ARTWORK
        var transparent = 0
        var opaque = 0
        var coloured = 0
        var light = 0
        var dark = 0
        for (p in argb) {
            val a = p ushr 24
            if (a < TRANSPARENT_ALPHA) { transparent++; continue }
            if (a < OPAQUE_ALPHA) continue
            opaque++
            val r = (p shr 16) and 0xFF
            val g = (p shr 8) and 0xFF
            val b = p and 0xFF
            if (maxOf(r, g, b) - minOf(r, g, b) > MAX_CHROMA) coloured++
            val luma = (r * 299 + g * 587 + b * 114) / 1000
            if (luma >= LIGHT_LUMA) light++
            if (luma <= DARK_LUMA) dark++
        }
        if (transparent < argb.size * MIN_TRANSPARENT) return IconClass.ARTWORK
        if (opaque == 0) return IconClass.CUTOUT
        val onTone = opaque * (1f - TOLERANCE)
        val glyph = coloured <= opaque * TOLERANCE && (light >= onTone || dark >= onTone)
        return if (glyph) IconClass.GLYPH else IconClass.CUTOUT
    }

    /**
     * Ink for a key whose face has relative luminance [faceLuminance] on a [light] or dark theme:
     * a light face wants dark ink, a dark face light ink; [InkTone.THEME] when the theme text already fits.
     */
    fun inkTone(faceLuminance: Float, light: Boolean): InkTone =
        if (faceLuminance > FACE_LIGHT) { if (light) InkTone.THEME else InkTone.DARK }
        else { if (light) InkTone.LIGHT else InkTone.THEME }
}

/** Maps [tone] to a colour for the current theme. */
internal fun resolveInk(tone: InkTone): Color = when (tone) {
    InkTone.THEME -> NdColors.Text
    InkTone.DARK -> IconStyle.DarkInk
    InkTone.LIGHT -> IconStyle.LightInk
}

/** Key face colour (top of the gradient) for an optional `bg_argb` [tint]; shared by [KeyCap] and the ink choice. */
internal fun keyFaceTop(tint: Color?): Color = tint?.let { lerp(NdColors.KeyFace, it, 0.40f) } ?: NdColors.KeyFace

/** Title/glyph ink for a key: [fg] when explicitly set, otherwise the theme text flipped if the tinted face needs it. */
internal fun keyInk(fg: Color, fgSet: Boolean, tint: Color?): Color =
    if (fgSet) fg else resolveInk(IconStyle.inkTone(keyFaceTop(tint).luminance(), NdColors.Light))

private val classCache = java.util.Collections.synchronizedMap(java.util.WeakHashMap<ImageBitmap, IconClass>())

/** Class of [bmp], read from its pixels once per bitmap. */
@Composable
internal fun rememberIconClass(bmp: ImageBitmap): IconClass = remember(bmp) {
    classCache.getOrPut(bmp) {
        val px = IntArray(bmp.width * bmp.height)
        bmp.readPixels(px)
        IconStyle.classify(px)
    }
}
