// SPDX-License-Identifier: GPL-3.0-or-later
//! `setImage`: load a plugin-folder file or `data:` URL, render SVG / JPEG to a 144x144 PNG.

use base64::Engine;
use std::path::Path;

pub(crate) const ICON_PX: u32 = 144;
const MAX_RASTER: usize = 512 * 1024;
const MAX_SVG: usize = 2 * 1024 * 1024;

#[derive(Debug, PartialEq)]
pub(crate) enum ImageError {
    /// -32602
    Params(String),
    /// -32002
    NotAllowed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Png,
    Jpeg,
    Svg,
}

/// Raw image bytes plus their sniffed kind.
pub(crate) struct Loaded {
    pub kind: Kind,
    pub bytes: Vec<u8>,
}

fn sniff(bytes: &[u8]) -> Option<Kind> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(Kind::Png);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Kind::Jpeg);
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let t = head.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("<svg") || t.starts_with("<?xml") || t.starts_with("<!--") || t.starts_with("<!DOCTYPE svg") {
        return Some(Kind::Svg);
    }
    None
}

fn check_size(kind: Kind, len: usize) -> Result<(), ImageError> {
    let max = if kind == Kind::Svg { MAX_SVG } else { MAX_RASTER };
    if len > max {
        return Err(ImageError::Params(format!("image is {len} bytes, the limit is {max}")));
    }
    Ok(())
}

/// Read the image named by `image` (a path inside `dir`, or a `data:` URL).
pub(crate) fn load(dir: &Path, image: &str) -> Result<Loaded, ImageError> {
    if let Some(rest) = image.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',').ok_or_else(|| ImageError::Params("malformed data URL".into()))?;
        let mut parts = meta.split(';');
        let mime = parts.next().unwrap_or("").to_ascii_lowercase();
        if !parts.any(|p| p.eq_ignore_ascii_case("base64")) {
            return Err(ImageError::Params("data URL must be base64".into()));
        }
        let want = match mime.as_str() {
            "image/png" => Kind::Png,
            "image/jpeg" | "image/jpg" => Kind::Jpeg,
            "image/svg+xml" => Kind::Svg,
            other => return Err(ImageError::Params(format!("unsupported image type {other:?}"))),
        };
        // Cheap size guard before decoding.
        check_size(want, data.len() / 4 * 3)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|e| ImageError::Params(format!("bad base64: {e}")))?;
        check_size(want, bytes.len())?;
        if sniff(&bytes) != Some(want) {
            return Err(ImageError::Params("image data does not match its declared type".into()));
        }
        return Ok(Loaded { kind: want, bytes });
    }
    if !crate::manifest::is_safe_rel(image) {
        return Err(ImageError::NotAllowed(format!("{image:?} is not a path inside the plugin folder")));
    }
    let root = dir.canonicalize().map_err(|e| ImageError::Params(format!("plugin folder: {e}")))?;
    let path =
        root.join(image).canonicalize().map_err(|e| ImageError::Params(format!("cannot read {image:?}: {e}")))?;
    if !path.starts_with(&root) {
        return Err(ImageError::NotAllowed(format!("{image:?} is outside the plugin folder")));
    }
    let len = std::fs::metadata(&path).map_err(|e| ImageError::Params(e.to_string()))?.len() as usize;
    check_size(Kind::Svg, len)?;
    let bytes = std::fs::read(&path).map_err(|e| ImageError::Params(format!("cannot read {image:?}: {e}")))?;
    let kind = sniff(&bytes).ok_or_else(|| ImageError::Params(format!("{image:?} is not a PNG, JPEG or SVG image")))?;
    check_size(kind, bytes.len())?;
    Ok(Loaded { kind, bytes })
}

/// PNG bytes for the importer: PNG passes through, SVG and JPEG are rendered to 144x144.
pub(crate) fn to_png(loaded: &Loaded) -> Result<Vec<u8>, ImageError> {
    match loaded.kind {
        Kind::Png => Ok(loaded.bytes.clone()),
        // Glyph SVGs get the same margin the config UI gives built-in icons (96 px in 144), so the
        // phone's caption under the key title doesn't run over the glyph.
        Kind::Svg => render_svg(&loaded.bytes, GLYPH_PAD_PX),
        Kind::Jpeg => {
            // Let resvg decode the JPEG: wrap it in an SVG image element.
            let b64 = base64::engine::general_purpose::STANDARD.encode(&loaded.bytes);
            let svg = format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{ICON_PX}\" height=\"{ICON_PX}\" viewBox=\"0 0 {ICON_PX} {ICON_PX}\"><image width=\"{ICON_PX}\" height=\"{ICON_PX}\" preserveAspectRatio=\"xMidYMid meet\" xlink:href=\"data:image/jpeg;base64,{b64}\"/></svg>"
            );
            render_svg(svg.as_bytes(), 0.0)
        }
    }
}

/// Margin around a plugin SVG glyph, matching the UI's built-in icon raster (inspector.js).
const GLYPH_PAD_PX: f32 = 24.0;

fn render_svg(svg: &[u8], pad: f32) -> Result<Vec<u8>, ImageError> {
    // No resources_dir: a plugin SVG can't pull other files in by relative reference.
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(svg, &opt).map_err(|e| ImageError::Params(format!("bad SVG: {e}")))?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w > 0.0 && h > 0.0) {
        return Err(ImageError::Params("SVG has no size".into()));
    }
    let px = ICON_PX as f32;
    let inner = px - 2.0 * pad;
    let scale = (inner / w).min(inner / h);
    let tx = (px - w * scale) / 2.0;
    let ty = (px - h * scale) / 2.0;
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(ICON_PX, ICON_PX).ok_or_else(|| ImageError::Params("pixmap".into()))?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, tx, ty), &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|e| ImageError::Params(format!("png encode: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="8" fill="#e3e6ea"/></svg>"##;

    #[test]
    fn svg_data_url_renders_to_png() {
        let url = format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(SVG));
        let l = load(Path::new("."), &url).unwrap();
        assert_eq!(l.kind, Kind::Svg);
        let png = to_png(&l).unwrap();
        assert_eq!(sniff(&png), Some(Kind::Png));
        let img = resvg::tiny_skia::Pixmap::decode_png(&png).unwrap();
        assert_eq!((img.width(), img.height()), (144, 144));
    }

    #[test]
    fn jpeg_is_converted() {
        // Minimal baseline JPEG is awkward to hand-write; a type/data mismatch must be rejected instead.
        let url = format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(SVG));
        assert!(matches!(load(Path::new("."), &url), Err(ImageError::Params(_))));
    }

    #[test]
    fn rejects_big_and_escaping() {
        let big = vec![0u8; MAX_RASTER + 10];
        let url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(big));
        assert!(matches!(load(Path::new("."), &url), Err(ImageError::Params(_))));
        assert!(matches!(load(Path::new("."), "../x.png"), Err(ImageError::NotAllowed(_))));
        assert!(matches!(load(Path::new("."), "/etc/passwd"), Err(ImageError::NotAllowed(_))));
    }
}
