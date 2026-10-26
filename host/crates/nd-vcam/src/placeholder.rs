// SPDX-License-Identifier: GPL-3.0-or-later
//! The "phone not connected" frame, rendered in code (NV12), cached per size.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::shm::nv12_len;

pub const TEXT: &str = "Notideck Camera - phone not connected";
/// Dark graphite background (luma); chroma is neutral.
pub const BG_Y: u8 = 0x2A;
pub const NEUTRAL_UV: u8 = 0x80;
const TEXT_Y: u8 = 0xD0;
const PHONE_Y: u8 = 0x78;
const SCREEN_Y: u8 = 0x38;

/// 5x8 bitmap font (bit 4 = leftmost column). Rows 0..=6 are the cell, row 7 holds descenders.
fn glyph(c: char) -> [u8; 8] {
    match c {
        ' ' => [0; 8],
        '-' => [0, 0, 0, 0, 0b01110, 0, 0, 0],
        'N' => [0b10001, 0b11001, 0b10101, 0b10101, 0b10011, 0b10001, 0b10001, 0],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110, 0],
        'a' => [0, 0, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111, 0],
        'c' => [0, 0, 0b01110, 0b10001, 0b10000, 0b10001, 0b01110, 0],
        'd' => [0b00001, 0b00001, 0b01111, 0b10001, 0b10001, 0b10001, 0b01111, 0],
        'e' => [0, 0, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110, 0],
        'h' => [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001, 0],
        'i' => [0b00100, 0, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110, 0],
        'k' => [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0],
        'm' => [0, 0, 0b11010, 0b10101, 0b10101, 0b10101, 0b10101, 0],
        'n' => [0, 0, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001, 0],
        'o' => [0, 0, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110, 0],
        'p' => [0, 0, 0b11110, 0b10001, 0b10001, 0b10001, 0b11110, 0b10000],
        'r' => [0, 0, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000, 0],
        't' => [0, 0b00100, 0b11110, 0b00100, 0b00100, 0b00100, 0b00011, 0],
        _ => [0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111, 0],
    }
}

struct Canvas<'a> {
    y: &'a mut [u8],
    w: usize,
    h: usize,
}

impl Canvas<'_> {
    fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, v: u8) {
        for row in y..(y + h).min(self.h) {
            let start = row * self.w + x.min(self.w);
            let end = row * self.w + (x + w).min(self.w);
            self.y[start..end].fill(v);
        }
    }
}

fn draw_text(c: &mut Canvas, x: usize, y: usize, scale: usize, text: &str, v: u8) {
    for (i, ch) in text.chars().enumerate() {
        let g = glyph(ch);
        let gx = x + i * 6 * scale;
        for (row, bits) in g.iter().enumerate() {
            for col in 0..5 {
                if bits & (0b10000 >> col) != 0 {
                    c.rect(gx + col * scale, y + row * scale, scale, scale, v);
                }
            }
        }
    }
}

/// Render the placeholder for `width`×`height` (NV12, tightly packed).
pub fn render(width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut buf = vec![NEUTRAL_UV; nv12_len(width, height)];
    buf[..w * h].fill(BG_Y);
    let mut c = Canvas { y: &mut buf[..w * h], w, h };

    // Phone glyph: outlined rounded-ish rectangle with a screen and a speaker slot.
    let ph = (h * 3 / 10).max(8);
    let pw = (ph * 55 / 100).max(5);
    let border = (ph / 14).max(1);
    let px = w.saturating_sub(pw) / 2;
    let py = h * 18 / 100;
    c.rect(px, py, pw, ph, PHONE_Y);
    c.rect(px + border, py + border * 3, pw.saturating_sub(border * 2), ph.saturating_sub(border * 6), SCREEN_Y);
    c.rect(px + pw / 2 - pw / 6, py + border, pw / 3, border.max(1), SCREEN_Y);
    // Round the outer corners by clipping one border-sized square back to the background.
    for (cx, cy) in [(px, py), (px + pw - border, py), (px, py + ph - border), (px + pw - border, py + ph - border)] {
        c.rect(cx, cy, border, border, BG_Y);
    }

    // Text, as large as fits in ~80% of the width.
    let cells = TEXT.chars().count();
    let scale = ((w * 8 / 10) / (cells * 6)).max(1);
    let tw = cells * 6 * scale - scale;
    let tx = w.saturating_sub(tw) / 2;
    let ty = py + ph + h * 8 / 100;
    draw_text(&mut c, tx, ty, scale, TEXT, TEXT_Y);
    buf
}

type Cache = Mutex<HashMap<(u32, u32), Arc<Vec<u8>>>>;

/// Cached [`render`].
pub fn cached(width: u32, height: u32) -> Arc<Vec<u8>> {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut map = cache.lock().unwrap_or_else(|e| e.into_inner());
    map.entry((width, height)).or_insert_with(|| Arc::new(render(width, height))).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm::MODES;

    #[test]
    fn all_modes_render() {
        for (w, h) in MODES {
            let f = render(w, h);
            assert_eq!(f.len(), nv12_len(w, h));
            assert_eq!(f[0], BG_Y);
            assert!(f[..(w * h) as usize].contains(&TEXT_Y), "no text at {w}x{h}");
            assert!(f[..(w * h) as usize].contains(&PHONE_Y), "no phone at {w}x{h}");
            assert!(f[(w * h) as usize..].iter().all(|&b| b == NEUTRAL_UV));
        }
    }

    #[test]
    fn text_fits_width() {
        for (w, h) in MODES {
            let f = render(w, h);
            // The last column must be background everywhere (text is centred within 80%).
            for row in 0..h as usize {
                assert_eq!(f[row * w as usize + w as usize - 1], BG_Y);
            }
        }
    }

    #[test]
    fn cache_returns_same_allocation() {
        assert!(Arc::ptr_eq(&cached(640, 360), &cached(640, 360)));
    }
}
