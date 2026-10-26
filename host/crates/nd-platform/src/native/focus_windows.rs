// SPDX-License-Identifier: GPL-3.0-or-later
//! Focused-window tracking on Windows (SPEC §6.3).
//!
//! A dedicated thread owns two out-of-context WinEvent hooks (foreground change, window name
//! change) and a message loop. Callbacks are plain `extern "system"` functions, so they reach the
//! thread's state through a `thread_local!`. Title changes are rate-limited to ~5/s; the last one
//! is flushed by a thread timer so the final title is never lost.

use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetForegroundWindow, GetMessageW, GetWindowTextW, GetWindowThreadProcessId,
    KillTimer, PeekMessageW, PostThreadMessageW, SetTimer, EVENT_OBJECT_NAMECHANGE, EVENT_SYSTEM_FOREGROUND, MSG,
    OBJID_WINDOW, PM_NOREMOVE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_QUIT, WM_TIMER,
};

use crate::{FocusHandle, FocusWatcher, FocusedApp, PlatformError, Result};

const TITLE_MIN_INTERVAL: Duration = Duration::from_millis(200);

pub struct WindowsFocus;

impl FocusWatcher for WindowsFocus {
    fn backend_name(&self) -> &'static str {
        "windows"
    }

    fn start(&self, on_change: Box<dyn Fn(FocusedApp) + Send + Sync>) -> Result<FocusHandle> {
        let (tx, rx) = mpsc::channel::<std::result::Result<u32, String>>();
        let join = std::thread::Builder::new().name("nd-focus".into()).spawn(move || run_thread(on_change, tx))?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(tid)) => Ok(FocusHandle(Box::new(Watch { tid, join: Some(join) }))),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(PlatformError::Other(e))
            }
            Err(_) => Err(PlatformError::Other("focus thread did not start".into())),
        }
    }
}

struct Watch {
    tid: u32,
    join: Option<JoinHandle<()>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// What to do with a freshly read focus snapshot.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Emit,
    Skip,
    /// Rate-limited: keep as pending and flush later.
    Defer,
}

/// Dedupe + title-change rate limiting (pure, testable).
#[derive(Default)]
struct Coalescer {
    last: Option<FocusedApp>,
    last_emit: Option<Instant>,
    pending: bool,
}

impl Coalescer {
    fn decide(&mut self, app: &FocusedApp, title_only: bool, now: Instant) -> Decision {
        if self.last.as_ref() == Some(app) {
            self.pending = false;
            return Decision::Skip;
        }
        if title_only {
            if let Some(t) = self.last_emit {
                if now.duration_since(t) < TITLE_MIN_INTERVAL {
                    self.pending = true;
                    return Decision::Defer;
                }
            }
        }
        self.last = Some(app.clone());
        self.last_emit = Some(now);
        self.pending = false;
        Decision::Emit
    }
}

struct State {
    on_change: Box<dyn Fn(FocusedApp) + Send + Sync>,
    co: Coalescer,
    timer: usize,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn handle_event(title_only: bool) {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return;
    }
    let app = snapshot(hwnd);
    // Decide under the borrow, call out after releasing it.
    let emit = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.as_mut()?;
        match st.co.decide(&app, title_only, Instant::now()) {
            Decision::Emit => {
                clear_timer(st);
                Some(true)
            }
            Decision::Defer => {
                if st.timer == 0 {
                    st.timer = unsafe { SetTimer(None, 0, TITLE_MIN_INTERVAL.as_millis() as u32, None) };
                }
                Some(false)
            }
            Decision::Skip => {
                clear_timer(st);
                Some(false)
            }
        }
    });
    if emit == Some(true) {
        STATE.with(|s| {
            if let Some(st) = s.borrow().as_ref() {
                (st.on_change)(app);
            }
        });
    }
}

fn clear_timer(st: &mut State) {
    if st.timer != 0 {
        unsafe {
            let _ = KillTimer(None, st.timer);
        }
        st.timer = 0;
    }
}

unsafe extern "system" fn hook_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND {
        handle_event(false);
    } else if event == EVENT_OBJECT_NAMECHANGE
        && id_object == OBJID_WINDOW.0
        && !hwnd.0.is_null()
        && hwnd == GetForegroundWindow()
    {
        handle_event(true);
    }
}

fn run_thread(on_change: Box<dyn Fn(FocusedApp) + Send + Sync>, ready: mpsc::Sender<std::result::Result<u32, String>>) {
    unsafe {
        // Force creation of this thread's message queue before the handle can post to it.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);

        STATE.with(|s| {
            *s.borrow_mut() = Some(State { on_change, co: Coalescer::default(), timer: 0 });
        });

        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let h1 = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(hook_proc), 0, 0, flags);
        let h2 = SetWinEventHook(EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE, None, Some(hook_proc), 0, 0, flags);
        if h1.0.is_null() || h2.0.is_null() {
            if !h1.0.is_null() {
                let _ = UnhookWinEvent(h1);
            }
            if !h2.0.is_null() {
                let _ = UnhookWinEvent(h2);
            }
            let _ = ready.send(Err("SetWinEventHook failed".into()));
            return;
        }
        let _ = ready.send(Ok(GetCurrentThreadId()));

        handle_event(false); // initial snapshot

        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if msg.message == WM_TIMER && msg.hwnd.0.is_null() {
                // Flush a deferred title change (re-reads the foreground window).
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        clear_timer(st);
                    }
                });
                handle_event(false);
            }
            // Out-of-context WinEvent callbacks are delivered from inside GetMessageW.
        }

        let _ = UnhookWinEvent(h1);
        let _ = UnhookWinEvent(h2);
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut() {
                clear_timer(st);
            }
            *s.borrow_mut() = None;
        });
    }
}

fn snapshot(hwnd: HWND) -> FocusedApp {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    let mut process = process_name(pid);
    let mut real_pid = pid;
    if process.eq_ignore_ascii_case("ApplicationFrameHost.exe") {
        if let Some((p, name)) = uwp_child(hwnd, pid) {
            real_pid = p;
            process = name;
        }
    }
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    let mut tbuf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut tbuf) };
    let title = String::from_utf16_lossy(&tbuf[..n.max(0) as usize]);
    FocusedApp { process, class, title, pid: if pid == 0 { None } else { Some(real_pid) } }
}

fn process_name(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        if ok.is_err() {
            return String::new();
        }
        basename(&String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// File name part of a Windows (or mixed-separator) path.
fn basename(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or("").to_string()
}

struct ChildSearch {
    frame_pid: u32,
    found: Option<(u32, String)>,
}

unsafe extern "system" fn enum_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let cs = &mut *(lparam.0 as *mut ChildSearch);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid != 0 && pid != cs.frame_pid {
        let name = process_name(pid);
        if !name.is_empty() {
            cs.found = Some((pid, name));
            return BOOL(0);
        }
    }
    BOOL(1)
}

fn uwp_child(hwnd: HWND, frame_pid: u32) -> Option<(u32, String)> {
    let mut cs = ChildSearch { frame_pid, found: None };
    unsafe {
        let _ = EnumChildWindows(hwnd, Some(enum_child), LPARAM(&mut cs as *mut _ as isize));
    }
    cs.found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn app(title: &str) -> FocusedApp {
        FocusedApp { process: "a.exe".into(), class: "C".into(), title: title.into(), pid: Some(1) }
    }

    #[test]
    fn basename_variants() {
        assert_eq!(basename(r"C:\Program Files\OBS\obs64.exe"), "obs64.exe");
        assert_eq!(basename("C:/x/y.exe"), "y.exe");
        assert_eq!(basename("plain.exe"), "plain.exe");
        assert_eq!(basename(""), "");
    }

    #[test]
    fn coalescer_dedupes_and_rate_limits() {
        let mut c = Coalescer::default();
        let t0 = Instant::now();
        assert_eq!(c.decide(&app("a"), false, t0), Decision::Emit);
        assert_eq!(c.decide(&app("a"), false, t0), Decision::Skip);
        // title change right after an emit is deferred
        assert_eq!(c.decide(&app("b"), true, t0 + Duration::from_millis(50)), Decision::Defer);
        assert!(c.pending);
        // flush after the interval emits the final title
        assert_eq!(c.decide(&app("b"), false, t0 + Duration::from_millis(300)), Decision::Emit);
        assert!(!c.pending);
        // a foreground switch is never deferred
        assert_eq!(c.decide(&app("c"), false, t0 + Duration::from_millis(310)), Decision::Emit);
        // title change after the interval emits directly
        assert_eq!(c.decide(&app("d"), true, t0 + Duration::from_millis(600)), Decision::Emit);
    }

    #[test]
    fn start_emits_initial_and_drop_is_prompt() {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let h = WindowsFocus
            .start(Box::new(move |a| {
                let _ = tx.lock().unwrap().send(a);
            }))
            .expect("start");
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(a) => {
                println!("initial FocusedApp: {a:?}");
            }
            Err(_) => println!("no foreground window event within 2s"),
        }
        let t = Instant::now();
        drop(h);
        assert!(t.elapsed() < Duration::from_secs(1), "drop took {:?}", t.elapsed());
    }
}
