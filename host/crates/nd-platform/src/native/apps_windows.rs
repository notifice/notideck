// SPDX-License-Identifier: GPL-3.0-or-later
//! Installed apps: Shell AppsFolder (FOLDERID_AppsFolder): Win32 shortcuts and Store apps.
//!
//! Every call runs on a short-lived worker thread with its own STA apartment, so callers on
//! any thread work and a stuck shell cannot hang the caller (timeouts below).

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, IShellItemImageFactory,
    SHCreateItemFromParsingName, SHGetKnownFolderItem, KF_FLAG_DEFAULT, SIGDN, SIGDN_NORMALDISPLAY,
    SIGDN_PARENTRELATIVEPARSING,
};

use crate::{AppCatalog, InstalledApp, PlatformError, Result};

const LIST_TIMEOUT: Duration = Duration::from_secs(10);
const ICON_TIMEOUT: Duration = Duration::from_secs(3);
const TARGET_PREFIX: &str = "shell:AppsFolder\\";

#[derive(Debug, Default)]
pub struct AppsFolder;

impl AppsFolder {
    pub fn new() -> Self {
        Self
    }
}

struct ComGuard;
impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Run `f` on a fresh STA thread, waiting at most `timeout`.
fn run_sta<T: Send + 'static>(
    what: &'static str,
    timeout: Duration,
    f: impl FnOnce() -> std::result::Result<T, String> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("nd-apps-{what}"))
        .spawn(move || {
            let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            if hr.is_err() {
                let _ = tx.send(Err(format!("CoInitializeEx: {hr:?}")));
                return;
            }
            let _guard = ComGuard;
            let _ = tx.send(f());
        })
        .map_err(|e| PlatformError::Other(format!("apps {what}: spawn: {e}")))?;
    match rx.recv_timeout(timeout) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(PlatformError::Other(format!("apps {what}: {e}"))),
        Err(_) => Err(PlatformError::Other(format!("apps {what}: timed out"))),
    }
}

fn display_name(item: &IShellItem, kind: SIGDN) -> Option<String> {
    unsafe {
        let p: PWSTR = item.GetDisplayName(kind).ok()?;
        if p.is_null() {
            return None;
        }
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

fn skip(name: &str, id: &str) -> bool {
    let lname = name.to_lowercase();
    let lid = id.to_lowercase();
    if lname.contains("uninstall") || lid.contains("uninstall") {
        return true;
    }
    if lid.starts_with("http") {
        return true;
    }
    [".url", ".chm", ".txt", ".pdf", ".html", ".htm"].iter().any(|e| lid.ends_with(e))
}

fn enumerate() -> std::result::Result<Vec<InstalledApp>, String> {
    unsafe {
        let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, HANDLE::default())
            .map_err(|e| format!("SHGetKnownFolderItem: {e}"))?;
        let en: IEnumShellItems =
            folder.BindToHandler(None, &BHID_EnumItems).map_err(|e| format!("BindToHandler: {e}"))?;
        let mut apps: Vec<InstalledApp> = Vec::new();
        loop {
            let mut batch: [Option<IShellItem>; 16] = Default::default();
            let mut got = 0u32;
            let hr = en.Next(&mut batch, Some(&mut got));
            if hr.is_err() {
                return Err(format!("IEnumShellItems::Next: {hr:?}"));
            }
            if got == 0 {
                break;
            }
            for item in batch.iter().take(got as usize).flatten() {
                let (Some(name), Some(id)) =
                    (display_name(item, SIGDN_NORMALDISPLAY), display_name(item, SIGDN_PARENTRELATIVEPARSING))
                else {
                    continue;
                };
                let (name, id) = (name.trim().to_string(), id.trim().to_string());
                if name.is_empty() || id.is_empty() || skip(&name, &id) {
                    continue;
                }
                apps.push(InstalledApp { target: format!("{TARGET_PREFIX}{id}"), id, name, args: Vec::new() });
            }
        }
        apps.sort_by_key(|a| (a.name.to_lowercase(), a.id.clone()));
        let mut seen = std::collections::HashSet::new();
        apps.retain(|a| seen.insert(a.id.clone()));
        Ok(apps)
    }
}

fn icon_png(id: &str) -> std::result::Result<Option<Vec<u8>>, String> {
    let path: Vec<u16> = format!("{TARGET_PREFIX}{id}").encode_utf16().chain(Some(0)).collect();
    let f: IShellItemImageFactory = unsafe { SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None) }
        .map_err(|e| format!("SHCreateItemFromParsingName: {e}"))?;
    super::win_icon::factory_icon_png(&f)
}

impl AppCatalog for AppsFolder {
    fn backend_name(&self) -> &'static str {
        "appsfolder"
    }

    fn list(&self) -> Result<Vec<InstalledApp>> {
        run_sta("list", LIST_TIMEOUT, enumerate)
    }

    fn icon(&self, app: &InstalledApp) -> Result<Option<Vec<u8>>> {
        if app.id.is_empty() {
            return Ok(None);
        }
        let id = app.id.clone();
        run_sta("icon", ICON_TIMEOUT, move || icon_png(&id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    #[test]
    fn list_and_icons() {
        let cat = AppsFolder::new();
        let t = std::time::Instant::now();
        let apps = cat.list().expect("list");
        eprintln!("appsfolder: {} apps in {:?}", apps.len(), t.elapsed());
        assert!(apps.len() > 5, "only {} apps", apps.len());
        let mut ids = std::collections::HashSet::new();
        for a in &apps {
            assert!(ids.insert(a.id.clone()), "dup id {}", a.id);
            assert!(a.target.starts_with("shell:AppsFolder\\"), "{}", a.target);
            assert!(!a.name.to_lowercase().contains("uninstall"), "{}", a.name);
            assert!(a.args.is_empty());
        }
        assert!(apps.windows(2).all(|w| w[0].name.to_lowercase() <= w[1].name.to_lowercase()));
        eprintln!("store apps with '!': {}", apps.iter().filter(|a| a.id.contains('!')).count());
        for a in apps.iter().take(5) {
            eprintln!("  {} | {}", a.name, a.id);
        }
        let t = std::time::Instant::now();
        let ok = apps.iter().take(6).filter(|a| matches!(cat.icon(a), Ok(Some(p)) if p.starts_with(&PNG_SIG))).count();
        eprintln!("icons ok: {ok}/6 in {:?}", t.elapsed());
        assert!(ok >= 1);
    }
}
