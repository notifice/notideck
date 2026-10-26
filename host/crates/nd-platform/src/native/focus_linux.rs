// SPDX-License-Identifier: GPL-3.0-or-later
//! Focused-window tracking on Linux (SPEC §6.3): Sway (i3-ipc), Hyprland (socket2), KWin (script + D-Bus, see
//! `focus_kwin`) and X11 (EWMH).
//!
//! All parsing is pure and unit-tested; the backends are thin I/O loops, each on its own thread.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use crate::{FocusHandle, FocusWatcher, FocusedApp, PlatformError, Result};

pub(super) type Callback = Box<dyn Fn(FocusedApp) + Send + Sync>;

// ---------------------------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Choice {
    Sway(PathBuf),
    Hyprland { sig: String, runtime_dir: Option<String> },
    Kwin,
    X11,
    None(&'static str),
}

fn non_empty(env: &dyn Fn(&str) -> Option<String>, k: &str) -> Option<String> {
    env(k).filter(|v| !v.is_empty())
}

/// Pure backend choice over an environment lookup.
fn choose(env: &dyn Fn(&str) -> Option<String>) -> Choice {
    if let Some(s) = non_empty(env, "SWAYSOCK") {
        return Choice::Sway(PathBuf::from(s));
    }
    if let Some(sig) = non_empty(env, "HYPRLAND_INSTANCE_SIGNATURE") {
        return Choice::Hyprland { sig, runtime_dir: non_empty(env, "XDG_RUNTIME_DIR") };
    }
    let wayland = non_empty(env, "XDG_SESSION_TYPE").is_some_and(|t| t.eq_ignore_ascii_case("wayland"));
    if wayland && is_kde(env) {
        return Choice::Kwin;
    }
    if wayland {
        return Choice::None("Wayland session without Sway/Hyprland: X11 would only see XWayland windows");
    }
    if non_empty(env, "DISPLAY").is_some() {
        return Choice::X11;
    }
    Choice::None("no display server detected")
}

/// `XDG_CURRENT_DESKTOP` is a `:`-separated list containing `KDE` (case-insensitive).
fn is_kde(env: &dyn Fn(&str) -> Option<String>) -> bool {
    non_empty(env, "XDG_CURRENT_DESKTOP").is_some_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("KDE")))
}

/// Picks Sway (`$SWAYSOCK`), then Hyprland (`$HYPRLAND_INSTANCE_SIGNATURE`), then KWin (Wayland + KDE), then X11
/// (`$DISPLAY`), else `NullFocus`.
pub fn select() -> Box<dyn FocusWatcher> {
    match choose(&|k| std::env::var(k).ok()) {
        Choice::Sway(p) => Box::new(LinuxFocus { kind: Kind::Sway(p) }),
        Choice::Hyprland { sig, runtime_dir } => Box::new(LinuxFocus { kind: Kind::Hyprland { sig, runtime_dir } }),
        Choice::Kwin => Box::new(LinuxFocus { kind: Kind::Kwin }),
        Choice::X11 => Box::new(LinuxFocus { kind: Kind::X11 }),
        Choice::None(why) => {
            tracing::info!("focus tracking unavailable: {why}");
            Box::new(crate::NullFocus)
        }
    }
}

enum Kind {
    Sway(PathBuf),
    Hyprland { sig: String, runtime_dir: Option<String> },
    Kwin,
    X11,
}

struct LinuxFocus {
    kind: Kind,
}

impl FocusWatcher for LinuxFocus {
    fn backend_name(&self) -> &'static str {
        match self.kind {
            Kind::Sway(_) => "sway",
            Kind::Hyprland { .. } => "hyprland",
            Kind::Kwin => "kwin",
            Kind::X11 => "x11",
        }
    }

    fn start(&self, on_change: Callback) -> Result<FocusHandle> {
        let proc_root = PathBuf::from("/proc");
        if matches!(self.kind, Kind::Kwin) {
            return super::focus_kwin::start(proc_root, on_change);
        }
        let guard = match &self.kind {
            Kind::Sway(p) => start_sway(p, proc_root, on_change)?,
            Kind::Hyprland { sig, runtime_dir } => {
                let dir = hypr_dir(sig, runtime_dir.as_deref(), &|p| p.exists());
                start_hyprland(&dir, proc_root, on_change)?
            }
            Kind::X11 => start_x11(proc_root, on_change)?,
            Kind::Kwin => unreachable!("handled above"),
        };
        Ok(FocusHandle(Box::new(guard)))
    }
}

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/// Calls the callback only when the value differs from the previous one.
pub(super) struct Emitter {
    cb: Callback,
    last: Option<FocusedApp>,
}

impl Emitter {
    pub(super) fn new(cb: Callback) -> Self {
        Self { cb, last: None }
    }
    pub(super) fn emit(&mut self, app: FocusedApp) {
        if self.last.as_ref() != Some(&app) {
            self.last = Some(app.clone());
            (self.cb)(app);
        }
    }
}

/// Stops a backend thread on drop: raises the flag, shuts the socket down, joins.
struct Guard {
    stop: Arc<AtomicBool>,
    sock: Option<UnixStream>,
    join: Option<JoinHandle<()>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(s) = &self.sock {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        if let Some(j) = self.join.take() {
            if j.thread().id() != std::thread::current().id() {
                let _ = j.join();
            }
        }
    }
}

/// `/proc/<pid>/comm`, trimmed; empty when unreadable.
fn proc_comm(proc_root: &Path, pid: u32) -> String {
    std::fs::read_to_string(proc_root.join(pid.to_string()).join("comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub(super) fn make_app(proc_root: &Path, class: String, title: String, pid: Option<u32>) -> FocusedApp {
    let process = pid.map(|p| proc_comm(proc_root, p)).unwrap_or_default();
    FocusedApp { process, class, title, pid }
}

fn json_pid(v: &Value) -> Option<u32> {
    v.as_i64().filter(|p| *p > 0).and_then(|p| u32::try_from(p).ok())
}

fn json_str(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

// ---------------------------------------------------------------------------------------------
// Sway (i3-ipc)
// ---------------------------------------------------------------------------------------------

const IPC_MAGIC: &[u8; 6] = b"i3-ipc";
const IPC_SUBSCRIBE: u32 = 2;
const IPC_GET_TREE: u32 = 4;
const IPC_EVENT_WINDOW: u32 = 0x8000_0003;

fn encode_frame(ty: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(14 + payload.len());
    v.extend_from_slice(IPC_MAGIC);
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(&ty.to_le_bytes());
    v.extend_from_slice(payload);
    v
}

fn read_frame(r: &mut impl Read) -> io::Result<(u32, Vec<u8>)> {
    let mut head = [0u8; 14];
    r.read_exact(&mut head)?;
    if &head[..6] != IPC_MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad i3-ipc magic"));
    }
    let len = u32::from_le_bytes(head[6..10].try_into().unwrap()) as usize;
    let ty = u32::from_le_bytes(head[10..14].try_into().unwrap());
    if len > 64 << 20 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "i3-ipc frame too large"));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok((ty, payload))
}

/// Depth-first search for the focused node (through `nodes` and `floating_nodes`).
fn sway_find_focused(node: &Value) -> Option<&Value> {
    if node["focused"].as_bool() == Some(true) {
        return Some(node);
    }
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node[key].as_array() {
            for c in children {
                if let Some(f) = sway_find_focused(c) {
                    return Some(f);
                }
            }
        }
    }
    None
}

fn sway_node_to_app(node: &Value, proc_root: &Path) -> FocusedApp {
    let mut class = json_str(&node["app_id"]);
    if class.is_empty() {
        class = json_str(&node["window_properties"]["class"]);
    }
    make_app(proc_root, class, json_str(&node["name"]), json_pid(&node["pid"]))
}

/// Result of interpreting one i3-ipc window event.
fn sway_window_event(payload: &[u8], proc_root: &Path) -> Option<FocusedApp> {
    let v: Value = serde_json::from_slice(payload).ok()?;
    let c = &v["container"];
    match v["change"].as_str()? {
        "focus" => Some(sway_node_to_app(c, proc_root)),
        "title" if c["focused"].as_bool() == Some(true) => Some(sway_node_to_app(c, proc_root)),
        _ => None,
    }
}

fn sway_tree_focused(payload: &[u8], proc_root: &Path) -> Option<FocusedApp> {
    let v: Value = serde_json::from_slice(payload).ok()?;
    Some(sway_find_focused(&v).map(|n| sway_node_to_app(n, proc_root)).unwrap_or_default())
}

fn start_sway(path: &Path, proc_root: PathBuf, cb: Callback) -> Result<Guard> {
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(&encode_frame(IPC_SUBSCRIBE, br#"["window"]"#))?;
    stream.write_all(&encode_frame(IPC_GET_TREE, b""))?;
    let sock = stream.try_clone()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let join = std::thread::Builder::new().name("nd-focus-sway".into()).spawn(move || {
        let mut em = Emitter::new(cb);
        while !stop2.load(Ordering::SeqCst) {
            let (ty, payload) = match read_frame(&mut stream) {
                Ok(f) => f,
                Err(e) => {
                    if !stop2.load(Ordering::SeqCst) {
                        tracing::warn!("sway focus watcher ended: {e}");
                    }
                    break;
                }
            };
            let app = match ty {
                IPC_GET_TREE => sway_tree_focused(&payload, &proc_root),
                IPC_EVENT_WINDOW => sway_window_event(&payload, &proc_root),
                IPC_SUBSCRIBE => {
                    if !String::from_utf8_lossy(&payload).contains("true") {
                        tracing::warn!("sway refused window subscription");
                    }
                    None
                }
                _ => None,
            };
            if let Some(a) = app {
                em.emit(a);
            }
        }
    })?;
    Ok(Guard { stop, sock: Some(sock), join: Some(join) })
}

// ---------------------------------------------------------------------------------------------
// Hyprland
// ---------------------------------------------------------------------------------------------

/// Instance directory: `$XDG_RUNTIME_DIR/hypr/<sig>` if it exists, else the older `/tmp/hypr/<sig>`.
fn hypr_dir(sig: &str, runtime_dir: Option<&str>, exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    if let Some(rt) = runtime_dir.filter(|r| !r.is_empty()) {
        let p = Path::new(rt).join("hypr").join(sig);
        if exists(&p) {
            return p;
        }
    }
    let old = Path::new("/tmp/hypr").join(sig);
    match runtime_dir.filter(|r| !r.is_empty()) {
        Some(rt) if !exists(&old) => Path::new(rt).join("hypr").join(sig),
        _ => old,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum HyprEvent {
    /// `activewindow>>class,title` (title may itself contain commas).
    Active { class: String, title: String },
    /// `activewindowv2>>address`
    ActiveV2,
}

fn parse_hypr_line(line: &str) -> Option<HyprEvent> {
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(rest) = line.strip_prefix("activewindowv2>>") {
        let _ = rest;
        return Some(HyprEvent::ActiveV2);
    }
    let rest = line.strip_prefix("activewindow>>")?;
    let (class, title) = rest.split_once(',').unwrap_or((rest, ""));
    Some(HyprEvent::Active { class: class.to_string(), title: title.to_string() })
}

/// Parses the `j/activewindow` reply. `{}` (no window) gives the default value.
fn parse_hypr_active_json(bytes: &[u8], proc_root: &Path) -> Option<FocusedApp> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    if v["class"].is_null() && v["title"].is_null() {
        return Some(FocusedApp::default());
    }
    Some(make_app(proc_root, json_str(&v["class"]), json_str(&v["title"]), json_pid(&v["pid"])))
}

fn hypr_query(cmd_sock: &Path, proc_root: &Path) -> Option<FocusedApp> {
    let mut s = UnixStream::connect(cmd_sock).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"j/activewindow").ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).ok()?;
    parse_hypr_active_json(&buf, proc_root)
}

fn start_hyprland(dir: &Path, proc_root: PathBuf, cb: Callback) -> Result<Guard> {
    let events = UnixStream::connect(dir.join(".socket2.sock"))?;
    let sock = events.try_clone()?;
    let cmd = dir.join(".socket.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let join = std::thread::Builder::new().name("nd-focus-hypr".into()).spawn(move || {
        let mut em = Emitter::new(cb);
        if let Some(a) = hypr_query(&cmd, &proc_root) {
            em.emit(a);
        }
        let mut reader = BufReader::new(events);
        let mut line = String::new();
        while !stop2.load(Ordering::SeqCst) {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            match parse_hypr_line(&line) {
                Some(HyprEvent::Active { class, title }) => {
                    let app = hypr_query(&cmd, &proc_root).unwrap_or_else(|| FocusedApp {
                        class,
                        title,
                        ..Default::default()
                    });
                    em.emit(app);
                }
                Some(HyprEvent::ActiveV2) => {
                    if let Some(a) = hypr_query(&cmd, &proc_root) {
                        em.emit(a);
                    }
                }
                None => {}
            }
        }
    })?;
    Ok(Guard { stop, sock: Some(sock), join: Some(join) })
}

// ---------------------------------------------------------------------------------------------
// X11
// ---------------------------------------------------------------------------------------------

/// `WM_CLASS` is `instance\0class\0`; we want the class (second) part, falling back to the first.
fn split_wm_class(raw: &[u8]) -> String {
    let mut parts = raw.split(|b| *b == 0).filter(|p| !p.is_empty());
    let first = parts.next();
    let second = parts.next();
    String::from_utf8_lossy(second.or(first).unwrap_or_default()).into_owned()
}

mod x11 {
    use super::*;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt, EventMask, Window};
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;

    struct Atoms {
        active: Atom,
        net_wm_name: Atom,
        utf8: Atom,
        pid: Atom,
    }

    fn atom(c: &RustConnection, name: &[u8]) -> Result<Atom> {
        c.intern_atom(false, name)
            .map_err(|e| PlatformError::Other(e.to_string()))?
            .reply()
            .map(|r| r.atom)
            .map_err(|e| PlatformError::Other(e.to_string()))
    }

    fn prop(c: &RustConnection, w: Window, p: Atom, ty: Atom) -> Option<Vec<u8>> {
        let r = c.get_property(false, w, p, ty, 0, 4096).ok()?.reply().ok()?;
        (r.format != 0).then_some(r.value)
    }

    fn active_window(c: &RustConnection, root: Window, a: &Atoms) -> Window {
        let Some(r) = c.get_property(false, root, a.active, AtomEnum::WINDOW, 0, 1).ok().and_then(|k| k.reply().ok())
        else {
            return 0;
        };
        r.value32().and_then(|mut v| v.next()).unwrap_or(0)
    }

    fn read_app(c: &RustConnection, w: Window, a: &Atoms, proc_root: &Path) -> FocusedApp {
        if w == 0 {
            return FocusedApp::default();
        }
        let class = prop(c, w, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into())
            .map(|v| split_wm_class(&v))
            .unwrap_or_default();
        let title = prop(c, w, a.net_wm_name, a.utf8)
            .or_else(|| prop(c, w, AtomEnum::WM_NAME.into(), AtomEnum::ANY.into()))
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .unwrap_or_default();
        let pid = c
            .get_property(false, w, a.pid, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|k| k.reply().ok())
            .and_then(|r| r.value32().and_then(|mut v| v.next()))
            .filter(|p| *p > 0);
        make_app(proc_root, class, title, pid)
    }

    fn watch(c: &RustConnection, w: Window, on: bool) {
        if w == 0 {
            return;
        }
        let mask = if on { EventMask::PROPERTY_CHANGE } else { EventMask::NO_EVENT };
        let _ = c.change_window_attributes(w, &ChangeWindowAttributesAux::new().event_mask(mask));
        let _ = c.flush();
    }

    pub fn start(proc_root: PathBuf, cb: Callback) -> Result<Guard> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| PlatformError::Other(format!("X11 connect: {e}")))?;
        let root = conn.setup().roots[screen].root;
        let atoms = Atoms {
            active: atom(&conn, b"_NET_ACTIVE_WINDOW")?,
            net_wm_name: atom(&conn, b"_NET_WM_NAME")?,
            utf8: atom(&conn, b"UTF8_STRING")?,
            pid: atom(&conn, b"_NET_WM_PID")?,
        };
        conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE))
            .map_err(|e| PlatformError::Other(e.to_string()))?;
        conn.flush().map_err(|e| PlatformError::Other(e.to_string()))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let join = std::thread::Builder::new().name("nd-focus-x11".into()).spawn(move || {
            let mut em = Emitter::new(cb);
            let mut current = active_window(&conn, root, &atoms);
            watch(&conn, current, true);
            em.emit(read_app(&conn, current, &atoms, &proc_root));
            while !stop2.load(Ordering::SeqCst) {
                let ev = match conn.poll_for_event() {
                    Ok(Some(ev)) => ev,
                    Ok(None) => {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("X11 focus watcher ended: {e}");
                        break;
                    }
                };
                let Event::PropertyNotify(p) = ev else { continue };
                if p.window == root && p.atom == atoms.active {
                    let next = active_window(&conn, root, &atoms);
                    if next != current {
                        watch(&conn, current, false);
                        current = next;
                        watch(&conn, current, true);
                    }
                    em.emit(read_app(&conn, current, &atoms, &proc_root));
                } else if p.window == current && (p.atom == atoms.net_wm_name || p.atom == u32::from(AtomEnum::WM_NAME))
                {
                    em.emit(read_app(&conn, current, &atoms, &proc_root));
                }
            }
        })?;
        Ok(Guard { stop, sock: None, join: Some(join) })
    }
}

fn start_x11(proc_root: PathBuf, cb: Callback) -> Result<Guard> {
    x11::start(proc_root, cb)
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nd-focus-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn selection_order() {
        assert_eq!(choose(&env_of(&[("SWAYSOCK", "/s"), ("DISPLAY", ":0")])), Choice::Sway("/s".into()));
        assert!(matches!(
            choose(&env_of(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc"), ("DISPLAY", ":0")])),
            Choice::Hyprland { .. }
        ));
        assert_eq!(choose(&env_of(&[("DISPLAY", ":0")])), Choice::X11);
        assert_eq!(choose(&env_of(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "x11")])), Choice::X11);
        assert!(matches!(choose(&env_of(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "wayland")])), Choice::None(_)));
        let kde = |desktop: &'static str, extra: &'static [(&'static str, &'static str)]| {
            let mut v = vec![("XDG_SESSION_TYPE", "wayland"), ("XDG_CURRENT_DESKTOP", desktop), ("DISPLAY", ":0")];
            v.extend_from_slice(extra);
            choose(&env_of(&v))
        };
        assert_eq!(kde("KDE", &[]), Choice::Kwin);
        assert_eq!(kde("plasma:kde", &[]), Choice::Kwin);
        assert!(matches!(kde("GNOME", &[]), Choice::None(_)));
        assert!(matches!(kde("KDE", &[("SWAYSOCK", "/s")]), Choice::Sway(_)));
        assert!(matches!(kde("KDE", &[("HYPRLAND_INSTANCE_SIGNATURE", "x")]), Choice::Hyprland { .. }));
        // X11 session on KDE stays on the EWMH backend.
        assert_eq!(
            choose(&env_of(&[("XDG_SESSION_TYPE", "x11"), ("XDG_CURRENT_DESKTOP", "KDE"), ("DISPLAY", ":0")])),
            Choice::X11
        );
        assert!(matches!(choose(&env_of(&[])), Choice::None(_)));
        assert!(matches!(choose(&env_of(&[("SWAYSOCK", "")])), Choice::None(_)));
    }

    #[test]
    fn frame_roundtrip() {
        let f = encode_frame(4, b"hello");
        assert_eq!(&f[..6], b"i3-ipc");
        assert_eq!(&f[6..10], &5u32.to_le_bytes());
        assert_eq!(&f[10..14], &4u32.to_le_bytes());
        let (ty, p) = read_frame(&mut io::Cursor::new(f)).unwrap();
        assert_eq!((ty, p.as_slice()), (4, &b"hello"[..]));
        let mut bad = encode_frame(1, b"");
        bad[0] = b'x';
        assert!(read_frame(&mut io::Cursor::new(bad)).is_err());
        assert!(read_frame(&mut io::Cursor::new(vec![1, 2, 3])).is_err());
    }

    const TREE: &str = r#"{"focused":false,"nodes":[
        {"focused":false,"type":"output","nodes":[
          {"focused":false,"type":"workspace","nodes":[
            {"focused":false,"app_id":"foot","name":"term","pid":1}],
           "floating_nodes":[
            {"focused":true,"app_id":null,"name":"Float \"win\"","pid":4242,
             "window_properties":{"class":"Steam","title":"x"}}]}]}],
        "floating_nodes":[]}"#;

    #[test]
    fn sway_tree_nested_and_floating() {
        let dir = tmpdir("tree");
        let app = sway_tree_focused(TREE.as_bytes(), &dir).unwrap();
        assert_eq!(app.class, "Steam");
        assert_eq!(app.title, "Float \"win\"");
        assert_eq!(app.pid, Some(4242));
        assert_eq!(app.process, "");
        // Nothing focused -> default.
        assert_eq!(sway_tree_focused(br#"{"focused":false,"nodes":[]}"#, &dir), Some(FocusedApp::default()));
        assert_eq!(sway_tree_focused(b"nope", &dir), None);
    }

    #[test]
    fn sway_events() {
        let dir = tmpdir("ev");
        std::fs::create_dir_all(dir.join("77")).unwrap();
        std::fs::write(dir.join("77/comm"), "firefox\n").unwrap();
        let focus = br#"{"change":"focus","container":{"focused":true,"app_id":"firefox","name":"Tab","pid":77}}"#;
        let a = sway_window_event(focus, &dir).unwrap();
        assert_eq!((a.class.as_str(), a.process.as_str(), a.pid), ("firefox", "firefox", Some(77)));
        let title = br#"{"change":"title","container":{"focused":true,"app_id":"firefox","name":"New","pid":77}}"#;
        assert_eq!(sway_window_event(title, &dir).unwrap().title, "New");
        let bg = br#"{"change":"title","container":{"focused":false,"app_id":"x","name":"bg"}}"#;
        assert_eq!(sway_window_event(bg, &dir), None);
        let close = br#"{"change":"close","container":{"focused":false}}"#;
        assert_eq!(sway_window_event(close, &dir), None);
    }

    #[test]
    fn hypr_lines() {
        assert_eq!(
            parse_hypr_line("activewindow>>class,title,with,commas\n"),
            Some(HyprEvent::Active { class: "class".into(), title: "title,with,commas".into() })
        );
        assert_eq!(parse_hypr_line("activewindow>>,"), Some(HyprEvent::Active { class: "".into(), title: "".into() }));
        assert_eq!(parse_hypr_line("activewindowv2>>55a1b2"), Some(HyprEvent::ActiveV2));
        assert_eq!(parse_hypr_line("workspace>>2"), None);
    }

    #[test]
    fn hypr_json_and_dir() {
        let dir = tmpdir("hypr");
        let a = parse_hypr_active_json(br#"{"address":"0x1","class":"kitty","title":"zsh, ~","pid":9}"#, &dir).unwrap();
        assert_eq!((a.class.as_str(), a.title.as_str(), a.pid), ("kitty", "zsh, ~", Some(9)));
        assert_eq!(parse_hypr_active_json(b"{}", &dir), Some(FocusedApp::default()));
        assert_eq!(parse_hypr_active_json(br#"{"class":"a","title":"b","pid":-1}"#, &dir).unwrap().pid, None);
        assert_eq!(parse_hypr_active_json(b"", &dir), None);

        let new = Path::new("/run/user/1/hypr/SIG").to_path_buf();
        assert_eq!(hypr_dir("SIG", Some("/run/user/1"), &|p| p == new), new);
        assert_eq!(
            hypr_dir("SIG", Some("/run/user/1"), &|p| p == Path::new("/tmp/hypr/SIG")),
            Path::new("/tmp/hypr/SIG")
        );
        assert_eq!(hypr_dir("SIG", Some("/run/user/1"), &|_| false), new);
        assert_eq!(hypr_dir("SIG", None, &|_| false), Path::new("/tmp/hypr/SIG"));
    }

    #[test]
    fn proc_comm_reads_and_trims() {
        let dir = tmpdir("proc");
        std::fs::create_dir_all(dir.join("5")).unwrap();
        std::fs::write(dir.join("5/comm"), "obs\n").unwrap();
        assert_eq!(proc_comm(&dir, 5), "obs");
        assert_eq!(proc_comm(&dir, 6), "");
    }

    #[test]
    fn wm_class() {
        assert_eq!(split_wm_class(b"navigator\0Firefox\0"), "Firefox");
        assert_eq!(split_wm_class(b"only\0"), "only");
        assert_eq!(split_wm_class(b""), "");
    }

    #[test]
    fn emitter_dedupes() {
        let (tx, rx) = mpsc::channel();
        let mut em = Emitter::new(Box::new(move |a| tx.send(a).unwrap()));
        let a = FocusedApp { class: "a".into(), ..Default::default() };
        em.emit(a.clone());
        em.emit(a.clone());
        em.emit(FocusedApp::default());
        em.emit(a);
        assert_eq!(rx.try_iter().count(), 3);
    }

    #[test]
    fn sway_fake_server_integration() {
        let dir = tmpdir("sway");
        let sock = dir.join("sway.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let (t1, p1) = read_frame(&mut s).unwrap();
            assert_eq!((t1, p1.as_slice()), (IPC_SUBSCRIBE, &br#"["window"]"#[..]));
            s.write_all(&encode_frame(IPC_SUBSCRIBE, br#"{"success":true}"#)).unwrap();
            let (t2, _) = read_frame(&mut s).unwrap();
            assert_eq!(t2, IPC_GET_TREE);
            s.write_all(&encode_frame(IPC_GET_TREE, TREE.as_bytes())).unwrap();
            let ev = br#"{"change":"focus","container":{"focused":true,"app_id":"foot","name":"term","pid":1}}"#;
            s.write_all(&encode_frame(IPC_EVENT_WINDOW, ev)).unwrap();
            // Duplicate must be swallowed.
            s.write_all(&encode_frame(IPC_EVENT_WINDOW, ev)).unwrap();
            let ev2 = br#"{"change":"title","container":{"focused":true,"app_id":"foot","name":"term2","pid":1}}"#;
            s.write_all(&encode_frame(IPC_EVENT_WINDOW, ev2)).unwrap();
            // Wait for the client to disconnect.
            let mut b = [0u8; 1];
            let _ = s.read(&mut b);
        });

        let (tx, rx) = mpsc::channel();
        let guard = start_sway(&sock, dir.clone(), Box::new(move |a| tx.send(a).unwrap())).unwrap();
        let t = Duration::from_secs(5);
        assert_eq!(rx.recv_timeout(t).unwrap().class, "Steam");
        assert_eq!(rx.recv_timeout(t).unwrap().title, "term");
        assert_eq!(rx.recv_timeout(t).unwrap().title, "term2");
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
        let started = std::time::Instant::now();
        drop(guard);
        assert!(started.elapsed() < Duration::from_secs(2));
        server.join().unwrap();
    }

    #[test]
    fn sway_connect_failure_is_error() {
        let dir = tmpdir("nosock");
        assert!(start_sway(&dir.join("missing"), dir.clone(), Box::new(|_| {})).is_err());
    }
}
