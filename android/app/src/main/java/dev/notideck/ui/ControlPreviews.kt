// SPDX-License-Identifier: GPL-3.0-or-later
package dev.notideck.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import dev.notideck.proto.v1.Control
import dev.notideck.proto.v1.ControlKind
import dev.notideck.net.AppTheme
import dev.notideck.ui.theme.NdColors
import dev.notideck.ui.theme.NotideckTheme

private val sample = mapOf(
    "mute" to Control(id = "mute", kind = ControlKind.CONTROL_KIND_KEY, col = 0, row = 0, title = "Mic mute", subtitle = "live", state_index = 1),
    "scene" to Control(id = "scene", kind = ControlKind.CONTROL_KIND_KEY, col = 1, row = 0, title = "Scene 2"),
    "vol" to Control(id = "vol", kind = ControlKind.CONTROL_KIND_DIAL, col = 2, row = 0, span_w = 1, span_h = 2, title = "Master", value_ = 0.62f),
    "cpu" to Control(id = "cpu", kind = ControlKind.CONTROL_KIND_TILE, col = 3, row = 0, title = "CPU", subtitle = "7950X", value_ = 0.47f),
    "clock" to Control(id = "clock", kind = ControlKind.CONTROL_KIND_TILE, col = 4, row = 0, title = "Clock", subtitle = "21:47"),
    "fader" to Control(id = "fader", kind = ControlKind.CONTROL_KIND_SLIDER, col = 0, row = 1, span_w = 2, title = "Game", value_ = 0.35f, horizontal = true),
    "mic" to Control(id = "mic", kind = ControlKind.CONTROL_KIND_SLIDER, col = 3, row = 1, span_h = 2, title = "Mic", value_ = 0.8f),
    "mKey" to Control(id = "mKey", kind = ControlKind.CONTROL_KIND_KEY, title = "Spotify", meter = true, level = 0.6f),
    "mTile" to Control(id = "mTile", kind = ControlKind.CONTROL_KIND_TILE, title = "Chrome", subtitle = "app", meter = true, level = 0.6f),
    "mDial" to Control(id = "mDial", kind = ControlKind.CONTROL_KIND_DIAL, title = "Game", value_ = 0.62f, meter = true, level = 0.6f),
    "mFaderV" to Control(id = "mFaderV", kind = ControlKind.CONTROL_KIND_SLIDER, title = "Music", value_ = 0.7f, meter = true, level = 0.6f),
    "mFaderH" to Control(id = "mFaderH", kind = ControlKind.CONTROL_KIND_SLIDER, title = "Music", value_ = 0.7f, horizontal = true, meter = true, level = 0.9f),
    "dimFader" to Control(id = "dimFader", kind = ControlKind.CONTROL_KIND_SLIDER, title = "Empty", value_ = 0.5f, meter = true, dimmed = true),
    "rec" to Control(id = "rec", kind = ControlKind.CONTROL_KIND_KEY, col = 4, row = 1, title = "REC", bg_argb = 0xFF7A1F16.toInt(), state_index = 1)
)

@Composable
private fun Themed(theme: AppTheme, content: @Composable () -> Unit) = NotideckTheme(theme) {
    Box(Modifier.fillMaxSize().background(NdColors.Base)) { content() }
}

private val previewIcons: Map<String, ImageBitmap> = run {
    val bmp = ImageBitmap(64, 64)
    val c = androidx.compose.ui.graphics.Canvas(bmp)
    c.drawRoundRect(0f, 0f, 64f, 64f, 14f, 14f, androidx.compose.ui.graphics.Paint().apply { color = Color(0xFF2F5BD8) })
    c.drawCircle(androidx.compose.ui.geometry.Offset(32f, 32f), 14f, androidx.compose.ui.graphics.Paint().apply { color = Color.White })
    fun icon(draw: (androidx.compose.ui.graphics.Canvas) -> Unit) = ImageBitmap(144, 144).also { draw(androidx.compose.ui.graphics.Canvas(it)) }
    fun paint(c: Color) = androidx.compose.ui.graphics.Paint().apply { color = c }
    val centre = androidx.compose.ui.geometry.Offset(72f, 72f)
    mapOf(
        "h" to bmp,
        // Round 24 classes: pale glyph, dark glyph, coloured cutout (transparent margin), opaque artwork.
        "glyphLight" to icon { it.drawCircle(centre, 34f, paint(Color(0xFFE3E6EA))) },
        "glyphDark" to icon { it.drawCircle(centre, 34f, paint(Color(0xFF202020))) },
        "cutout" to icon { it.drawRoundRect(24f, 24f, 120f, 120f, 24f, 24f, paint(Color(0xFF1DB954))) },
        "art" to icon { it.drawRect(0f, 0f, 144f, 144f, paint(Color(0xFFB5651D))); it.drawCircle(centre, 40f, paint(Color(0xFFF2D16B))) }
    )
}

private fun fader(id: String, v: Float, level: Float = 0.62f, muted: Boolean = false, icon: Boolean = true,
                  horizontal: Boolean = false, title: String = "Spotify") =
    Control(id = id, kind = ControlKind.CONTROL_KIND_SLIDER, title = title, value_ = v, horizontal = horizontal,
        meter = true, level = level, state_index = if (muted) 1 else 0, icon_hash = if (icon) "h" else "")

private val faders = mapOf(
    "f100" to fader("f100", 1f, 0.95f),
    "f50" to fader("f50", 0.5f, 0.55f),
    "f0" to fader("f0", 0f, 0f),
    "fMuted" to fader("fMuted", 0.7f, 0.8f, muted = true),
    "fShort" to fader("fShort", 0.6f, 0.5f).copy(span_h = 1),
    "fHoriz" to fader("fHoriz", 0.7f, 0.7f, horizontal = true),
    "fHorizMuted" to fader("fHorizMuted", 0.7f, 0.7f, muted = true, horizontal = true),
    "fNoIcon" to fader("fNoIcon", 0.4f, 0.4f, icon = false, title = "Master")
)

@Composable
private fun Item(id: String, theme: AppTheme) = Themed(theme) {
    DeckControlItem(sample[id] ?: faders.getValue(id), icons = previewIcons, onInput = { _, _, _ -> })
}

@Composable
private fun Grid(theme: AppTheme) = Themed(theme) {
    DeckGrid(cols = 5, rows = 3, controls = sample, onInput = { _, _, _, _ -> })
}

@Preview(name = "Grid Black", widthDp = 760, heightDp = 360, backgroundColor = 0xFF000000, showBackground = true)
@Composable private fun GridBlack() = Grid(AppTheme.Black)
@Preview(name = "Grid Graphite", widthDp = 760, heightDp = 360, backgroundColor = 0xFF121417, showBackground = true)
@Composable private fun GridGraphite() = Grid(AppTheme.Graphite)

@Preview(name = "Key Black", widthDp = 120, heightDp = 120, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun KeyBlack() = Item("mute", AppTheme.Black)
@Preview(name = "Key Graphite", widthDp = 120, heightDp = 120, showBackground = true, backgroundColor = 0xFF121417)
@Composable private fun KeyGraphite() = Item("mute", AppTheme.Graphite)

@Preview(name = "Dial Black", widthDp = 120, heightDp = 240, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun DialBlack() = Item("vol", AppTheme.Black)
@Preview(name = "Dial Graphite", widthDp = 120, heightDp = 240, showBackground = true, backgroundColor = 0xFF121417)
@Composable private fun DialGraphite() = Item("vol", AppTheme.Graphite)

@Preview(name = "SliderH Black", widthDp = 240, heightDp = 100, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun SliderHBlack() = Item("fader", AppTheme.Black)
@Preview(name = "SliderH Graphite", widthDp = 240, heightDp = 100, showBackground = true, backgroundColor = 0xFF121417)
@Composable private fun SliderHGraphite() = Item("fader", AppTheme.Graphite)

@Preview(name = "SliderV Black", widthDp = 100, heightDp = 240, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun SliderVBlack() = Item("mic", AppTheme.Black)
@Preview(name = "SliderV Graphite", widthDp = 100, heightDp = 240, showBackground = true, backgroundColor = 0xFF121417)
@Composable private fun SliderVGraphite() = Item("mic", AppTheme.Graphite)

@Preview(name = "Tile Black", widthDp = 140, heightDp = 120, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun TileBlack() = Item("cpu", AppTheme.Black)
@Preview(name = "Tile Graphite", widthDp = 140, heightDp = 120, showBackground = true, backgroundColor = 0xFF121417)
@Composable private fun TileGraphite() = Item("cpu", AppTheme.Graphite)

@Preview(name = "Meter Key", widthDp = 120, heightDp = 120, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun MeterKey() = Item("mKey", AppTheme.Black)
@Preview(name = "Meter Tile", widthDp = 140, heightDp = 120, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun MeterTile() = Item("mTile", AppTheme.Black)
@Preview(name = "Meter Dial", widthDp = 120, heightDp = 240, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun MeterDial() = Item("mDial", AppTheme.Black)
@Preview(name = "Meter SliderV", widthDp = 100, heightDp = 240, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun MeterSliderV() = Item("mFaderV", AppTheme.Black)
@Preview(name = "Meter SliderH", widthDp = 240, heightDp = 100, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun MeterSliderH() = Item("mFaderH", AppTheme.Black)
@Preview(name = "Dimmed Slider", widthDp = 100, heightDp = 240, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun DimmedSlider() = Item("dimFader", AppTheme.Black)

private fun media(span_w: Int, span_h: Int, dimmed: Boolean = false, art: Boolean = true) =
    Control(id = "media", kind = ControlKind.CONTROL_KIND_TILE, span_w = span_w, span_h = span_h,
        face = dev.notideck.proto.v1.ControlFace.CONTROL_FACE_MEDIA, title = "Teardrop", subtitle = "Massive Attack",
        detail = "1:23 / 3:45", value_ = 0.37f, state_index = 1, dimmed = dimmed, icon_hash = if (art) "h" else "")

@Composable
private fun MediaItem(c: Control) = Themed(AppTheme.Black) { DeckControlItem(c, icons = previewIcons, onInput = { _, _, _ -> }) }

@Preview(name = "Media wide", widthDp = 300, heightDp = 100, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun MediaWide() = MediaItem(media(3, 1))
@Preview(name = "Media wide no art", widthDp = 300, heightDp = 100, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun MediaWideNoArt() = MediaItem(media(3, 1, art = false))
@Preview(name = "Media tall", widthDp = 140, heightDp = 240, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun MediaTall() = MediaItem(media(2, 2))
@Preview(name = "Media 1x1", widthDp = 100, heightDp = 100, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun MediaMini() = MediaItem(media(1, 1))
@Preview(name = "Media dimmed", widthDp = 300, heightDp = 100, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun MediaDimmed() = MediaItem(media(3, 1, dimmed = true))

@Preview(name = "Fader 100", widthDp = 100, heightDp = 300, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun Fader100() = Item("f100", AppTheme.Black)
@Preview(name = "Fader 50", widthDp = 100, heightDp = 300, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun Fader50() = Item("f50", AppTheme.Black)
@Preview(name = "Fader 0", widthDp = 100, heightDp = 300, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun Fader0() = Item("f0", AppTheme.Black)
@Preview(name = "Fader muted", widthDp = 100, heightDp = 300, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun FaderMuted() = Item("fMuted", AppTheme.Black)
@Preview(name = "Fader short", widthDp = 100, heightDp = 150, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun FaderShort() = Item("fShort", AppTheme.Graphite)
@Preview(name = "Fader horizontal", widthDp = 300, heightDp = 100, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun FaderHoriz() = Item("fHoriz", AppTheme.Black)
@Preview(name = "Fader horizontal muted", widthDp = 300, heightDp = 100, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun FaderHorizMuted() = Item("fHorizMuted", AppTheme.Black)
@Preview(name = "Fader no icon", widthDp = 100, heightDp = 300, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun FaderNoIcon() = Item("fNoIcon", AppTheme.Black)

// Pastel themes: grid + key + slider (+ meter slider) per theme.
@Preview(name = "Grid Cream", widthDp = 760, heightDp = 360, backgroundColor = 0xFFF6F1EA, showBackground = true)
@Composable private fun GridCream() = Grid(AppTheme.Cream)
@Preview(name = "Key Cream", widthDp = 120, heightDp = 120, showBackground = true, backgroundColor = 0xFFF6F1EA)
@Composable private fun KeyCream() = Item("mute", AppTheme.Cream)
@Preview(name = "SliderV Cream", widthDp = 100, heightDp = 240, showBackground = true, backgroundColor = 0xFFF6F1EA)
@Composable private fun SliderVCream() = Item("mFaderV", AppTheme.Cream)

@Preview(name = "Grid Lavender", widthDp = 760, heightDp = 360, backgroundColor = 0xFFEFEDF7, showBackground = true)
@Composable private fun GridLavender() = Grid(AppTheme.Lavender)
@Preview(name = "Key Lavender", widthDp = 120, heightDp = 120, showBackground = true, backgroundColor = 0xFFEFEDF7)
@Composable private fun KeyLavender() = Item("mute", AppTheme.Lavender)
@Preview(name = "SliderV Lavender", widthDp = 100, heightDp = 240, showBackground = true, backgroundColor = 0xFFEFEDF7)
@Composable private fun SliderVLavender() = Item("mFaderV", AppTheme.Lavender)

@Preview(name = "Grid Mint", widthDp = 760, heightDp = 360, backgroundColor = 0xFFEDF5F1, showBackground = true)
@Composable private fun GridMint() = Grid(AppTheme.Mint)
@Preview(name = "Key Mint", widthDp = 120, heightDp = 120, showBackground = true, backgroundColor = 0xFFEDF5F1)
@Composable private fun KeyMint() = Item("mute", AppTheme.Mint)
@Preview(name = "SliderV Mint", widthDp = 100, heightDp = 240, showBackground = true, backgroundColor = 0xFFEDF5F1)
@Composable private fun SliderVMint() = Item("mFaderV", AppTheme.Mint)

@Preview(name = "Grid Dusk", widthDp = 760, heightDp = 360, backgroundColor = 0xFF1E1B26, showBackground = true)
@Composable private fun GridDusk() = Grid(AppTheme.Dusk)
@Preview(name = "Key Dusk", widthDp = 120, heightDp = 120, showBackground = true, backgroundColor = 0xFF1E1B26)
@Composable private fun KeyDusk() = Item("mute", AppTheme.Dusk)
@Preview(name = "SliderV Dusk", widthDp = 100, heightDp = 240, showBackground = true, backgroundColor = 0xFF1E1B26)
@Composable private fun SliderVDusk() = Item("mFaderV", AppTheme.Dusk)

@Preview(name = "Grid Tide", widthDp = 760, heightDp = 360, backgroundColor = 0xFF172027, showBackground = true)
@Composable private fun GridTide() = Grid(AppTheme.Tide)
@Preview(name = "Key Tide", widthDp = 120, heightDp = 120, showBackground = true, backgroundColor = 0xFF172027)
@Composable private fun KeyTide() = Item("mute", AppTheme.Tide)
@Preview(name = "SliderV Tide", widthDp = 100, heightDp = 240, showBackground = true, backgroundColor = 0xFF172027)
@Composable private fun SliderVTide() = Item("mFaderV", AppTheme.Tide)

// Round 24: icon classes on a dark and a light theme, titled and untitled, plus a tinted light face.
private fun iconKey(hash: String, title: String = "Mute", bg: Int = 0) =
    Control(id = "ik", kind = ControlKind.CONTROL_KIND_KEY, title = title, icon_hash = hash, bg_argb = bg, meter = true, level = 0.5f)

@Composable
private fun IconRow(theme: AppTheme, bg: Int = 0, title: String = "Mute") = Themed(theme) {
    androidx.compose.foundation.layout.Row(Modifier.fillMaxSize()) {
        for (h in listOf("glyphLight", "glyphDark", "cutout", "art")) {
            Box(Modifier.weight(1f).fillMaxSize()) {
                DeckControlItem(iconKey(h, title, bg), icons = previewIcons, onInput = { _, _, _ -> })
            }
        }
    }
}

@Preview(name = "Icons Black", widthDp = 480, heightDp = 120, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun IconsBlack() = IconRow(AppTheme.Black)
@Preview(name = "Icons Cream", widthDp = 480, heightDp = 120, showBackground = true, backgroundColor = 0xFFF6F1EA)
@Composable private fun IconsCream() = IconRow(AppTheme.Cream)
@Preview(name = "Icons Cream untitled", widthDp = 480, heightDp = 120, showBackground = true, backgroundColor = 0xFFF6F1EA)
@Composable private fun IconsCreamUntitled() = IconRow(AppTheme.Cream, title = "")
@Preview(name = "Icons Cream dark tint", widthDp = 480, heightDp = 120, showBackground = true, backgroundColor = 0xFFF6F1EA)
@Composable private fun IconsCreamDarkTint() = IconRow(AppTheme.Cream, bg = 0xFF7A1F16.toInt())
@Preview(name = "Icons Black light tint", widthDp = 480, heightDp = 120, showBackground = true, backgroundColor = 0xFF000000)
@Composable private fun IconsBlackLightTint() = IconRow(AppTheme.Black, bg = 0xFFF2E6B8.toInt())
