// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding plugin folders and the persisted enable/approval state (`plugins.json`).

use crate::manifest::{Manifest, Permission};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// One plugin folder found on disk.
#[derive(Debug, Clone)]
pub(crate) struct Discovered {
    pub id: String,
    pub dir: PathBuf,
    pub bundled: bool,
    pub manifest: Result<Manifest, String>,
}

fn scan_dir(root: &Path, bundled: bool, out: &mut Vec<Discovered>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        let mf = dir.join("manifest.json");
        if !mf.is_file() {
            continue;
        }
        let parsed = std::fs::read_to_string(&mf)
            .map_err(|e| format!("cannot read manifest.json: {e}"))
            .and_then(|s| Manifest::parse(&s).map_err(|e| e.to_string()));
        let id = parsed.as_ref().map(|m| m.id.clone()).unwrap_or_else(|_| name.clone());
        out.push(Discovered { id, dir, bundled, manifest: parsed });
    }
}

/// Scan the bundled roots in order, then the user root. The first bundled plugin with an id wins among bundled
/// ones; a valid user plugin overrides any bundled plugin with the same id. An invalid folder never replaces a
/// valid plugin.
pub(crate) fn discover(bundled_dirs: &[PathBuf], user_dir: &Path) -> BTreeMap<String, Discovered> {
    let mut found = Vec::new();
    for d in bundled_dirs {
        scan_dir(d, true, &mut found);
    }
    let bundled_count = found.len();
    scan_dir(user_dir, false, &mut found);
    let mut map: BTreeMap<String, Discovered> = BTreeMap::new();
    for (i, d) in found.into_iter().enumerate() {
        let is_user = i >= bundled_count;
        match map.get(&d.id) {
            None => {
                map.insert(d.id.clone(), d);
            }
            Some(old) => {
                let replace = d.manifest.is_ok() && (is_user || old.manifest.is_err());
                if replace {
                    map.insert(d.id.clone(), d);
                }
            }
        }
    }
    map
}

/// `plugins.json`: `{"disabled": [ids], "approved": {id: [perms]}}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Persisted {
    #[serde(default)]
    pub disabled: BTreeSet<String>,
    #[serde(default)]
    pub approved: BTreeMap<String, Vec<Permission>>,
}

impl Persisted {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!(target: "plugin", error = %e, "plugins.json is invalid; starting with defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Atomic write: temp file in the same folder, then rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }

    /// Bundled plugins never need approval; a user plugin needs one unless its permissions are a subset of the
    /// approved set (an empty list approved once counts).
    pub fn needs_approval(&self, id: &str, bundled: bool, perms: &[Permission]) -> bool {
        if bundled {
            return false;
        }
        match self.approved.get(id) {
            None => true,
            Some(ok) => {
                let ok: HashSet<_> = ok.iter().copied().collect();
                !perms.iter().all(|p| ok.contains(p))
            }
        }
    }
}
