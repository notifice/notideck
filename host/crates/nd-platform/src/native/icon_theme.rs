// SPDX-License-Identifier: GPL-3.0-or-later
//! Freedesktop icon-theme lookup (a pragmatic subset of the icon theme spec): theme chain via
//! `Inherits=`, per-directory `Size`/`Scale`, PNG preferred at >= 128 px, then SVG, then the largest PNG.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub(crate) const MAX_PNG_BYTES: u64 = 2 * 1024 * 1024; // the page downsizes anything larger than 512 px before upload
pub(crate) const MAX_SVG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
struct ThemeDir {
    /// Directory relative to the theme root, e.g. `apps/48`.
    rel: String,
    /// Nominal pixel size (`Size` times `Scale`).
    size: u32,
}

#[derive(Debug, Clone)]
struct Theme {
    name: String,
    dirs: Vec<ThemeDir>,
}

/// Parsed theme chain plus base directories. Build once per catalog listing.
#[derive(Debug, Default)]
pub(crate) struct IconLookup {
    bases: Vec<PathBuf>,
    pixmaps: Option<PathBuf>,
    themes: Vec<Theme>,
}

/// `Inherits=` and `Directories=` plus per-directory settings from an `index.theme`.
fn parse_index(text: &str) -> (Vec<String>, Vec<ThemeDir>) {
    let mut group = String::new();
    let (mut inherits, mut dir_names): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut per: Vec<(String, u32, u32)> = Vec::new(); // (group, size, scale)
    let mut cur: Option<(String, u32, u32)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(g) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if let Some(c) = cur.take() {
                per.push(c);
            }
            group = g.to_string();
            if group != "Icon Theme" {
                cur = Some((group.clone(), 0, 1));
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if group == "Icon Theme" {
            let list = || v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>();
            match k {
                "Inherits" => inherits = list(),
                "Directories" => dir_names = list(),
                _ => {}
            }
        } else if let Some(c) = cur.as_mut() {
            match k {
                "Size" => c.1 = v.parse().unwrap_or(0),
                "Scale" => c.2 = v.parse().unwrap_or(1).max(1),
                _ => {}
            }
        }
    }
    if let Some(c) = cur.take() {
        per.push(c);
    }
    let dirs = dir_names
        .into_iter()
        .filter_map(|d| {
            let (_, size, scale) = per.iter().find(|(g, _, _)| *g == d)?;
            Some(ThemeDir { rel: d, size: size * scale })
        })
        .collect();
    (inherits, dirs)
}

/// Lower sorts first: PNG >= 128 closest to 256, then SVG, then the largest smaller PNG.
fn rank(svg: bool, size: u32) -> (u8, u32) {
    if svg {
        (1, 0)
    } else if size >= 128 {
        (0, size.abs_diff(256))
    } else {
        (2, u32::MAX - size)
    }
}

fn read_capped(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() || m.len() > cap {
        return None;
    }
    std::fs::read(path).ok()
}

/// Read an icon file by absolute path. `.png` and `.svg` only, size-capped.
pub(crate) fn read_icon_file(p: &Path) -> Option<Vec<u8>> {
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => read_capped(p, MAX_PNG_BYTES),
        "svg" => read_capped(p, MAX_SVG_BYTES),
        _ => None,
    }
}

/// `Icon=foo.png` -> `foo` (only for the known icon extensions; real names contain dots).
fn strip_icon_ext(name: &str) -> &str {
    for ext in [".png", ".svg", ".xpm"] {
        if let Some(s) = name.strip_suffix(ext) {
            return s;
        }
    }
    name
}

impl IconLookup {
    /// `bases` are `icons` directories in precedence order; `theme` is the user's theme name.
    pub(crate) fn new(bases: Vec<PathBuf>, pixmaps: Option<PathBuf>, theme: Option<&str>) -> Self {
        let mut me = IconLookup { bases, pixmaps, themes: Vec::new() };
        let mut seen = HashSet::new();
        let mut queue: Vec<String> = theme.map(|t| vec![t.to_string()]).unwrap_or_default();
        // Depth-first over Inherits, so a theme's parents come right after it.
        while !queue.is_empty() {
            let name = queue.remove(0);
            if !seen.insert(name.clone()) {
                continue;
            }
            let Some((inherits, dirs)) = me.load_theme(&name) else { continue };
            me.themes.push(Theme { name, dirs });
            let mut next = inherits;
            next.append(&mut queue);
            queue = next;
        }
        if seen.insert("hicolor".to_string()) {
            if let Some((_, dirs)) = me.load_theme("hicolor") {
                me.themes.push(Theme { name: "hicolor".into(), dirs });
            }
        } else if let Some(pos) = me.themes.iter().position(|t| t.name == "hicolor") {
            // hicolor always goes last, even if a theme inherits it explicitly.
            let h = me.themes.remove(pos);
            me.themes.push(h);
        }
        me
    }

    fn load_theme(&self, name: &str) -> Option<(Vec<String>, Vec<ThemeDir>)> {
        if name.is_empty() || name.contains('/') {
            return None;
        }
        self.bases.iter().find_map(|b| {
            let text = std::fs::read_to_string(b.join(name).join("index.theme")).ok()?;
            Some(parse_index(&text))
        })
    }

    /// Resolve an `Icon=` value (theme name or absolute path) to image bytes (PNG or SVG).
    pub(crate) fn lookup(&self, icon: &str) -> Option<Vec<u8>> {
        let p = Path::new(icon);
        if p.is_absolute() {
            return read_icon_file(p);
        }
        let name = strip_icon_ext(icon);
        if name.is_empty() || name.contains('/') {
            return None;
        }
        for theme in &self.themes {
            if let Some(b) = self.find_in_theme(theme, name) {
                return Some(b);
            }
        }
        // Icons living directly in a base dir, then the flat pixmaps dir.
        for dir in self.bases.iter().chain(self.pixmaps.iter()) {
            for ext in ["png", "svg"] {
                if let Some(b) = read_icon_file(&dir.join(format!("{name}.{ext}"))) {
                    return Some(b);
                }
            }
        }
        None
    }

    fn find_in_theme(&self, theme: &Theme, name: &str) -> Option<Vec<u8>> {
        let mut cands: Vec<((u8, u32), PathBuf)> = Vec::new();
        for d in &theme.dirs {
            for base in &self.bases {
                let dir = base.join(&theme.name).join(&d.rel);
                for (ext, svg) in [("png", false), ("svg", true)] {
                    let f = dir.join(format!("{name}.{ext}"));
                    if f.is_file() {
                        cands.push((rank(svg, d.size), f));
                    }
                }
            }
        }
        if cands.is_empty() {
            cands = self.unindexed_candidates(theme, name);
        }
        cands.sort_by_key(|a| a.0);
        cands.into_iter().find_map(|(_, f)| read_icon_file(&f))
    }

    /// Size directories present on disk but missing from `index.theme` (AppImageLauncher puts
    /// icons in `~/.local/share/icons/hicolor/1024x1024/apps`, which hicolor's index doesn't list).
    fn unindexed_candidates(&self, theme: &Theme, name: &str) -> Vec<((u8, u32), PathBuf)> {
        let mut cands = Vec::new();
        for base in &self.bases {
            let Ok(sizes) = std::fs::read_dir(base.join(&theme.name)) else { continue };
            for size_dir in sizes.flatten() {
                let dir_name = size_dir.file_name().to_string_lossy().into_owned();
                let Some(size) = dir_size(&dir_name) else { continue };
                let Ok(contexts) = std::fs::read_dir(size_dir.path()) else { continue };
                for ctx in contexts.flatten() {
                    for (ext, svg) in [("png", false), ("svg", true)] {
                        let f = ctx.path().join(format!("{name}.{ext}"));
                        if f.is_file() {
                            cands.push((rank(svg, size), f));
                        }
                    }
                }
            }
        }
        cands
    }
}

/// Nominal size of a theme directory named `48x48`, `48x48@2` or `scalable` (0).
fn dir_size(name: &str) -> Option<u32> {
    if name == "scalable" {
        return Some(0);
    }
    let (dims, scale) = match name.split_once('@') {
        Some((d, s)) => (d, s.parse::<u32>().ok()?.max(1)),
        None => (name, 1),
    };
    let (w, h) = dims.split_once('x')?;
    let (w, _) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    Some(w * scale)
}

/// The user's icon theme name: KDE `kdeglobals` `[Icons] Theme=`, else `breeze` on KDE, else none.
pub(crate) fn user_theme() -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let cfg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let mut files: Vec<PathBuf> = Vec::new();
    if let Some(c) = &cfg {
        files.push(c.join("kdeglobals"));
        // Plasma writes the default look-and-feel theme here until the user overrides it.
        files.push(c.join("kdedefaults/kdeglobals"));
    }
    files.push(PathBuf::from("/etc/xdg/kdeglobals"));
    for f in files {
        if let Some(t) = std::fs::read_to_string(&f).ok().and_then(|t| kde_icon_theme(&t)) {
            return Some(t);
        }
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_uppercase();
    desktop.split(':').any(|d| d == "KDE").then(|| "breeze".to_string())
}

pub(crate) fn kde_icon_theme(text: &str) -> Option<String> {
    let mut in_icons = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_icons = line == "[Icons]";
        } else if in_icons {
            if let Some(v) = line.strip_prefix("Theme=") {
                let v = v.trim();
                return (!v.is_empty()).then(|| v.to_string());
            }
        }
    }
    None
}

/// Icon base dirs: `$XDG_DATA_HOME/icons`, `~/.icons`, `$XDG_DATA_DIRS/*/icons`, flatpak exports.
pub(crate) fn default_bases() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out: Vec<PathBuf> = Vec::new();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    if let Some(d) = data_home {
        out.push(d.join("icons"));
    }
    if let Some(h) = &home {
        out.push(h.join(".icons"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    out.extend(data_dirs.split(':').filter(|s| !s.is_empty()).map(|d| Path::new(d).join("icons")));
    if let Some(h) = &home {
        out.push(h.join(".local/share/flatpak/exports/share/icons"));
    }
    out.push(PathBuf::from("/var/lib/flatpak/exports/share/icons"));
    let mut seen = HashSet::new();
    out.retain(|d| seen.insert(d.clone()));
    out
}

impl IconLookup {
    /// Lookup for the running system.
    pub(crate) fn system() -> Self {
        Self::new(default_bases(), Some(PathBuf::from("/usr/share/pixmaps")), user_theme().as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("nd-icons-{tag}-{}", std::process::id()));
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

    fn write(dir: &Path, rel: &str, body: &[u8]) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn index(inherits: &str, dirs: &[(&str, u32)]) -> String {
        let names: Vec<_> = dirs.iter().map(|d| d.0).collect();
        let mut s = format!("[Icon Theme]\nName=T\nInherits={inherits}\nDirectories={}\n", names.join(","));
        for (d, size) in dirs {
            s.push_str(&format!("\n[{d}]\nSize={size}\nType=Fixed\n"));
        }
        s
    }

    #[test]
    fn inherits_chain_and_hicolor_last() {
        let t = Tmp::new("chain");
        let icons = t.0.join("icons");
        write(&icons, "child/index.theme", index("parent", &[("apps/48", 48)]).as_bytes());
        write(&icons, "parent/index.theme", index("", &[("apps/48", 48)]).as_bytes());
        write(&icons, "hicolor/index.theme", index("", &[("apps/48", 48)]).as_bytes());
        write(&icons, "parent/apps/48/only-parent.png", b"parent");
        write(&icons, "hicolor/apps/48/only-hicolor.png", b"hicolor");
        write(&icons, "parent/apps/48/both.png", b"parent");
        write(&icons, "child/apps/48/both.png", b"child");
        write(&icons, "hicolor/apps/48/both.png", b"hicolor");
        let l = IconLookup::new(vec![icons], None, Some("child"));
        assert_eq!(l.lookup("both"), Some(b"child".to_vec()));
        assert_eq!(l.lookup("only-parent"), Some(b"parent".to_vec()));
        assert_eq!(l.lookup("only-hicolor"), Some(b"hicolor".to_vec()));
        assert_eq!(l.lookup("missing"), None);
    }

    #[test]
    fn inherits_cycle_terminates() {
        let t = Tmp::new("cycle");
        let icons = t.0.join("icons");
        write(&icons, "a/index.theme", index("b", &[("apps/48", 48)]).as_bytes());
        write(&icons, "b/index.theme", index("a", &[("apps/48", 48)]).as_bytes());
        write(&icons, "b/apps/48/x.png", b"b");
        let l = IconLookup::new(vec![icons], None, Some("a"));
        assert_eq!(l.lookup("x"), Some(b"b".to_vec()));
    }

    #[test]
    fn png_svg_preference() {
        let t = Tmp::new("pref");
        let icons = t.0.join("icons");
        let dirs = [("apps/48", 48), ("apps/128", 128), ("apps/256", 256), ("apps/scalable", 64)];
        write(&icons, "th/index.theme", index("", &dirs).as_bytes());
        // Big PNG beats SVG.
        write(&icons, "th/apps/48/a.png", b"png48");
        write(&icons, "th/apps/128/a.png", b"png128");
        write(&icons, "th/apps/256/a.png", b"png256");
        write(&icons, "th/apps/scalable/a.svg", b"<svg/>");
        // Only small PNGs: SVG wins.
        write(&icons, "th/apps/48/b.png", b"png48");
        write(&icons, "th/apps/scalable/b.svg", b"<svg/>");
        // Only small PNGs: largest.
        write(&icons, "th/apps/48/c.png", b"png48");
        write(&icons, "th/apps/128/c.png", b"png128");
        let l = IconLookup::new(vec![icons.clone()], None, Some("th"));
        assert_eq!(l.lookup("a"), Some(b"png256".to_vec()));
        assert_eq!(l.lookup("b"), Some(b"<svg/>".to_vec()));
        // 128 counts as "big enough" so c resolves to png128.
        assert_eq!(l.lookup("c"), Some(b"png128".to_vec()));
        std::fs::remove_file(icons.join("th/apps/128/c.png")).unwrap();
        assert_eq!(l.lookup("c"), Some(b"png48".to_vec()));
    }

    #[test]
    fn size_dirs_missing_from_the_index_are_searched() {
        let t = Tmp::new("unidx");
        let sys = t.0.join("sys");
        let user = t.0.join("user");
        write(&sys, "hicolor/index.theme", index("", &[("48x48/apps", 48)]).as_bytes());
        write(&user, "hicolor/1024x1024/apps/ai_app.png", b"big");
        write(&user, "hicolor/32x32/apps/ai_app.png", b"small");
        let l = IconLookup::new(vec![user, sys], None, None);
        assert_eq!(l.lookup("ai_app"), Some(b"big".to_vec()));
        assert_eq!((dir_size("48x48@2"), dir_size("scalable"), dir_size("apps")), (Some(96), Some(0), None));
    }

    #[test]
    fn scale_multiplies_size() {
        let (_, dirs) = parse_index("[Icon Theme]\nDirectories=apps/64@2\n[apps/64@2]\nSize=64\nScale=2\nType=Fixed\n");
        assert_eq!(dirs[0].size, 128);
    }

    #[test]
    fn pixmaps_and_names_with_extension() {
        let t = Tmp::new("pix");
        let icons = t.0.join("icons");
        let pix = t.0.join("pixmaps");
        write(&icons, "hicolor/index.theme", index("", &[("apps/48", 48)]).as_bytes());
        write(&pix, "legacy.png", b"pix");
        write(&icons, "hicolor/apps/48/foo.png", b"foo");
        let l = IconLookup::new(vec![icons], Some(pix), None);
        assert_eq!(l.lookup("legacy"), Some(b"pix".to_vec()));
        assert_eq!(l.lookup("legacy.png"), Some(b"pix".to_vec()));
        assert_eq!(l.lookup("foo.png"), Some(b"foo".to_vec()));
        assert_eq!(l.lookup("foo.svg"), Some(b"foo".to_vec()));
    }

    #[test]
    fn absolute_paths() {
        let t = Tmp::new("abs");
        write(&t.0, "i.svg", b"<svg/>");
        write(&t.0, "i.png", b"png");
        write(&t.0, "i.xpm", b"xpm");
        write(&t.0, "big.svg", &vec![b' '; 1024 * 1024 + 1]);
        write(&t.0, "big.png", &vec![0u8; MAX_PNG_BYTES as usize + 1]);
        let l = IconLookup::new(Vec::new(), None, None);
        assert_eq!(l.lookup(t.0.join("i.svg").to_str().unwrap()), Some(b"<svg/>".to_vec()));
        assert_eq!(l.lookup(t.0.join("i.png").to_str().unwrap()), Some(b"png".to_vec()));
        assert_eq!(l.lookup(t.0.join("i.xpm").to_str().unwrap()), None);
        assert_eq!(l.lookup(t.0.join("big.svg").to_str().unwrap()), None);
        assert_eq!(l.lookup(t.0.join("big.png").to_str().unwrap()), None);
    }

    #[test]
    fn kde_theme_parse() {
        assert_eq!(kde_icon_theme("[General]\nTheme=no\n[Icons]\nTheme=breeze-dark\n").as_deref(), Some("breeze-dark"));
        assert_eq!(kde_icon_theme("[General]\nTheme=x\n"), None);
    }
}
