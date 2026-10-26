// SPDX-License-Identifier: GPL-3.0-or-later
//! Single-instance guard and "show the first window" hand-off (ADR 0003).
//!
//! The primary instance holds an OS lock (Windows named mutex, Unix `flock`) and writes
//! `<data dir>/instance.json` with its UI port and token. A second launch reads that file
//! and sends `POST /api/shell/show` over plain loopback HTTP, then exits.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a secondary launch needs to reach the primary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub ui_port: u16,
    pub token: String,
}

/// Holds the single-instance lock until dropped (process exit releases it too).
pub struct InstanceLock {
    #[cfg(unix)]
    _file: std::fs::File,
    #[cfg(windows)]
    _handle: isize,
}

#[cfg(windows)]
impl Drop for InstanceLock {
    fn drop(&mut self) {
        #[link(name = "kernel32")]
        extern "system" {
            fn CloseHandle(h: isize) -> i32;
        }
        // SAFETY: the handle came from CreateMutexW and is closed exactly once.
        unsafe {
            CloseHandle(self._handle);
        }
    }
}

/// Per-user data directory: `%LOCALAPPDATA%\Notideck` or `$XDG_STATE_HOME/notideck`
/// (fallback `~/.local/state/notideck`). Holds logs and `instance.json`.
pub fn data_dir() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        return base.join("Notideck");
    }
    if let Some(x) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(x).join("notideck");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".local").join("state").join("notideck")
}

/// Try to become the single instance. `Ok(None)` means another instance holds the lock.
/// `suffix` separates dev instances that run on overridden ports.
pub fn acquire(suffix: Option<&str>) -> std::io::Result<Option<InstanceLock>> {
    #[cfg(unix)]
    {
        let name = match suffix {
            Some(s) => format!("notideck-{s}.lock"),
            None => "notideck.lock".to_string(),
        };
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(data_dir);
        lock_file(&dir.join(name))
    }
    #[cfg(windows)]
    {
        let name = match suffix {
            Some(s) => format!("Local\\NotideckAgent-{s}"),
            None => "Local\\NotideckAgent".to_string(),
        };
        win_mutex(&name)
    }
}

#[cfg(unix)]
fn lock_file(path: &Path) -> std::io::Result<Option<InstanceLock>> {
    use std::os::unix::io::AsRawFd;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    // SAFETY: plain flock(2) on a descriptor we own.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(InstanceLock { _file: file }));
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(err)
    }
}

#[cfg(windows)]
fn win_mutex(name: &str) -> std::io::Result<Option<InstanceLock>> {
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attrs: *const core::ffi::c_void, initial_owner: i32, name: *const u16) -> isize;
        fn GetLastError() -> u32;
    }
    const ERROR_ALREADY_EXISTS: u32 = 183;
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let (handle, err) = unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, wide.as_ptr());
        (h, GetLastError())
    };
    if handle == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if err == ERROR_ALREADY_EXISTS {
        return Ok(None);
    }
    Ok(Some(InstanceLock { _handle: handle }))
}

const INFO_FILE: &str = "instance.json";

/// Publish how to reach this instance (0600 on Unix, the token is a credential).
pub fn write_info(dir: &Path, info: &InstanceInfo) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(info).map_err(std::io::Error::other)?;
    let path = dir.join(INFO_FILE);
    let tmp = dir.join(format!("{INFO_FILE}.tmp"));
    {
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&json)?;
    }
    std::fs::rename(tmp, path)
}

pub fn read_info(dir: &Path) -> Option<InstanceInfo> {
    serde_json::from_slice(&std::fs::read(dir.join(INFO_FILE)).ok()?).ok()
}

/// Ask the primary instance to show its window. Returns the HTTP status line on success.
pub fn notify_primary(info: &InstanceInfo) -> std::io::Result<String> {
    post_primary(info, "/api/shell/show")
}

/// Ask the primary instance to quit (`--quit`). Returns the HTTP status line on success.
pub fn quit_primary(info: &InstanceInfo) -> std::io::Result<String> {
    post_primary(info, "/api/shell/quit")
}

/// Poll the single-instance lock until it is free (the primary has exited). `Ok(true)` when it was
/// free within `timeout`; the lock is released again right away.
pub fn wait_until_free(suffix: Option<&str>, timeout: Duration) -> std::io::Result<bool> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if acquire(suffix)?.is_some() {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `--quit`: returns the process exit code. 0 when the primary is gone (or never ran), 1 when it could
/// not be reached or did not exit within `timeout`.
pub fn run_quit(dir: &Path, suffix: Option<&str>, timeout: Duration) -> i32 {
    let running = !matches!(acquire(suffix), Ok(Some(_)));
    if !running {
        return 0;
    }
    let Some(info) = read_info(dir) else {
        eprintln!("Notideck is running, but its instance file is missing; cannot ask it to quit.");
        return 1;
    };
    if let Err(e) = quit_primary(&info) {
        eprintln!("Could not ask Notideck to quit: {e}");
        return 1;
    }
    match wait_until_free(suffix, timeout) {
        Ok(true) => 0,
        Ok(false) => {
            eprintln!("Notideck did not exit within {} seconds.", timeout.as_secs());
            1
        }
        Err(e) => {
            eprintln!("cannot check the instance lock: {e}");
            1
        }
    }
}

fn post_primary(info: &InstanceInfo, path: &str) -> std::io::Result<String> {
    use std::net::{SocketAddr, TcpStream};
    // We were just launched by the user, so we own the foreground right; pass it on so the
    // primary may bring its window to the front.
    #[cfg(windows)]
    // SAFETY: plain Win32 call without pointers.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], info.ui_port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    s.set_write_timeout(Some(Duration::from_secs(3)))?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nx-nd-token: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        info.ui_port, info.token
    );
    s.write_all(req.as_bytes())?;
    let mut buf = Vec::new();
    let _ = s.take(4096).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let status = text.lines().next().unwrap_or_default().to_string();
    if status.contains(" 2") {
        Ok(status)
    } else {
        Err(std::io::Error::other(format!("unexpected response: '{status}'")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nd-instance-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn info_roundtrip() {
        let dir = tmp("info");
        assert!(read_info(&dir).is_none());
        let info = InstanceInfo { ui_port: 4242, token: "abc".into() };
        write_info(&dir, &info).unwrap();
        assert_eq!(read_info(&dir), Some(info));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("instance.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn second_lock_is_refused() {
        let dir = tmp("lock");
        let path = dir.join("t.lock");
        let first = lock_file(&path).unwrap();
        assert!(first.is_some());
        assert!(lock_file(&path).unwrap().is_none());
        drop(first);
        assert!(lock_file(&path).unwrap().is_some());
    }

    #[test]
    fn notify_posts_with_token() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = c.read(&mut buf).unwrap();
            c.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        let r = notify_primary(&InstanceInfo { ui_port: port, token: "tok".into() }).unwrap();
        assert!(r.contains("204"));
        let req = t.join().unwrap();
        assert!(req.starts_with("POST /api/shell/show HTTP/1.1"));
        assert!(req.contains("x-nd-token: tok"));
    }

    #[test]
    fn quit_posts_to_the_quit_route() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = c.read(&mut buf).unwrap();
            c.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        quit_primary(&InstanceInfo { ui_port: port, token: "tok".into() }).unwrap();
        let req = t.join().unwrap();
        assert!(req.starts_with("POST /api/shell/quit HTTP/1.1"), "{req}");
        assert!(req.contains("x-nd-token: tok"));
    }

    #[test]
    fn quit_with_nothing_running_is_ok() {
        let dir = tmp("quitnone");
        let suffix = format!("quit-test-{}", std::process::id());
        assert_eq!(run_quit(&dir, Some(&suffix), Duration::from_millis(200)), 0);
    }

    #[cfg(unix)]
    #[test]
    fn quit_waits_for_the_lock_and_times_out() {
        let dir = tmp("quitwait");
        let suffix = format!("quit-wait-{}", std::process::id());
        let held = acquire(Some(&suffix)).unwrap().unwrap();
        // No instance.json: refuses, exit 1.
        assert_eq!(run_quit(&dir, Some(&suffix), Duration::from_millis(100)), 1);
        // The lock frees while we wait.
        let s2 = suffix.clone();
        let t = std::thread::spawn(move || wait_until_free(Some(&s2), Duration::from_secs(5)).unwrap());
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
        assert!(t.join().unwrap());
        // Held for the whole window: false.
        let held = acquire(Some(&suffix)).unwrap().unwrap();
        assert!(!wait_until_free(Some(&suffix), Duration::from_millis(250)).unwrap());
        drop(held);
    }
}
