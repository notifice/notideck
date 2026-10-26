// SPDX-License-Identifier: GPL-3.0-or-later
//! Image files dropped onto the Linux window. WebKitGTK hands a file dragged from a file manager
//! to the page as `text/uri-list` without `File` objects, so the page can't read it. wry's
//! drag-drop handler gives us the paths instead: we read the first image here and pass its bytes
//! to the page (`window.__ndNativeDrop`), which uploads it like a dropped file.

use base64::Engine;
use std::path::{Path, PathBuf};

/// Larger files are refused (the page downscales anything it gets to a small PNG anyway).
pub const MAX_DROP_BYTES: u64 = 16 * 1024 * 1024;

fn mime_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        _ => return None,
    })
}

/// The script for a drop at `position` (CSS px in the webview), or `None` when no dropped path
/// looks like an image (the drop is then left to WebKit, which the navigation handler refuses).
pub fn drop_script(paths: &[PathBuf], position: (i32, i32)) -> Option<String> {
    let (path, mime) = paths.iter().find_map(|p| mime_of(p).map(|m| (p, m)))?;
    let (x, y) = position;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let read = std::fs::metadata(path).map_err(|e| e.to_string()).and_then(|m| {
        if m.len() > MAX_DROP_BYTES {
            Err(format!("{name} is larger than {} MiB.", MAX_DROP_BYTES / (1024 * 1024)))
        } else {
            std::fs::read(path).map_err(|e| e.to_string())
        }
    });
    let payload = match read {
        Ok(bytes) => serde_json::json!({
            "x": x, "y": y, "name": name, "mime": mime,
            "b64": base64::engine::general_purpose::STANDARD.encode(bytes),
        }),
        Err(e) => serde_json::json!({ "x": x, "y": y, "error": format!("Could not read {name}: {e}") }),
    };
    Some(format!("window.__ndNativeDrop && window.__ndNativeDrop({payload});"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_images_are_left_to_webkit() {
        assert!(drop_script(&[PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b")], (1, 2)).is_none());
        assert!(drop_script(&[], (1, 2)).is_none());
    }

    #[test]
    fn first_image_is_sent_with_position_and_mime() {
        let dir = std::env::temp_dir().join(format!("nd-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svg = dir.join("Logo.SVG");
        std::fs::write(&svg, b"<svg/>").unwrap();
        let s = drop_script(&[dir.join("notes.txt"), svg], (10, 20)).unwrap();
        assert!(s.starts_with("window.__ndNativeDrop && window.__ndNativeDrop({"));
        let json: serde_json::Value =
            serde_json::from_str(&s[s.find('{').unwrap()..s.rfind('}').unwrap() + 1]).unwrap();
        assert_eq!(json["x"], 10);
        assert_eq!(json["y"], 20);
        assert_eq!(json["name"], "Logo.SVG");
        assert_eq!(json["mime"], "image/svg+xml");
        assert_eq!(json["b64"], "PHN2Zy8+");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unreadable_image_reports_an_error() {
        let s = drop_script(&[PathBuf::from("/nonexistent/x.png")], (0, 0)).unwrap();
        assert!(s.contains("\"error\":\"Could not read x.png"));
    }
}
