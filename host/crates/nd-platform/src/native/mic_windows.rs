// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows phone-mic output (round 25): shared-mode WASAPI into a virtual cable (VB-Cable's
//! "CABLE Input") plus the default-device guard helpers (`IPolicyConfig`).
//!
//! COM objects are not `Send`, so every open stream runs a playback thread that owns them and is
//! fed through a bounded ring buffer.

#![allow(non_snake_case)]

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::{GUID, HRESULT, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::{PKEY_DeviceInterface_FriendlyName, PKEY_Device_FriendlyName};
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, eMultimedia, eRender, EDataFlow, ERole, IAudioClient, IAudioRenderClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
    WAVE_FORMAT_PCM,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;

use crate::{
    is_virtual_cable_name, AudioEndpoint, DefaultDevices, DeviceFlow, DeviceRole, MicOutputDevice, MicSink, MicStream,
    PlatformError, Result,
};

fn win_err(what: &str, e: windows::core::Error) -> PlatformError {
    PlatformError::Other(format!("{what}: {e}"))
}

/// `CoInitializeEx(MTA)` for the current thread; tolerates an already-set apartment model.
struct ComGuard(bool);

impl ComGuard {
    fn new() -> Self {
        Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

// --- IPolicyConfig (undocumented, stable since Windows 7) -------------------------------------

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Only `SetDefaultEndpoint` is called; the earlier vtable slots are declared (with loose
/// signatures) so it lands on the right slot.
#[windows::core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: windows::core::IUnknown {
    unsafe fn GetMixFormat(&self, id: PCWSTR, fmt: *mut *mut c_void) -> HRESULT;
    unsafe fn GetDeviceFormat(&self, id: PCWSTR, default: BOOL, fmt: *mut *mut c_void) -> HRESULT;
    unsafe fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
    unsafe fn SetDeviceFormat(&self, id: PCWSTR, endpoint_fmt: *mut c_void, mix_fmt: *mut c_void) -> HRESULT;
    unsafe fn GetProcessingPeriod(&self, id: PCWSTR, default: BOOL, default_period: *mut i64, min: *mut i64)
        -> HRESULT;
    unsafe fn SetProcessingPeriod(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
    unsafe fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    unsafe fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    unsafe fn GetPropertyValue(&self, id: PCWSTR, fx: BOOL, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
    unsafe fn SetPropertyValue(&self, id: PCWSTR, fx: BOOL, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
    unsafe fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
    unsafe fn SetEndpointVisibility(&self, id: PCWSTR, visible: BOOL) -> HRESULT;
}

// --- device helpers ---------------------------------------------------------------------------

fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| win_err("MMDeviceEnumerator", e))
}

fn device_id(dev: &IMMDevice) -> Result<String> {
    unsafe {
        let p = dev.GetId().map_err(|e| win_err("GetId", e))?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.as_ptr() as *const c_void));
        Ok(s)
    }
}

fn device_string(dev: &IMMDevice, key: &PROPERTYKEY) -> String {
    unsafe {
        let Ok(store) = dev.OpenPropertyStore(STGM_READ) else { return String::new() };
        let Ok(pv) = store.GetValue(key) else { return String::new() };
        // PROPVARIANT's Drop clears it.
        windows::core::BSTR::try_from(&pv).map(|b| b.to_string()).unwrap_or_default()
    }
}

struct Ep {
    id: String,
    name: String,
    adapter: String,
    flow: DeviceFlow,
}

impl Ep {
    fn is_cable(&self) -> bool {
        is_virtual_cable_name(&self.name) || is_virtual_cable_name(&self.adapter)
    }
}

fn list_endpoints(en: &IMMDeviceEnumerator, flow: DeviceFlow) -> Result<Vec<(Ep, IMMDevice)>> {
    let df: EDataFlow = if flow == DeviceFlow::Render { eRender } else { eCapture };
    let col =
        unsafe { en.EnumAudioEndpoints(df, DEVICE_STATE_ACTIVE) }.map_err(|e| win_err("EnumAudioEndpoints", e))?;
    let n = unsafe { col.GetCount() }.map_err(|e| win_err("GetCount", e))?;
    let mut out = Vec::new();
    for i in 0..n {
        let Ok(dev) = (unsafe { col.Item(i) }) else { continue };
        let Ok(id) = device_id(&dev) else { continue };
        let name = device_string(&dev, &PKEY_Device_FriendlyName);
        let adapter = device_string(&dev, &PKEY_DeviceInterface_FriendlyName);
        out.push((Ep { id, name, adapter, flow }, dev));
    }
    Ok(out)
}

fn all_endpoints() -> Result<Vec<Ep>> {
    let _com = ComGuard::new();
    let en = enumerator()?;
    let mut v: Vec<Ep> = list_endpoints(&en, DeviceFlow::Render)?.into_iter().map(|(e, _)| e).collect();
    v.extend(list_endpoints(&en, DeviceFlow::Capture)?.into_iter().map(|(e, _)| e));
    Ok(v)
}

/// Render devices as the UI lists them: cables first, then the rest by name.
fn render_devices(eps: &[Ep]) -> Vec<MicOutputDevice> {
    let mut out: Vec<MicOutputDevice> = eps
        .iter()
        .filter(|e| e.flow == DeviceFlow::Render)
        .map(|e| {
            let cable = e.is_cable();
            let capture_name = if cable {
                eps.iter()
                    .find(|c| c.flow == DeviceFlow::Capture && !e.adapter.is_empty() && c.adapter == e.adapter)
                    .map(|c| c.name.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            MicOutputDevice { id: e.id.clone(), name: e.name.clone(), virtual_cable: cable, capture_name }
        })
        .collect();
    out.sort_by(|a, b| {
        b.virtual_cable.cmp(&a.virtual_cable).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

fn default_id(en: &IMMDeviceEnumerator, flow: EDataFlow, role: ERole) -> String {
    unsafe { en.GetDefaultAudioEndpoint(flow, role) }.ok().and_then(|d| device_id(&d).ok()).unwrap_or_default()
}

// --- the sink ---------------------------------------------------------------------------------

pub struct WasapiMicSink {}

impl WasapiMicSink {
    pub fn new() -> Self {
        WasapiMicSink {}
    }
}

impl Default for WasapiMicSink {
    fn default() -> Self {
        Self::new()
    }
}

impl MicSink for WasapiMicSink {
    fn backend_name(&self) -> &'static str {
        "wasapi"
    }

    fn devices(&self) -> Result<Vec<MicOutputDevice>> {
        Ok(render_devices(&all_endpoints()?))
    }

    fn open(&self, device: &str, sample_rate: u32) -> Result<Box<dyn MicStream>> {
        let devices = self.devices()?;
        let chosen = if device.is_empty() {
            devices
                .iter()
                .find(|d| d.virtual_cable)
                .ok_or_else(|| PlatformError::Other("no virtual cable found".into()))?
        } else {
            devices
                .iter()
                .find(|d| d.id == device)
                .ok_or_else(|| PlatformError::Other(format!("unknown output device '{device}'")))?
        };
        WasapiStream::start(chosen.id.clone(), sample_rate).map(|s| Box::new(s) as Box<dyn MicStream>)
    }

    fn defaults(&self) -> Result<DefaultDevices> {
        let _com = ComGuard::new();
        let en = enumerator()?;
        Ok(DefaultDevices {
            render: default_id(&en, eRender, eConsole),
            render_comm: default_id(&en, eRender, eCommunications),
            capture: default_id(&en, eCapture, eConsole),
            capture_comm: default_id(&en, eCapture, eCommunications),
        })
    }

    fn endpoints(&self) -> Result<Vec<AudioEndpoint>> {
        Ok(all_endpoints()?
            .into_iter()
            .map(|e| {
                let virtual_cable = e.is_cable();
                AudioEndpoint { id: e.id, name: e.name, flow: e.flow, virtual_cable }
            })
            .collect())
    }

    fn set_default(&self, id: &str, role: DeviceRole) -> Result<()> {
        if id.contains('\0') || id.is_empty() {
            return Err(PlatformError::Other("invalid endpoint id".into()));
        }
        let _com = ComGuard::new();
        let policy: IPolicyConfig = unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL) }
            .map_err(|e| win_err("IPolicyConfig", e))?;
        let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        let roles: &[ERole] = match role {
            DeviceRole::Console => &[eConsole, eMultimedia],
            DeviceRole::Communications => &[eCommunications],
        };
        for r in roles {
            unsafe { policy.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), *r) }
                .ok()
                .map_err(|e| win_err("SetDefaultEndpoint", e))?;
        }
        Ok(())
    }
}

// --- the stream -------------------------------------------------------------------------------

struct Shared {
    ring: Mutex<VecDeque<i16>>,
    /// Frames in the device buffer at the last poll.
    padding: AtomicU32,
    lost: AtomicBool,
    stop: AtomicBool,
}

pub struct WasapiStream {
    shared: Arc<Shared>,
    cap: usize,
    join: Option<JoinHandle<()>>,
}

impl WasapiStream {
    fn start(id: String, rate: u32) -> Result<Self> {
        let shared = Arc::new(Shared {
            ring: Mutex::new(VecDeque::new()),
            padding: AtomicU32::new(0),
            lost: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let sh = shared.clone();
        let join = std::thread::Builder::new().name("nd-mic-wasapi".into()).spawn(move || {
            let _com = ComGuard::new();
            match setup(&id, rate) {
                Ok((client, render, frames)) => {
                    let _ = ready_tx.send(Ok(()));
                    playback(&sh, &client, &render, frames);
                    let _ = unsafe { client.Stop() };
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self { shared, cap: (rate as usize / 4).max(480), join: Some(join) }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => {
                shared.stop.store(true, Ordering::Relaxed);
                Err(PlatformError::Other("opening the audio output timed out".into()))
            }
        }
    }
}

fn setup(id: &str, rate: u32) -> Result<(IAudioClient, IAudioRenderClient, u32)> {
    let en = enumerator()?;
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    let dev = unsafe { en.GetDevice(PCWSTR(wide.as_ptr())) }.map_err(|e| win_err("GetDevice", e))?;
    let client: IAudioClient = unsafe { dev.Activate(CLSCTX_ALL, None) }.map_err(|e| win_err("Activate", e))?;
    let fmt = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: 1,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * 2,
        nBlockAlign: 2,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                2_000_000, // 200 ms in 100 ns units
                0,
                &fmt,
                None,
            )
            .map_err(|e| win_err("IAudioClient::Initialize", e))?;
        let frames = client.GetBufferSize().map_err(|e| win_err("GetBufferSize", e))?;
        let render: IAudioRenderClient = client.GetService().map_err(|e| win_err("IAudioRenderClient", e))?;
        client.Start().map_err(|e| win_err("IAudioClient::Start", e))?;
        Ok((client, render, frames))
    }
}

fn playback(sh: &Shared, client: &IAudioClient, render: &IAudioRenderClient, buffer_frames: u32) {
    while !sh.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(5));
        let pad = match unsafe { client.GetCurrentPadding() } {
            Ok(p) => p,
            Err(e) => {
                if e.code() != AUDCLNT_E_DEVICE_INVALIDATED {
                    tracing::warn!("mic GetCurrentPadding: {e}");
                }
                sh.lost.store(true, Ordering::Relaxed);
                return;
            }
        };
        let free = buffer_frames.saturating_sub(pad) as usize;
        let mut ring = match sh.ring.lock() {
            Ok(r) => r,
            Err(_) => return,
        };
        let n = free.min(ring.len());
        if n == 0 {
            sh.padding.store(pad, Ordering::Relaxed);
            continue;
        }
        let buf = match unsafe { render.GetBuffer(n as u32) } {
            Ok(b) => b as *mut i16,
            Err(e) => {
                tracing::warn!("mic GetBuffer: {e}");
                sh.lost.store(true, Ordering::Relaxed);
                return;
            }
        };
        // SAFETY: GetBuffer returned room for `n` mono 16-bit frames.
        let dst = unsafe { std::slice::from_raw_parts_mut(buf, n) };
        for (d, s) in dst.iter_mut().zip(ring.drain(..n)) {
            *d = s;
        }
        drop(ring);
        if let Err(e) = unsafe { render.ReleaseBuffer(n as u32, 0) } {
            tracing::warn!("mic ReleaseBuffer: {e}");
            sh.lost.store(true, Ordering::Relaxed);
            return;
        }
        sh.padding.store(pad + n as u32, Ordering::Relaxed);
    }
}

/// The engine period is 10 ms and the writer thread polls every 5 ms, so 30 ms leaves >= 15 ms in the device.
const WASAPI_TARGET_MS: u32 = 30;

impl MicStream for WasapiStream {
    fn target_ms(&self) -> u32 {
        WASAPI_TARGET_MS
    }

    fn write(&mut self, samples: &[i16]) -> Result<usize> {
        if self.shared.lost.load(Ordering::Relaxed) {
            return Err(PlatformError::Other("audio output lost".into()));
        }
        let mut ring = self.shared.ring.lock().map_err(|_| PlatformError::Other("mic ring poisoned".into()))?;
        let n = samples.len().min(self.cap.saturating_sub(ring.len()));
        ring.extend(&samples[..n]);
        Ok(n)
    }

    fn queued(&self) -> Result<usize> {
        let ring = self.shared.ring.lock().map_err(|_| PlatformError::Other("mic ring poisoned".into()))?;
        Ok(ring.len() + self.shared.padding.load(Ordering::Relaxed) as usize)
    }

    fn lost(&self) -> bool {
        self.shared.lost.load(Ordering::Relaxed)
    }
}

impl Drop for WasapiStream {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(name: &str, adapter: &str, flow: DeviceFlow) -> Ep {
        Ep { id: format!("id-{name}"), name: name.into(), adapter: adapter.into(), flow }
    }

    #[test]
    fn cable_ordering_and_capture_name() {
        let eps = vec![
            ep("Speakers (Realtek)", "Realtek", DeviceFlow::Render),
            ep("CABLE Input (VB-Audio Virtual Cable)", "VB-Audio Virtual Cable", DeviceFlow::Render),
            ep("CABLE Output (VB-Audio Virtual Cable)", "VB-Audio Virtual Cable", DeviceFlow::Capture),
        ];
        let d = render_devices(&eps);
        assert_eq!(d.len(), 2);
        assert!(d[0].virtual_cable);
        assert_eq!(d[0].capture_name, "CABLE Output (VB-Audio Virtual Cable)");
        assert!(!d[1].virtual_cable && d[1].capture_name.is_empty());
    }

    #[test]
    fn policy_config_instantiates() {
        let _com = ComGuard::new();
        // Only creates the object; never calls SetDefaultEndpoint against the real system.
        let p: windows::core::Result<IPolicyConfig> =
            unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL) };
        assert!(p.is_ok(), "{:?}", p.err());
    }

    #[test]
    fn ring_cap_limits_write() {
        let shared = Arc::new(Shared {
            ring: Mutex::new(VecDeque::new()),
            padding: AtomicU32::new(7),
            lost: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let mut s = WasapiStream { shared, cap: 100, join: None };
        assert_eq!(s.write(&[1; 60]).unwrap(), 60);
        assert_eq!(s.write(&[1; 60]).unwrap(), 40);
        assert_eq!(s.queued().unwrap(), 107);
        s.shared.lost.store(true, Ordering::Relaxed);
        assert!(s.write(&[1]).is_err() && s.lost());
    }

    #[test]
    #[ignore]
    fn diagnostics() {
        let s = WasapiMicSink::new();
        println!("devices: {:#?}", s.devices());
        println!("defaults: {:#?}", s.defaults());
        println!("endpoints: {:#?}", s.endpoints());
    }

    #[test]
    #[ignore]
    fn play_quiet_tone_into_cable() {
        let s = WasapiMicSink::new();
        let mut st = match s.open("", 48000) {
            Ok(st) => st,
            Err(e) => {
                println!("open failed: {e}");
                return;
            }
        };
        let mut t = 0usize;
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            let chunk: Vec<i16> = (0..480)
                .map(|i| ((((t + i) as f32) * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 1500.0) as i16)
                .collect();
            let n = st.write(&chunk).unwrap();
            t += n;
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("queued at end: {:?}, lost: {}", st.queued(), st.lost());
    }
}
