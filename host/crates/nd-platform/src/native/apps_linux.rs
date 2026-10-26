// SPDX-License-Identifier: GPL-3.0-or-later
//! Installed apps: XDG desktop entries from $XDG_DATA_HOME/applications and $XDG_DATA_DIRS/*/applications.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::{AppCatalog, InstalledApp, PlatformError, Result};

use super::icon_theme::IconLookup;

#[derive(Debug, Default)]
pub struct DesktopFiles {
    /// desktop file id -> `Icon=` value, filled by `list`.
    icons: Mutex<HashMap<String, String>>,
    /// Icon-theme index parsed by `list` (or lazily by `icon`).
    lookup: Mutex<Option<std::sync::Arc<IconLookup>>>,
}

impl DesktopFiles {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A parsed `[Desktop Entry]` that is launchable and displayable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub name: String,
    pub exec: String,
    pub try_exec: Option<String>,
    pub icon: Option<String>,
}

/// Undo the desktop-entry string escapes (`\s \n \t \r \\`).
fn unescape_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Parse the `[Desktop Entry]` group. `None` if not a displayable application.
pub(crate) fn parse_desktop_entry(text: &str) -> Option<Entry> {
    let mut in_group = false;
    let (mut typ, mut name, mut exec, mut try_exec, mut icon) = (None, None, None, None, None);
    let (mut no_display, mut hidden) = (false, false);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "Type" => typ = Some(v.to_string()),
            "Name" => name = Some(unescape_value(v)),
            "Exec" => exec = Some(unescape_value(v)),
            "TryExec" => try_exec = Some(unescape_value(v)),
            "Icon" => icon = Some(unescape_value(v)),
            "NoDisplay" => no_display = v == "true",
            "Hidden" => hidden = v == "true",
            _ => {}
        }
    }
    if typ.as_deref() != Some("Application") || no_display || hidden {
        return None;
    }
    let name = name.filter(|n| !n.is_empty())?;
    let exec = exec.filter(|e| !e.trim().is_empty())?;
    Some(Entry { name, exec, try_exec: try_exec.filter(|t| !t.is_empty()), icon: icon.filter(|i| !i.is_empty()) })
}

/// Split an (already string-unescaped) `Exec` value into words, honoring double
/// quotes, and strip field codes. `None` if the program is empty or quoting is broken.
pub(crate) fn split_exec(exec: &str) -> Option<Vec<String>> {
    let mut words: Vec<(String, bool)> = Vec::new(); // (text, had_quotes)
    let mut cur = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut in_q = false;
    let mut it = exec.chars();
    while let Some(c) = it.next() {
        if in_q {
            match c {
                '"' => in_q = false,
                '\\' => match it.next() {
                    Some(e @ ('"' | '`' | '$' | '\\')) => cur.push(e),
                    Some(o) => {
                        cur.push('\\');
                        cur.push(o);
                    }
                    None => return None,
                },
                _ => cur.push(c),
            }
        } else if c == '"' {
            in_q = true;
            started = true;
            quoted = true;
        } else if c.is_whitespace() {
            if started {
                words.push((std::mem::take(&mut cur), quoted));
                started = false;
                quoted = false;
            }
        } else {
            cur.push(c);
            started = true;
        }
    }
    if in_q {
        return None;
    }
    if started {
        words.push((cur, quoted));
    }
    let mut out = Vec::new();
    for (w, was_quoted) in words {
        let mut s = String::new();
        let mut had_code = false;
        let mut it = w.chars();
        while let Some(c) = it.next() {
            if c != '%' {
                s.push(c);
                continue;
            }
            match it.next() {
                Some('%') => s.push('%'),
                Some('f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'i' | 'c' | 'k' | 'v' | 'm') => had_code = true,
                Some(o) => {
                    s.push('%');
                    s.push(o);
                }
                None => s.push('%'),
            }
        }
        if s.is_empty() && (had_code || !was_quoted) {
            continue;
        }
        out.push(s);
    }
    if out.first().is_none_or(|p| p.is_empty()) {
        return None;
    }
    Some(out)
}

fn on_path(program: &str) -> bool {
    let p = Path::new(program);
    if p.is_absolute() {
        return p.exists();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(program).exists()))
        .unwrap_or(false)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>, depth: usize) {
    if depth > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(root, &path, out, depth + 1);
        } else if path.extension().is_some_and(|e| e == "desktop") {
            if let Ok(rel) = path.strip_prefix(root) {
                let id = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("-");
                out.push((id, path));
            }
        }
    }
}

/// Scan `dirs` in precedence order. Returns apps sorted by name plus id -> icon value.
pub(crate) fn scan_dirs(dirs: &[PathBuf]) -> (Vec<InstalledApp>, HashMap<String, String>) {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    let mut icons = HashMap::new();
    for dir in dirs {
        let mut files = Vec::new();
        collect(dir, dir, &mut files, 0);
        for (id, path) in files {
            // An earlier dir wins even if its entry is hidden (that's how overrides hide apps).
            if !seen.insert(id.clone()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(entry) = parse_desktop_entry(&text) else { continue };
            if let Some(t) = &entry.try_exec {
                if !on_path(t) {
                    continue;
                }
            }
            let Some(mut words) = split_exec(&entry.exec) else { continue };
            let target = words.remove(0);
            if let Some(icon) = entry.icon {
                icons.insert(id.clone(), icon);
            }
            apps.push(InstalledApp { id, name: entry.name, target, args: words });
        }
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    (apps, icons)
}

fn default_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    if let Some(d) = data_home {
        dirs.push(d.join("applications"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    dirs.extend(data_dirs.split(':').filter(|s| !s.is_empty()).map(|d| Path::new(d).join("applications")));
    if let Some(h) = &home {
        dirs.push(h.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    dirs.push(PathBuf::from("/var/lib/snapd/desktop/applications"));
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

impl AppCatalog for DesktopFiles {
    fn backend_name(&self) -> &'static str {
        "desktop-files"
    }
    fn list(&self) -> Result<Vec<InstalledApp>> {
        let (apps, icons) = scan_dirs(&default_dirs());
        *self.icons.lock().map_err(|_| PlatformError::Unsupported("app catalog lock poisoned"))? = icons;
        *self.lookup.lock().map_err(|_| PlatformError::Unsupported("app catalog lock poisoned"))? =
            Some(std::sync::Arc::new(IconLookup::system()));
        Ok(apps)
    }
    fn icon(&self, app: &InstalledApp) -> Result<Option<Vec<u8>>> {
        let path = self.icons.lock().ok().and_then(|m| m.get(&app.id).cloned());
        let Some(p) = path else { return Ok(None) };
        let lookup = {
            let mut g = self.lookup.lock().map_err(|_| PlatformError::Unsupported("app catalog lock poisoned"))?;
            g.get_or_insert_with(|| std::sync::Arc::new(IconLookup::system())).clone()
        };
        Ok(lookup.lookup(&p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exec_quoting_and_codes() {
        assert_eq!(split_exec(r#""/opt/My App/app" --flag %U"#), Some(sv(&["/opt/My App/app", "--flag"])));
        assert_eq!(split_exec("env FOO=1 code --new-window %F"), Some(sv(&["env", "FOO=1", "code", "--new-window"])));
        assert_eq!(split_exec(r#"sh -c "echo \"hi\"""#), Some(sv(&["sh", "-c", r#"echo "hi""#])));
        assert_eq!(split_exec("app 100%% --x=%u"), Some(sv(&["app", "100%", "--x="])));
        assert_eq!(split_exec("%U"), None);
        assert_eq!(split_exec(r#"app "unterminated"#), None);
    }

    #[test]
    fn exec_double_escaped_in_file() {
        let e = parse_desktop_entry("[Desktop Entry]\nType=Application\nName=X\nExec=sh -c \"echo \\\\\"hi\\\\\"\"\n")
            .unwrap();
        assert_eq!(split_exec(&e.exec), Some(sv(&["sh", "-c", r#"echo "hi""#])));
    }

    #[test]
    fn entry_filtering() {
        let ok = "[Desktop Entry]\nType=Application\nName=A\nName[de]=Ä\nExec=a %U\nIcon=/x.png\n";
        let e = parse_desktop_entry(ok).unwrap();
        assert_eq!((e.name.as_str(), e.exec.as_str(), e.icon.as_deref()), ("A", "a %U", Some("/x.png")));
        assert!(parse_desktop_entry("[Desktop Entry]\nType=Application\nName=A\nExec=a\nNoDisplay=true\n").is_none());
        assert!(parse_desktop_entry("[Desktop Entry]\nType=Application\nName=A\nExec=a\nHidden=true\n").is_none());
        assert!(parse_desktop_entry("[Desktop Entry]\nType=Link\nName=A\nExec=a\n").is_none());
        assert!(parse_desktop_entry("[Desktop Entry]\nType=Application\nName=A\n").is_none());
        // Only localized name: no plain Name -> skipped.
        assert!(parse_desktop_entry("[Desktop Entry]\nType=Application\nName[de]=Ä\nExec=a\n").is_none());
        // Keys in other groups are ignored.
        let other = "[Desktop Entry]\nType=Application\nName=A\nExec=a\n[Desktop Action x]\nName=B\nExec=b\n";
        assert_eq!(parse_desktop_entry(other).unwrap().name, "A");
        // OnlyShowIn is not filtered.
        assert!(parse_desktop_entry("[Desktop Entry]\nType=Application\nName=A\nExec=a\nOnlyShowIn=KDE;\n").is_some());
    }

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("nd-apps-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn de(name: &str, exec: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n")
    }

    #[test]
    fn scan_ids_precedence_sorting() {
        let t = Tmp::new("scan");
        let (a, b) = (t.0.join("a"), t.0.join("b"));
        write(&a, "foo.desktop", &de("Zed", "zed-a %U"));
        write(&b, "foo.desktop", &de("Shadowed", "zed-b"));
        write(&b, "kde/foo.desktop", &de("alpha", "/usr/bin/alpha --x"));
        write(&b, "hidden.desktop", "[Desktop Entry]\nType=Application\nName=H\nExec=h\nNoDisplay=true\n");
        write(&b, "noexec.desktop", "[Desktop Entry]\nType=Application\nName=N\n");
        write(&b, "gone.desktop", &format!("{}TryExec=/definitely/not/here\n", de("Gone", "gone")));
        write(&b, "icon.desktop", &format!("{}Icon=/some/icon.png\n", de("Beta", "beta")));
        let (apps, icons) = scan_dirs(&[a, b]);
        let ids: Vec<_> = apps.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["kde-foo.desktop", "icon.desktop", "foo.desktop"]);
        assert_eq!(apps[0].target, "/usr/bin/alpha");
        assert_eq!(apps[0].args, ["--x"]);
        assert_eq!(apps[2].name, "Zed");
        assert_eq!(apps[2].target, "zed-a");
        assert!(apps[2].args.is_empty());
        assert_eq!(icons.get("icon.desktop").map(String::as_str), Some("/some/icon.png"));
    }

    /// Run with `cargo test -p nd-platform real_system_icons -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_system_icons() {
        let cat = DesktopFiles::new();
        let apps = cat.list().unwrap();
        let mut missing = Vec::new();
        for a in &apps {
            match cat.icon(a).unwrap() {
                Some(b) => {
                    assert!(b.starts_with(b"\x89PNG") || b.windows(4).any(|w| w == b"<svg") || b.starts_with(b"<?xml"))
                }
                None => missing.push(format!("{} (Icon={:?})", a.id, cat.icons.lock().unwrap().get(&a.id))),
            }
        }
        println!("apps {} found {} missing {}", apps.len(), apps.len() - missing.len(), missing.len());
        for m in &missing {
            println!("missing: {m}");
        }
    }
}
