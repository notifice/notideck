// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui.theme

import androidx.compose.ui.graphics.Color
import dev.notideck.net.AppTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.math.pow

class PaletteContrastTest {
    private fun lin(c: Float): Double =
        if (c <= 0.03928f) c / 12.92 else ((c + 0.055) / 1.055).pow(2.4)

    private fun luminance(c: Color): Double = 0.2126 * lin(c.red) + 0.7152 * lin(c.green) + 0.0722 * lin(c.blue)

    private fun contrast(a: Color, b: Color): Double {
        val la = luminance(a); val lb = luminance(b)
        return (maxOf(la, lb) + 0.05) / (minOf(la, lb) + 0.05)
    }

    @Test
    fun textOnBaseMeetsAA() {
        for (t in AppTheme.entries) {
            val p = NdPalettes.of(t)
            val r = contrast(p.text, p.base)
            assertTrue("$t text/base = $r", r >= 4.5)
        }
    }

    @Test
    fun textDimOnBaseMeetsLargeTextAA() {
        for (t in AppTheme.entries) {
            val p = NdPalettes.of(t)
            val r = contrast(p.textDim, p.base)
            assertTrue("$t textDim/base = $r", r >= 3.0)
        }
    }

    @Test
    fun onAccentIsReadableOnAccent() {
        for (t in AppTheme.entries) {
            val p = NdPalettes.of(t)
            val r = contrast(p.onAccent, p.accent)
            assertTrue("$t onAccent/accent = $r", r >= 4.5)
        }
    }

    @Test
    fun accentInkIsVisibleOnKeyFace() {
        for (t in AppTheme.entries) {
            val p = NdPalettes.of(t)
            val r = contrast(p.accentInk, p.keyFace)
            assertTrue("$t accentInk/keyFace = $r", r >= 3.0)
        }
    }

    @Test
    fun lightFlagMatchesBaseLuminance() {
        for (t in AppTheme.entries) {
            val p = NdPalettes.of(t)
            assertEquals("$t", luminance(p.base) > 0.5, p.light)
        }
    }

    @Test
    fun derivedTokensFollowLerp() {
        val a = NdPalettes.Black
        val b = NdPalettes.Cream
        assertEquals(a.hot, a.lerpTo(b, 0.2f).hot)
        assertEquals(b.hot, a.lerpTo(b, 0.8f).hot)
    }
}
