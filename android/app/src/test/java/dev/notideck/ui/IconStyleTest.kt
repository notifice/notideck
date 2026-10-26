// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

class IconStyleTest {
    private val clear = 0x00000000
    private val glyphGrey = 0xFFE3E6EA.toInt() // host built-in glyph colour
    private val white = 0xFFFFFFFF.toInt()

    /** 10x10 image: [ink] on the first [inked] pixels, [rest] elsewhere. */
    private fun img(inked: Int, ink: Int, rest: Int = clear) = IntArray(100) { if (it < inked) ink else rest }

    private fun cls(px: IntArray) = IconStyle.classify(px)

    @Test fun lightGlyph() = assertEquals(IconClass.GLYPH, cls(img(30, glyphGrey)))
    @Test fun whiteGlyph() = assertEquals(IconClass.GLYPH, cls(img(50, white)))
    @Test fun darkGlyph() = assertEquals(IconClass.GLYPH, cls(img(40, 0xFF202020.toInt())))

    @Test fun fewColouredAntialiasPixelsTolerated() {
        val px = img(40, white)
        px[0] = 0xFF3366CC.toInt()
        assertEquals(IconClass.GLYPH, cls(px))
    }

    @Test fun midGreyIsCutout() = assertEquals(IconClass.CUTOUT, cls(img(40, 0xFF808080.toInt())))

    @Test fun colouredLogoWithMarginIsCutout() = assertEquals(IconClass.CUTOUT, cls(img(40, 0xFF1DB954.toInt())))

    @Test fun mixedLightAndDarkIsCutout() {
        val px = img(40, white)
        for (i in 0 until 20) px[i] = 0xFF101010.toInt()
        assertEquals(IconClass.CUTOUT, cls(px))
    }

    @Test fun halfAlphaPixelsAreIgnored() {
        // 30 white strokes + 40 half-transparent dark fringe pixels: still a glyph.
        val px = img(30, white)
        for (i in 30 until 70) px[i] = 0x70101010.toInt()
        assertEquals(IconClass.GLYPH, cls(px))
    }

    @Test fun opaquePhotoIsArtwork() = assertEquals(IconClass.ARTWORK, cls(img(100, white)))

    @Test fun mostlyOpaqueIsArtwork() = assertEquals(IconClass.ARTWORK, cls(img(90, 0xFF1DB954.toInt())))

    @Test fun fullyTransparentIsNotGlyph() = assertNotEquals(IconClass.GLYPH, cls(img(0, white)))

    @Test fun emptyIsNotGlyph() = assertNotEquals(IconClass.GLYPH, cls(IntArray(0)))

    @Test fun lightFaceOnLightThemeKeepsThemeInk() = assertEquals(InkTone.THEME, IconStyle.inkTone(0.8f, light = true))
    @Test fun darkFaceOnLightThemeGetsLightInk() = assertEquals(InkTone.LIGHT, IconStyle.inkTone(0.1f, light = true))
    @Test fun lightFaceOnDarkThemeGetsDarkInk() = assertEquals(InkTone.DARK, IconStyle.inkTone(0.7f, light = false))
    @Test fun darkFaceOnDarkThemeKeepsThemeInk() = assertEquals(InkTone.THEME, IconStyle.inkTone(0.05f, light = false))
    @Test fun thresholdIsExclusive() {
        assertEquals(InkTone.LIGHT, IconStyle.inkTone(0.40f, light = true))
        assertEquals(InkTone.THEME, IconStyle.inkTone(0.40f, light = false))
    }
}
