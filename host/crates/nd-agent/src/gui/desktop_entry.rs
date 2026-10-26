//! Linux: make sure a `notideck.desktop` exists so the compositor can map the window's
//! Wayland app_id (`notideck`) to a name and icon even for dev / AppImage / tarball runs.
//! We only ever create or rewrite files carrying our marker line.

use std::path::{Path, PathBuf};

pub const APP_ID: &str = "notideck";
const FILE_NAME: &str = "notideck.desktop";
const MARKER: &str = "X-Notideck-Generated=true";
const ICON_PNG: &[u8] = include_bytes!("../../res/icon.png");

/// Quote one `Exec` argument per the desktop-entry spec, including the string-level escaping
/// of backslashes that applies to the whole value.
pub fn escape_exec_arg(arg: &str) -> String {
    let needs_quote = arg.is_empty()
        || arg.chars().any(|c| {
            matches!(
                c,
                ' ' | '\t'
                    | '\n'
                    | '"'
                    | '\''
                    | '\\'
                    | '>'
                    | '<'
                    | '~'
                    | '|'
                    | '&'
                    | ';'
                    | '$'
                    | '*'
                    | '?'
                    | '#'
                    | '('
                    | ')'
                    | '`'
            )
        });
    let arg = arg.replace('%', "%%");
    let quoted = if needs_quote {
        let mut s = String::from("\"");
        for c in arg.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                s.push('\\');
            }
            s.push(c);
        }
        s.push('"');
        s
    } else {
        arg
    };
    quoted.replace('\\', "\\\\")
}

pub fn exec_line(exe: &str) -> String {
    format!("Exec={} --open", escape_exec_arg(exe))
}

pub fn file_text(exe: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Notideck\nComment=Turn a phone into a Stream Deck-style controller\n{}\nIcon=notideck\nTerminal=false\nCategories=Utility;\nStartupWMClass={APP_ID}\n{MARKER}\n",
        exec_line(exe)
    )
}

/// `own`: content of the user-level file if present. `other_exists`: a `notideck.desktop`
/// exists in some other data dir. Write when nothing exists, or our own file is outdated.
pub fn should_write(own: Option<&str>, other_exists: bool, desired: &str) -> bool {
    match own {
        Some(text) => text.lines().any(|l| l == MARKER) && text != desired,
        None => !other_exists,
    }
}

/// Core of [`ensure`], with the directories injected.
pub fn ensure_in(data_home: &Path, data_dirs: &[PathBuf], exe: &str) -> std::io::Result<bool> {
    let own_path = data_home.join("applications").join(FILE_NAME);
    let own = std::fs::read_to_string(&own_path).ok();
    let other_exists = data_dirs.iter().any(|d| d.join("applications").join(FILE_NAME).exists());
    let desired = file_text(exe);
    // An unreadable-but-present file must never be overwritten.
    if own.is_none() && own_path.exists() {
        return Ok(false);
    }
    if !should_write(own.as_deref(), other_exists, &desired) {
        return Ok(false);
    }
    if let Some(dir) = own_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&own_path, desired)?;
    let icon = data_home.join("icons/hicolor/256x256/apps/notideck.png");
    if std::fs::read(&icon).ok().as_deref() != Some(ICON_PNG) {
        if let Some(dir) = icon.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&icon, ICON_PNG)?;
    }
    Ok(true)
}

fn env_dirs(var: &str, default: &str) -> Vec<PathBuf> {
    let v = std::env::var(var).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string());
    v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect()
}

/// Best-effort: failures only log a warning.
pub fn ensure() {
    let data_home = match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(".local/share"),
            None => return,
        },
    };
    let exe = match std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()).map(PathBuf::from) {
        Some(p) => p,
        None => match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("desktop entry: current_exe failed: {e}");
                return;
            }
        },
    };
    let Some(exe) = exe.to_str() else { return };
    let dirs = env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share");
    match ensure_in(&data_home, &dirs, exe) {
        Ok(true) => tracing::info!("wrote user desktop entry for {exe}"),
        Ok(false) => {}
        Err(e) => tracing::warn!("desktop entry could not be written: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nd-desktop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn escaping() {
        assert_eq!(escape_exec_arg("/usr/bin/notideck"), "/usr/bin/notideck");
        assert_eq!(escape_exec_arg("/a b/n"), "\"/a b/n\"");
        assert_eq!(escape_exec_arg("/a$b"), "\"/a\\\\$b\"");
        assert_eq!(escape_exec_arg("/100%"), "/100%%");
        assert_eq!(escape_exec_arg("/a\"b"), "\"/a\\\\\"b\"");
    }

    #[test]
    fn decision() {
        let want = file_text("/x");
        assert!(should_write(None, false, &want));
        assert!(!should_write(None, true, &want));
        assert!(!should_write(Some(&want), false, &want));
        assert!(should_write(Some(&file_text("/old")), false, &want));
        assert!(!should_write(Some("[Desktop Entry]\nExec=/old\n"), false, &want));
    }

    #[test]
    fn writes_and_rewrites() {
        let home = tmp("home");
        let sys = tmp("sys");
        assert!(ensure_in(&home, std::slice::from_ref(&sys), "/a/nd").unwrap());
        let p = home.join("applications/notideck.desktop");
        assert!(std::fs::read_to_string(&p).unwrap().contains("Exec=/a/nd --open"));
        assert!(home.join("icons/hicolor/256x256/apps/notideck.png").exists());
        assert!(!ensure_in(&home, std::slice::from_ref(&sys), "/a/nd").unwrap());
        assert!(ensure_in(&home, std::slice::from_ref(&sys), "/b/nd").unwrap());
        assert!(std::fs::read_to_string(&p).unwrap().contains("Exec=/b/nd --open"));
        // foreign file without marker is left alone
        std::fs::write(&p, "[Desktop Entry]\nExec=mine\n").unwrap();
        assert!(!ensure_in(&home, &[sys], "/c/nd").unwrap());
        assert!(std::fs::read_to_string(&p).unwrap().contains("Exec=mine"));
    }

    #[test]
    fn system_entry_wins() {
        let home = tmp("home2");
        let sys = tmp("sys2");
        std::fs::create_dir_all(sys.join("applications")).unwrap();
        std::fs::write(sys.join("applications/notideck.desktop"), "x").unwrap();
        assert!(!ensure_in(&home, &[sys], "/a/nd").unwrap());
        assert!(!home.join("applications/notideck.desktop").exists());
    }
}
