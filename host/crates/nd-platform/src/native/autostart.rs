// SPDX-License-Identifier: GPL-3.0-or-later
//! Start-at-login registration: HKCU Run key on Windows, XDG autostart entry on Linux.

use crate::{Autostart, PlatformError, Result};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// Pure helpers (compiled on every OS so they are unit-testable everywhere).
// ---------------------------------------------------------------------------------------------

/// Quote one argument for a Windows command line (CommandLineToArgvW rules, simplified).
#[allow(dead_code)]
fn win_quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Registry Run value data: `"<exe>" <args...>`. The exe is always quoted.
#[allow(dead_code)]
fn windows_run_value(exe: &Path, args: &[&str]) -> String {
    let mut s = format!("\"{}\"", exe.display());
    for a in args {
        s.push(' ');
        s.push_str(&win_quote(a));
    }
    s
}

/// Extract the executable path from a Run value (quoted or bare).
#[allow(dead_code)]
fn windows_run_value_exe(value: &str) -> &str {
    let v = value.trim_start();
    if let Some(rest) = v.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else {
        v.split(' ').next().unwrap_or("")
    }
}

/// Case-insensitive, separator-insensitive path comparison for Windows paths.
#[allow(dead_code)]
fn windows_same_exe(value: &str, exe: &Path) -> bool {
    let norm = |s: &str| s.replace('/', "\\").to_lowercase();
    norm(windows_run_value_exe(value)) == norm(&exe.display().to_string())
}

/// Quote one argument for a desktop-entry `Exec=` key (Desktop Entry Spec, "The Exec key").
/// Returns the form to place in the *unescaped* command line; `%` is doubled.
#[allow(dead_code)]
fn desktop_exec_quote(arg: &str) -> String {
    let arg = arg.replace('%', "%%");
    const RESERVED: &[char] =
        &[' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')', '`'];
    if !arg.is_empty() && !arg.contains(RESERVED) {
        return arg;
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Full `Exec=` value as stored in the file (command line, then string-level backslash escaping).
#[allow(dead_code)]
fn desktop_exec_line(exe: &Path, args: &[&str]) -> String {
    let mut cmd = desktop_exec_quote(&exe.display().to_string());
    for a in args {
        cmd.push(' ');
        cmd.push_str(&desktop_exec_quote(a));
    }
    cmd.replace('\\', "\\\\")
}

/// Contents of `notideck.desktop`.
#[allow(dead_code)]
fn desktop_file_content(exe: &Path, args: &[&str]) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Notideck\nComment=Turn your phone into a deck and webcam\n\
         Exec={}\nIcon=notideck\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        desktop_exec_line(exe, args)
    )
}

/// Does desktop file `content` launch `exe`? Compares the stored Exec value's first token.
#[allow(dead_code)]
fn desktop_file_references(content: &str, exe: &Path) -> bool {
    let expected = desktop_exec_line(exe, &[]);
    content
        .lines()
        .filter_map(|l| l.strip_prefix("Exec="))
        .any(|v| v == expected || v.strip_prefix(&expected).is_some_and(|r| r.starts_with(' ')))
}

#[allow(dead_code)]
fn desktop_autostart_path(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_config_home.filter(|s| !s.is_empty()) {
        Some(x) if Path::new(x).is_absolute() => PathBuf::from(x),
        _ => PathBuf::from(home.filter(|s| !s.is_empty())?).join(".config"),
    };
    Some(base.join("autostart").join("notideck.desktop"))
}

// ---------------------------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------------------------

#[cfg(target_os = "linux")]
struct LinuxAutostart {
    exe: PathBuf,
    args: Vec<String>,
    /// Overridable for tests.
    file: Option<PathBuf>,
}

#[cfg(target_os = "linux")]
impl LinuxAutostart {
    fn path(&self) -> Result<PathBuf> {
        if let Some(f) = &self.file {
            return Ok(f.clone());
        }
        desktop_autostart_path(std::env::var("XDG_CONFIG_HOME").ok().as_deref(), std::env::var("HOME").ok().as_deref())
            .ok_or_else(|| PlatformError::Other("cannot determine the autostart directory (no HOME)".into()))
    }
}

#[cfg(target_os = "linux")]
impl Autostart for LinuxAutostart {
    fn is_enabled(&self) -> Result<bool> {
        match std::fs::read_to_string(self.path()?) {
            Ok(c) => Ok(desktop_file_references(&c, &self.exe)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn set_enabled(&self, enabled: bool) -> Result<()> {
        let path = self.path()?;
        if enabled {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
            std::fs::write(&path, desktop_file_content(&self.exe, &args))?;
        } else {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    const SUBKEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    const NAME: PCWSTR = w!("Notideck");

    pub struct WindowsAutostart {
        pub exe: PathBuf,
        pub args: Vec<String>,
    }

    fn check(what: &str, e: WIN32_ERROR) -> Result<()> {
        if e == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(PlatformError::Other(format!("{what} failed: Win32 error {}", e.0)))
        }
    }

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    impl Autostart for WindowsAutostart {
        fn is_enabled(&self) -> Result<bool> {
            unsafe {
                let mut h = HKEY::default();
                let e = RegOpenKeyExW(HKEY_CURRENT_USER, SUBKEY, 0, KEY_QUERY_VALUE, &mut h);
                if e == ERROR_FILE_NOT_FOUND {
                    return Ok(false);
                }
                check("RegOpenKeyExW", e)?;
                let key = Key(h);
                let mut ty = Default::default();
                let mut len = 0u32;
                let e = RegQueryValueExW(key.0, NAME, None, Some(&mut ty), None, Some(&mut len));
                if e == ERROR_FILE_NOT_FOUND {
                    return Ok(false);
                }
                check("RegQueryValueExW", e)?;
                let mut buf = vec![0u16; (len as usize).div_ceil(2) + 1];
                let mut len = (buf.len() * 2) as u32;
                let e = RegQueryValueExW(
                    key.0,
                    NAME,
                    None,
                    Some(&mut ty),
                    Some(buf.as_mut_ptr() as *mut u8),
                    Some(&mut len),
                );
                if e == ERROR_FILE_NOT_FOUND {
                    return Ok(false);
                }
                check("RegQueryValueExW", e)?;
                let n = (len as usize / 2).min(buf.len());
                let value = String::from_utf16_lossy(&buf[..n]);
                let value = value.trim_end_matches('\0');
                Ok(windows_same_exe(value, &self.exe))
            }
        }

        fn set_enabled(&self, enabled: bool) -> Result<()> {
            unsafe {
                if enabled {
                    let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
                    let value = windows_run_value(&self.exe, &args);
                    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
                    let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
                    let mut h = HKEY::default();
                    check(
                        "RegCreateKeyExW",
                        RegCreateKeyExW(
                            HKEY_CURRENT_USER,
                            SUBKEY,
                            0,
                            PCWSTR::null(),
                            REG_OPTION_NON_VOLATILE,
                            KEY_SET_VALUE,
                            None,
                            &mut h,
                            None,
                        ),
                    )?;
                    let key = Key(h);
                    check("RegSetValueExW", RegSetValueExW(key.0, NAME, 0, REG_SZ, Some(bytes)))
                } else {
                    let mut h = HKEY::default();
                    let e = RegOpenKeyExW(HKEY_CURRENT_USER, SUBKEY, 0, KEY_SET_VALUE, &mut h);
                    if e == ERROR_FILE_NOT_FOUND {
                        return Ok(());
                    }
                    check("RegOpenKeyExW", e)?;
                    let key = Key(h);
                    let e = RegDeleteValueW(key.0, NAME);
                    if e == ERROR_FILE_NOT_FOUND {
                        return Ok(());
                    }
                    check("RegDeleteValueW", e)
                }
            }
        }
    }
}

struct Unsupported;

impl Autostart for Unsupported {
    fn is_enabled(&self) -> Result<bool> {
        Ok(false)
    }
    fn set_enabled(&self, _: bool) -> Result<()> {
        Err(PlatformError::Unsupported("autostart"))
    }
}

#[allow(unused_variables)]
pub(crate) fn create(exe: &Path, args: &[&str]) -> Box<dyn Autostart> {
    #[cfg(target_os = "linux")]
    {
        return Box::new(LinuxAutostart {
            exe: exe.to_path_buf(),
            args: args.iter().map(|s| s.to_string()).collect(),
            file: None,
        });
    }
    #[cfg(windows)]
    {
        return Box::new(win::WindowsAutostart {
            exe: exe.to_path_buf(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });
    }
    #[allow(unreachable_code)]
    Box::new(Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_value_quotes_exe_and_args() {
        let v = windows_run_value(Path::new(r"C:\Program Files\Notideck\notideck.exe"), &["--minimized", "a b"]);
        assert_eq!(v, r#""C:\Program Files\Notideck\notideck.exe" --minimized "a b""#);
        assert_eq!(win_quote(r#"x"y"#), r#""x\"y""#);
        assert_eq!(win_quote(r"a b\"), r#""a b\\""#);
        assert_eq!(win_quote(""), "\"\"");
    }

    #[test]
    fn win_same_exe_is_case_insensitive() {
        let exe = Path::new(r"C:\Apps\Notideck.exe");
        assert!(windows_same_exe(r#""c:\apps\NOTIDECK.exe" --minimized"#, exe));
        assert!(windows_same_exe(r"c:\apps\notideck.exe", exe));
        assert!(!windows_same_exe(r#""C:\Other\Notideck.exe""#, exe));
    }

    #[test]
    fn desktop_quote_rules() {
        assert_eq!(desktop_exec_quote("/usr/bin/notideck"), "/usr/bin/notideck");
        assert_eq!(desktop_exec_quote("/opt/My App/nd"), "\"/opt/My App/nd\"");
        assert_eq!(desktop_exec_quote("100%"), "100%%");
        assert_eq!(desktop_exec_quote("a$b"), "\"a\\$b\"");
        assert_eq!(desktop_exec_quote("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(desktop_exec_quote(""), "\"\"");
    }

    #[test]
    fn desktop_line_double_escapes_backslashes() {
        // Command line is `"a\$b"`; the file stores each backslash doubled.
        assert_eq!(desktop_exec_line(Path::new("a$b"), &[]), "\"a\\\\$b\"");
        assert_eq!(desktop_exec_line(Path::new("/opt/My App/nd"), &["--minimized"]), "\"/opt/My App/nd\" --minimized");
    }

    #[test]
    fn desktop_file_content_and_detection() {
        let exe = Path::new("/opt/My App/notideck");
        let c = desktop_file_content(exe, &["--minimized"]);
        assert!(c.starts_with("[Desktop Entry]\nType=Application\nName=Notideck\n"));
        assert!(c.contains("Exec=\"/opt/My App/notideck\" --minimized\n"));
        assert!(c.contains("Icon=notideck\n"));
        assert!(c.contains("X-GNOME-Autostart-enabled=true\n"));
        assert!(desktop_file_references(&c, exe));
        assert!(!desktop_file_references(&c, Path::new("/opt/My App/other")));
        assert!(!desktop_file_references(&c, Path::new("/opt/My")));
    }

    #[test]
    fn autostart_path_resolution() {
        // `is_absolute` and the separator are OS-specific, so build the expectations per OS.
        let (xdg, home) = if cfg!(windows) { (r"C:\x\cfg", r"C:\Users\u") } else { ("/x/cfg", "/home/u") };
        let want = |base: &str| PathBuf::from(base).join("autostart").join("notideck.desktop");
        assert_eq!(desktop_autostart_path(Some(xdg), Some(home)).unwrap(), want(xdg));
        let home_cfg = PathBuf::from(home).join(".config");
        let home_cfg = home_cfg.to_str().unwrap();
        assert_eq!(desktop_autostart_path(Some(""), Some(home)).unwrap(), want(home_cfg));
        assert_eq!(desktop_autostart_path(Some("relative"), Some(home)).unwrap(), want(home_cfg));
        assert!(desktop_autostart_path(None, None).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_roundtrip_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!("nd-autostart-test-{}", std::process::id()));
        let file = dir.join("autostart").join("notideck.desktop");
        let a = LinuxAutostart { exe: "/usr/bin/notideck".into(), args: vec!["--minimized".into()], file: Some(file) };
        assert!(!a.is_enabled().unwrap());
        a.set_enabled(true).unwrap();
        assert!(a.is_enabled().unwrap());
        a.set_enabled(false).unwrap();
        a.set_enabled(false).unwrap();
        assert!(!a.is_enabled().unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }
}
