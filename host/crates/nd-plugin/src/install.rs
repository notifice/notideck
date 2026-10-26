// SPDX-License-Identifier: GPL-3.0-or-later
//! Unpacking a plugin `.zip` safely.

use crate::host::HostError;
use crate::manifest::{is_safe_rel, Command, Entry, Manifest};
use std::io::{Cursor, Read, Write};
use std::path::Path;

pub(crate) const MAX_UNPACKED: u64 = 50 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;

fn invalid(s: impl Into<String>) -> HostError {
    HostError::Invalid(s.into())
}

/// Unpack `bytes` into the (existing, empty) folder `dest`, stripping a single top-level folder.
pub(crate) fn extract(bytes: &[u8], dest: &Path) -> Result<(), HostError> {
    let mut ar = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| invalid(format!("not a valid zip: {e}")))?;
    if ar.len() > MAX_ENTRIES {
        return Err(invalid("zip has too many entries"));
    }
    // Pass 1: names and declared sizes.
    let mut names: Vec<(String, bool)> = Vec::new();
    let mut declared: u64 = 0;
    for i in 0..ar.len() {
        let f = ar.by_index(i).map_err(|e| invalid(format!("bad zip entry: {e}")))?;
        let raw = f.name().map_err(|e| invalid(format!("bad zip entry name: {e}")))?.to_string();
        let trimmed = raw.trim_end_matches('/');
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("__MACOSX") {
            continue;
        }
        if !is_safe_rel(trimmed) {
            return Err(invalid(format!("zip entry {raw:?} escapes the plugin folder")));
        }
        if f.is_symlink() {
            return Err(invalid(format!("zip entry {raw:?} is a symbolic link")));
        }
        declared = declared.saturating_add(f.size());
        if declared > MAX_UNPACKED {
            return Err(invalid("zip unpacks to more than 50 MB"));
        }
        names.push((trimmed.replace('\\', "/"), f.is_dir()));
    }
    // Where is the manifest?
    let prefix = if names.iter().any(|(n, d)| n == "manifest.json" && !d) {
        String::new()
    } else {
        let tops: std::collections::BTreeSet<&str> = names.iter().filter_map(|(n, _)| n.split('/').next()).collect();
        let top = match tops.iter().collect::<Vec<_>>().as_slice() {
            [t] => (**t).to_string(),
            _ => return Err(invalid("zip must hold manifest.json at its root or inside a single top folder")),
        };
        let want = format!("{top}/manifest.json");
        if !names.iter().any(|(n, d)| *n == want && !d) {
            return Err(invalid("zip has no manifest.json at its root or in its top folder"));
        }
        format!("{top}/")
    };

    // Pass 2: extract with a hard cap on the real size.
    let mut total: u64 = 0;
    for i in 0..ar.len() {
        let mut f = ar.by_index(i).map_err(|e| invalid(format!("bad zip entry: {e}")))?;
        let raw = f.name().map_err(|e| invalid(format!("bad zip entry name: {e}")))?.replace('\\', "/");
        let trimmed = raw.trim_end_matches('/');
        if trimmed.is_empty() || trimmed.starts_with("__MACOSX") {
            continue;
        }
        let Some(rel) = trimmed.strip_prefix(prefix.as_str()).or(if prefix.is_empty() { Some(trimmed) } else { None })
        else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        let target = dest.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| HostError::Io(e.to_string()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| HostError::Io(e.to_string()))?;
        }
        let mut out = std::fs::File::create(&target).map_err(|e| HostError::Io(e.to_string()))?;
        let mut limited = (&mut f).take(MAX_UNPACKED + 1 - total);
        let n = std::io::copy(&mut limited, &mut out).map_err(|e| HostError::Io(e.to_string()))?;
        out.flush().map_err(|e| HostError::Io(e.to_string()))?;
        total += n;
        if total > MAX_UNPACKED {
            return Err(invalid("zip unpacks to more than 50 MB"));
        }
        #[cfg(unix)]
        if let Some(mode) = f.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode((mode & 0o777) | 0o600));
        }
    }
    Ok(())
}

fn command_program(c: &Command) -> Option<&str> {
    match c {
        Command::Path(p) => Some(p),
        Command::Argv(v) => v.first().map(String::as_str),
    }
}

/// After unpacking into `dir`: the manifest must have an entry for this OS and the entry's program must be in
/// the package (or on `PATH`). The error names the other OS's executable when the package is built for that.
pub(crate) fn check_entry(m: &Manifest, dir: &Path) -> Result<(), HostError> {
    let (this, other_name, other) = if cfg!(windows) {
        ("Windows", "Linux", if let Entry::PerOs { linux, .. } = &m.entry { linux.as_ref() } else { None })
    } else {
        ("Linux", "Windows", if let Entry::PerOs { windows, .. } = &m.entry { windows.as_ref() } else { None })
    };
    let other_prog = other.and_then(command_program).filter(|p| dir.join(p).is_file());
    let no_build = || match other_prog {
        Some(p) => invalid(format!("This plugin package has no {this} build (it contains {p} for {other_name}).")),
        None => invalid(format!("This plugin package has no {this} build.")),
    };
    let Some(argv) = m.command_for_os() else { return Err(no_build()) };
    let prog = argv.first().map(String::as_str).unwrap_or_default();
    if !crate::supervisor::program_exists(dir, prog) {
        return Err(match other_prog {
            Some(_) => no_build(),
            None => invalid(format!("This plugin package is missing its program ({prog}) for {this}.")),
        });
    }
    Ok(())
}
