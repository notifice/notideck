// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-app volume via WASAPI audio sessions (`IAudioSessionManager2`) on the default render device.
//!
//! A dedicated MTA worker thread owns every COM object; trait calls are requests over a channel
//! with a reply channel, so [`WasapiMixer`] is `Send + Sync`.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, STILL_ACTIVE, S_OK};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl,
    IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName};

use crate::{AppAudio, AudioSession, PlatformError, Result};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

enum Req {
    Sessions(Sender<Result<Vec<AudioSession>>>),
    SetVolume(String, f32, Sender<Result<()>>),
    SetMuted(String, bool, Sender<Result<()>>),
    Peaks(Vec<String>, Sender<Vec<Option<f32>>>),
    Icon(String, Sender<Option<Vec<u8>>>),
    Stop,
}

/// WASAPI session mixer. COM objects live on a dedicated MTA worker thread; trait calls are
/// requests over a channel, so the type is `Send + Sync`.
#[derive(Debug)]
pub struct WasapiMixer {
    tx: Mutex<Sender<Req>>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl WasapiMixer {
    /// Starts the worker and lists sessions once; error if that fails.
    pub fn open() -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Req>();
        let join = std::thread::Builder::new().name("nd-mixer".into()).spawn(move || worker(rx))?;
        let m = Self { tx: Mutex::new(tx), join: Mutex::new(Some(join)) };
        m.sessions()?;
        Ok(m)
    }

    fn call<T>(&self, make: impl FnOnce(Sender<T>) -> Req) -> Result<T> {
        let (rtx, rrx) = mpsc::channel();
        {
            let tx = self.tx.lock().map_err(|_| PlatformError::Other("mixer lock poisoned".into()))?;
            tx.send(make(rtx)).map_err(|_| PlatformError::Other("mixer worker stopped".into()))?;
        }
        rrx.recv_timeout(REQUEST_TIMEOUT).map_err(|_| PlatformError::Other("mixer worker timed out".into()))
    }
}

impl Drop for WasapiMixer {
    fn drop(&mut self) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(Req::Stop);
        }
        if let Some(j) = self.join.lock().ok().and_then(|mut j| j.take()) {
            let _ = j.join();
        }
    }
}

impl AppAudio for WasapiMixer {
    fn backend_name(&self) -> &'static str {
        "wasapi"
    }
    fn sessions(&self) -> Result<Vec<AudioSession>> {
        self.call(Req::Sessions)?
    }
    fn set_session_volume(&self, id: &str, volume: f32) -> Result<()> {
        let v = if volume.is_nan() { 0.0 } else { volume.clamp(0.0, 1.0) };
        self.call(|r| Req::SetVolume(id.to_string(), v, r))?
    }
    fn set_session_muted(&self, id: &str, muted: bool) -> Result<()> {
        self.call(|r| Req::SetMuted(id.to_string(), muted, r))?
    }
    fn has_peaks(&self) -> bool {
        true
    }
    fn peaks(&self, ids: &[String]) -> Result<Vec<Option<f32>>> {
        self.call(|r| Req::Peaks(ids.to_vec(), r))
    }
    fn app_icon(&self, session: &AudioSession) -> Result<Option<Vec<u8>>> {
        if session.is_system {
            return Ok(None);
        }
        let Some(path) = session.icon_hint.clone().filter(|p| !p.is_empty()) else {
            return Ok(None);
        };
        self.call(|r| Req::Icon(path, r))
    }
}

// ------------------------------------------------------------------------------ worker

struct Entry {
    volume: ISimpleAudioVolume,
    meter: Option<IAudioMeterInformation>,
}

#[derive(Default)]
struct State {
    device_id: String,
    cache: HashMap<String, Entry>,
}

fn worker(rx: Receiver<Req>) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut st = State::default();
    while let Ok(req) = rx.recv() {
        match req {
            Req::Sessions(r) => {
                let _ = r.send(st.enumerate());
            }
            Req::SetVolume(id, v, r) => {
                let _ = r.send(st.with_entry(&id, |e| {
                    unsafe { e.volume.SetMasterVolume(v, std::ptr::null()) }.map_err(|e| err("SetMasterVolume", e))
                }));
            }
            Req::SetMuted(id, m, r) => {
                let _ = r.send(st.with_entry(&id, |e| {
                    unsafe { e.volume.SetMute(m, std::ptr::null()) }.map_err(|e| err("SetMute", e))
                }));
            }
            Req::Peaks(ids, r) => {
                let out = ids
                    .iter()
                    .map(|id| {
                        let m = st.cache.get(id)?.meter.as_ref()?;
                        unsafe { m.GetPeakValue() }.ok().map(|p| p.clamp(0.0, 1.0))
                    })
                    .collect();
                let _ = r.send(out);
            }
            Req::Icon(path, r) => {
                let _ = r.send(extract_icon_png(&path));
            }
            Req::Stop => break,
        }
    }
    st.cache.clear();
    if com {
        unsafe { CoUninitialize() };
    }
}

fn err(what: &str, e: windows::core::Error) -> PlatformError {
    PlatformError::Other(format!("wasapi {what}: {e}"))
}

impl State {
    fn with_entry(&mut self, id: &str, f: impl Fn(&Entry) -> Result<()>) -> Result<()> {
        if let Some(e) = self.cache.get(id) {
            if f(e).is_ok() {
                return Ok(());
            }
        }
        // Unknown, or the cached session died / device changed: re-enumerate once.
        self.enumerate()?;
        match self.cache.get(id) {
            Some(e) => f(e),
            None => Err(PlatformError::Other("unknown session".into())),
        }
    }

    fn enumerate(&mut self) -> Result<Vec<AudioSession>> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| err("enumerator", e))?;
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) }
            .map_err(|e| err("default device", e))?;
        let dev_id = unsafe { device.GetId() }.map(take_pwstr).unwrap_or_default();
        if dev_id != self.device_id {
            self.cache.clear();
            self.device_id = dev_id;
        }
        let mgr: IAudioSessionManager2 =
            unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| err("session manager", e))?;
        let list = unsafe { mgr.GetSessionEnumerator() }.map_err(|e| err("GetSessionEnumerator", e))?;
        let count = unsafe { list.GetCount() }.map_err(|e| err("GetCount", e))?;
        let mut out = Vec::new();
        let mut cache = HashMap::new();
        for i in 0..count {
            let Ok(ctrl) = (unsafe { list.GetSession(i) }) else { continue };
            match read_session(&ctrl) {
                Ok(Some((s, entry))) => {
                    cache.insert(s.id.clone(), entry);
                    out.push(s);
                }
                Ok(None) => {}
                Err(e) => tracing::debug!("mixer: skipping session {i}: {e}"),
            }
        }
        self.cache = cache;
        Ok(out)
    }
}

fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s
}

fn read_session(ctrl: &IAudioSessionControl) -> windows::core::Result<Option<(AudioSession, Entry)>> {
    let state = unsafe { ctrl.GetState() }?;
    if state == AudioSessionStateExpired {
        return Ok(None);
    }
    let c2: IAudioSessionControl2 = ctrl.cast()?;
    let id = take_pwstr(unsafe { c2.GetSessionInstanceIdentifier() }?);
    if id.is_empty() {
        return Ok(None);
    }
    let pid = unsafe { c2.GetProcessId() }.ok().filter(|p| *p != 0);
    let is_system = unsafe { c2.IsSystemSoundsSession() } == S_OK;
    // WASAPI keeps an exited app's session around as inactive; list only apps that are still running.
    if !is_system && pid.is_some_and(|p| !process_alive(p)) {
        return Ok(None);
    }
    let (process, icon_hint) = match pid.filter(|_| !is_system).and_then(image_path) {
        Some(path) => {
            let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
            (name, Some(path))
        }
        None => (String::new(), None),
    };
    let display_name = if is_system {
        "System sounds".to_string()
    } else {
        let explicit = take_pwstr(unsafe { ctrl.GetDisplayName() }.unwrap_or(PWSTR::null()));
        if !explicit.trim().is_empty() && !explicit.starts_with('@') {
            explicit
        } else if let Some(d) = icon_hint.as_deref().and_then(file_description).filter(|d| !d.trim().is_empty()) {
            d
        } else if !process.is_empty() {
            process.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or_else(|| process.clone())
        } else {
            id.clone()
        }
    };
    let volume: ISimpleAudioVolume = ctrl.cast()?;
    let level = unsafe { volume.GetMasterVolume() }.unwrap_or(1.0);
    let muted = unsafe { volume.GetMute() }.map(|b| b.as_bool()).unwrap_or(false);
    let meter = ctrl.cast::<IAudioMeterInformation>().ok();
    let session = AudioSession {
        id,
        pid,
        process,
        display_name,
        volume: level.clamp(0.0, 1.0),
        muted,
        active: state == AudioSessionStateActive,
        is_system,
        icon_hint,
    };
    Ok(Some((session, Entry { volume, meter })))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// False only when the process is known to be gone; access-denied (elevated / protected) counts as alive.
fn process_alive(pid: u32) -> bool {
    unsafe {
        let h = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(e) => return e.code() == ERROR_ACCESS_DENIED.to_hresult(),
        };
        let mut code = 0u32;
        let r = GetExitCodeProcess(h, &mut code);
        let _ = CloseHandle(h);
        r.is_err() || code == STILL_ACTIVE.0 as u32
    }
}

fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// `FileDescription` from the exe's version resource (first translation).
fn file_description(path: &str) -> Option<String> {
    let p = wide(path);
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(p.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(p.as_ptr()), 0, size, data.as_mut_ptr() as *mut c_void).ok()?;
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let sub = wide(r"\VarFileInfo\Translation");
        if !VerQueryValueW(data.as_ptr() as *const c_void, PCWSTR(sub.as_ptr()), &mut ptr, &mut len).as_bool()
            || len < 4
            || ptr.is_null()
        {
            return None;
        }
        let t = std::slice::from_raw_parts(ptr as *const u16, 2);
        let key = wide(&format!("\\StringFileInfo\\{:04x}{:04x}\\FileDescription", t[0], t[1]));
        let mut sptr: *mut c_void = std::ptr::null_mut();
        let mut slen = 0u32;
        if !VerQueryValueW(data.as_ptr() as *const c_void, PCWSTR(key.as_ptr()), &mut sptr, &mut slen).as_bool()
            || slen == 0
            || sptr.is_null()
        {
            return None;
        }
        let chars = std::slice::from_raw_parts(sptr as *const u16, slen as usize);
        let end = chars.iter().position(|c| *c == 0).unwrap_or(chars.len());
        Some(String::from_utf16_lossy(&chars[..end]).trim().to_string())
    }
}

// ------------------------------------------------------------------------------ icons

fn extract_icon_png(path: &str) -> Option<Vec<u8>> {
    match try_icon(path) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("mixer: icon extraction for {path} failed: {e}");
            None
        }
    }
}

fn try_icon(path: &str) -> std::result::Result<Option<Vec<u8>>, String> {
    let p = wide(path);
    let f: IShellItemImageFactory = unsafe { SHCreateItemFromParsingName(PCWSTR(p.as_ptr()), None) }
        .map_err(|e| format!("SHCreateItemFromParsingName: {e}"))?;
    super::win_icon::factory_icon_png(&f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_key;

    #[test]
    fn app_key_cases() {
        assert_eq!(app_key(r"C:\x\Spotify.exe", false), "spotify");
        assert_eq!(app_key("firefox-bin", false), "firefox-bin");
        assert_eq!(app_key("Anything.exe", true), "system");
    }

    #[test]
    fn sessions_peaks_and_volume_roundtrip() {
        let Ok(m) = WasapiMixer::open() else {
            eprintln!("no audio device; skipping");
            return;
        };
        let sessions = m.sessions().unwrap();
        for s in &sessions {
            assert!(!s.id.is_empty());
        }
        let ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
        let peaks = m.peaks(&ids).unwrap();
        assert_eq!(peaks.len(), ids.len());
        for p in peaks {
            let p = p.expect("peak for listed id");
            assert!((0.0..=1.0).contains(&p));
        }
        assert_eq!(m.peaks(&["nope".into()]).unwrap(), vec![None]);
        assert!(m.set_session_volume("nope", 0.5).is_err());
        if let Some(s) = sessions.first() {
            let orig = s.volume;
            let target = if orig > 0.5 { orig - 0.1 } else { orig + 0.1 };
            m.set_session_volume(&s.id, target).unwrap();
            let after = m.sessions().unwrap().into_iter().find(|x| x.id == s.id).unwrap();
            m.set_session_volume(&s.id, orig).unwrap();
            assert!((after.volume - target).abs() < 0.02, "{} vs {target}", after.volume);
        }
        let t = std::time::Instant::now();
        for _ in 0..20 {
            let _ = m.peaks(&ids);
        }
        eprintln!("20 peaks calls: {:?}; {} sessions", t.elapsed(), sessions.len());
    }

    #[test]
    fn icon_for_current_exe_is_png() {
        let Ok(m) = WasapiMixer::open() else { return };
        let exe = std::env::current_exe().unwrap().display().to_string();
        let s = AudioSession { id: "x".into(), icon_hint: Some(exe), ..Default::default() };
        let png = m.app_icon(&s).unwrap().expect("icon");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let sys = AudioSession { is_system: true, ..Default::default() };
        assert!(m.app_icon(&sys).unwrap().is_none());
    }

    #[test]
    fn process_alive_self_and_exited_child() {
        assert!(process_alive(std::process::id()));
        let mut child = std::process::Command::new("cmd").args(["/C", "exit"]).spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        // The child's handle is still open here (exit code readable), so it must read as exited.
        assert!(!process_alive(pid));
        drop(child);
        assert!(!process_alive(pid));
    }
}
