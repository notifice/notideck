// SPDX-License-Identifier: GPL-3.0-or-later
//! Content-addressed icon store (SPEC §9: `icons/<sha256>.<ext>`).

use crate::error::{CoreError, Result};
use nd_proto::v1::PutIcon;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Maximum accepted icon size in bytes.
pub const MAX_ICON_BYTES: usize = 2 * 1024 * 1024;

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Compute the lowercase hex sha256 of `data`.
pub fn hash_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Sniff the MIME type of an image; only PNG and WebP are accepted.
pub fn sniff_mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(PNG_MAGIC) {
        Some("image/png")
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn ext_for_mime(mime: &str) -> &'static str {
    if mime == "image/webp" {
        "webp"
    } else {
        "png"
    }
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Content-addressed icon directory.
#[derive(Debug, Clone)]
pub struct IconStore {
    dir: PathBuf,
}

impl IconStore {
    /// Create a store rooted at `dir` (created lazily on first import).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Import image bytes, returning their sha256 hash. Idempotent.
    pub fn import(&self, data: &[u8]) -> Result<String> {
        if data.is_empty() || data.len() > MAX_ICON_BYTES {
            return Err(CoreError::Profile(format!("icon size {} out of range (1..={MAX_ICON_BYTES})", data.len())));
        }
        let mime = sniff_mime(data).ok_or_else(|| CoreError::Profile("icon must be PNG or WebP".to_string()))?;
        let hash = hash_bytes(data);
        let path = self.dir.join(format!("{hash}.{}", ext_for_mime(mime)));
        if !path.exists() {
            fs::create_dir_all(&self.dir)?;
            let tmp = self.dir.join(format!(".{hash}.tmp"));
            fs::write(&tmp, data)?;
            fs::rename(&tmp, &path)?;
        }
        Ok(hash)
    }

    /// Look up an icon by hash, returning `(mime, bytes)`.
    pub fn get(&self, hash: &str) -> Option<(String, Vec<u8>)> {
        if !valid_hash(hash) {
            return None;
        }
        for (ext, mime) in [("png", "image/png"), ("webp", "image/webp")] {
            let path = self.dir.join(format!("{hash}.{ext}"));
            if let Ok(data) = fs::read(&path) {
                return Some((mime.to_string(), data));
            }
        }
        None
    }

    pub fn contains(&self, hash: &str) -> bool {
        valid_hash(hash) && ["png", "webp"].iter().any(|e| self.dir.join(format!("{hash}.{e}")).exists())
    }

    /// Build a `PutIcon` message for `hash`, if stored.
    pub fn put_icon(&self, hash: &str) -> Option<PutIcon> {
        self.get(hash).map(|(mime_type, data)| PutIcon { hash: hash.to_string(), mime_type, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let mut v = PNG_MAGIC.to_vec();
        v.extend_from_slice(b"fake-body");
        v
    }

    #[test]
    fn hash_is_known_sha256() {
        assert_eq!(hash_bytes(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff_mime(&png()), Some("image/png"));
        assert_eq!(sniff_mime(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_mime(b"GIF89a"), None);
    }

    #[test]
    fn import_and_lookup_roundtrip() {
        let dir = std::env::temp_dir().join(format!("nd-icons-{}", std::process::id()));
        let store = IconStore::new(dir.join("icons"));
        let data = png();
        let hash = store.import(&data).unwrap();
        assert_eq!(hash, hash_bytes(&data));
        assert_eq!(store.import(&data).unwrap(), hash);
        assert!(store.contains(&hash));
        let msg = store.put_icon(&hash).unwrap();
        assert_eq!(msg.mime_type, "image/png");
        assert_eq!(msg.data, data);
        assert!(store.get("../etc/passwd").is_none());
        assert!(store.import(b"not an image").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
