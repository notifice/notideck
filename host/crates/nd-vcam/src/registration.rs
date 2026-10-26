// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform-independent part of installing the virtual camera DLL (ADR 0005 decision 6):
//! where the DLL goes, how an installed copy is compared with the shipped one, and the copy
//! strategy when the installed file is locked by the Frame Server. The registry is abstracted
//! behind [`Registry`] so all of this is unit-testable without touching HKLM.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

pub const DLL_NAME: &str = "notideck_vcam.dll";
/// Relative to `%ProgramFiles%`.
pub const INSTALL_SUBDIR: &str = "Notideck\\VirtualCamera";
/// Description (default value) of the CLSID key.
pub const CLSID_DESCRIPTION: &str = "Notideck Camera Source";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallState {
    /// No registration, or it points to a file that no longer exists.
    NotInstalled,
    /// Registered and identical to the DLL shipped with the app.
    Installed { path: PathBuf },
    /// Registered, but a different build than the shipped DLL.
    Outdated { path: PathBuf },
}

/// The COM registration (`HKLM\SOFTWARE\Classes\CLSID\{CLSID}\InprocServer32`).
pub trait Registry {
    fn read_server_path(&self) -> Option<PathBuf>;
    fn write_server_path(&self, dll: &Path) -> Result<()>;
    fn delete_registration(&self) -> Result<()>;
}

pub fn file_hash(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

pub fn hash8(hash: &[u8; 32]) -> String {
    hash[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// Is the registered DLL present and identical to `app_dll`?
pub fn compute_state(reg: &dyn Registry, app_dll: &Path) -> InstallState {
    let Some(path) = reg.read_server_path() else {
        return InstallState::NotInstalled;
    };
    if !path.is_file() {
        return InstallState::NotInstalled;
    }
    match (file_hash(&path), file_hash(app_dll)) {
        (Ok(a), Ok(b)) if a == b => InstallState::Installed { path },
        _ => InstallState::Outdated { path },
    }
}

fn is_ours(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == DLL_NAME || (n.starts_with("notideck_vcam-") && n.ends_with(".dll"))
}

/// Copy `dll_src` into `dir` and point the registration at it. Returns the registered path.
///
/// The canonical name is used when possible. If it is locked (loaded by the Frame Server) and
/// differs from the new build, the DLL is copied to `notideck_vcam-<hash8>.dll` instead.
/// Superseded files are removed best-effort.
pub fn install_into(dll_src: &Path, dir: &Path, reg: &dyn Registry) -> Result<PathBuf> {
    let src_hash = file_hash(dll_src).with_context(|| format!("reading {}", dll_src.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let canonical = dir.join(DLL_NAME);
    let same = |p: &Path| p.is_file() && file_hash(p).map(|h| h == src_hash).unwrap_or(false);

    let canonical_ok = same(&canonical) || (fs::copy(dll_src, &canonical).is_ok() && same(&canonical));
    let target = if canonical_ok {
        canonical
    } else {
        let versioned = dir.join(format!("notideck_vcam-{}.dll", hash8(&src_hash)));
        if !same(&versioned) {
            fs::copy(dll_src, &versioned).with_context(|| format!("copying to {}", versioned.display()))?;
            if !same(&versioned) {
                bail!("copy to {} is corrupt", versioned.display());
            }
        }
        versioned
    };

    reg.write_server_path(&target)?;
    cleanup_old(dir, &target);
    Ok(target)
}

fn cleanup_old(dir: &Path, keep: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p != keep && p.file_name().and_then(|n| n.to_str()).map(is_ours).unwrap_or(false) {
            let _ = fs::remove_file(&p); // locked ones stay until the next install
        }
    }
}

/// Remove the registration, then our files (and the directory if it ends up empty).
pub fn uninstall_from(dir: &Path, reg: &dyn Registry) -> Result<()> {
    let registered = reg.read_server_path();
    reg.delete_registration()?;
    let mut dirs = vec![dir.to_path_buf()];
    if let Some(p) = registered.as_deref().and_then(Path::parent) {
        if p != dir {
            dirs.push(p.to_path_buf());
        }
    }
    for d in dirs {
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.file_name().and_then(|n| n.to_str()).map(is_ours).unwrap_or(false) {
                    let _ = fs::remove_file(&p);
                }
            }
        }
        let _ = fs::remove_dir(&d); // only succeeds when empty
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeReg(RefCell<Option<PathBuf>>);
    impl Registry for FakeReg {
        fn read_server_path(&self) -> Option<PathBuf> {
            self.0.borrow().clone()
        }
        fn write_server_path(&self, dll: &Path) -> Result<()> {
            *self.0.borrow_mut() = Some(dll.to_path_buf());
            Ok(())
        }
        fn delete_registration(&self) -> Result<()> {
            *self.0.borrow_mut() = None;
            Ok(())
        }
    }

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("nd-vcam-test-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            if let Ok(rd) = fs::read_dir(&self.0) {
                for e in rd.flatten() {
                    let mut perm = e.metadata().unwrap().permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perm.set_readonly(false);
                    let _ = fs::set_permissions(e.path(), perm);
                }
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn state_transitions() {
        let t = Tmp::new("state");
        let app = t.0.join("app.dll");
        fs::write(&app, b"v1").unwrap();
        let reg = FakeReg::default();
        assert_eq!(compute_state(&reg, &app), InstallState::NotInstalled);

        let dir = t.0.join("pf");
        let p = install_into(&app, &dir, &reg).unwrap();
        assert_eq!(p, dir.join(DLL_NAME));
        assert_eq!(compute_state(&reg, &app), InstallState::Installed { path: p.clone() });

        fs::write(&app, b"v2 newer").unwrap();
        assert_eq!(compute_state(&reg, &app), InstallState::Outdated { path: p.clone() });

        // Registered file removed behind our back.
        fs::remove_file(&p).unwrap();
        assert_eq!(compute_state(&reg, &app), InstallState::NotInstalled);
    }

    #[test]
    fn update_replaces_and_locked_uses_versioned_name() {
        let t = Tmp::new("locked");
        let app = t.0.join("app.dll");
        let dir = t.0.join("pf");
        let reg = FakeReg::default();
        fs::write(&app, b"build one").unwrap();
        let p1 = install_into(&app, &dir, &reg).unwrap();
        assert_eq!(p1, dir.join(DLL_NAME));

        // Idempotent.
        assert_eq!(install_into(&app, &dir, &reg).unwrap(), p1);

        // Update while unlocked: replaced in place.
        fs::write(&app, b"build two").unwrap();
        let p2 = install_into(&app, &dir, &reg).unwrap();
        assert_eq!(p2, p1);
        assert_eq!(fs::read(&p2).unwrap(), b"build two");

        // Simulate a lock (read-only stands in for ERROR_USER_MAPPED_FILE).
        let mut perm = fs::metadata(&p2).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&p2, perm).unwrap();
        fs::write(&app, b"build three").unwrap();
        let p3 = install_into(&app, &dir, &reg).unwrap();
        assert_ne!(p3, p2);
        let name = p3.file_name().unwrap().to_str().unwrap().to_string();
        assert!(name.starts_with("notideck_vcam-") && name.ends_with(".dll"), "{name}");
        assert_eq!(reg.read_server_path(), Some(p3.clone()));
        assert_eq!(compute_state(&reg, &app), InstallState::Installed { path: p3.clone() });
        assert_eq!(fs::read(&p3).unwrap(), b"build three");
    }

    #[test]
    fn uninstall_removes_registration_and_files() {
        let t = Tmp::new("uninst");
        let app = t.0.join("app.dll");
        fs::write(&app, b"x").unwrap();
        let dir = t.0.join("pf");
        let reg = FakeReg::default();
        install_into(&app, &dir, &reg).unwrap();
        fs::write(dir.join("notideck_vcam-deadbeef.dll"), b"old").unwrap();
        fs::write(dir.join("unrelated.txt"), b"keep").unwrap();
        uninstall_from(&dir, &reg).unwrap();
        assert_eq!(reg.read_server_path(), None);
        assert!(!dir.join(DLL_NAME).exists());
        assert!(!dir.join("notideck_vcam-deadbeef.dll").exists());
        assert!(dir.join("unrelated.txt").exists());
    }

    #[test]
    fn hash8_is_hex() {
        let h = [0xab; 32];
        assert_eq!(hash8(&h), "abababab");
    }
}
