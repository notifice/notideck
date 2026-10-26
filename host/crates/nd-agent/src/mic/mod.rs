// SPDX-License-Identifier: GPL-3.0-or-later
//! The phone-microphone manager (round 25, docs/PLAN.md).
//!
//! Plays the phone's microphone into a virtual cable (Windows VB-Cable) or a virtual source
//! (Linux) through [`nd_platform::MicSink`], so Discord & co. can use it.
//!
//! * [`logic`] — pure decisions: state machine, jitter / drift control, gain, level, guard.
//! * [`playback`] — the playback thread (sink writes may block briefly).
//! * this file — [`MicManager`]: the async driver (phone choice, MicStart/MicStop, output open /
//!   reopen, device re-checks, default-device guard) and the status JSON.

pub mod logic;
pub mod playback;

use self::logic::{
    Candidate, Effect, GuardAction, GuardMemory, Input, Machine, OutputStatus, Phase, PhoneMic, PhoneView, Restored,
    Slot,
};
use self::playback::{AudioSlot, Playback, Shared};
use nd_config::{MicControlReq, MicSettings};
use nd_core::{MicActions, MicSnapshot};
use nd_platform::{MicOutputDevice, MicSink, PlatformError};
use nd_proto::v1::{Capability, MediaKind, MicStart, MicState};
use nd_transport::{Link, MediaStream, ServerHandle, SessionHandle, TransportEvent};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, Notify};
use tracing::{debug, info, warn};

const TICK: Duration = Duration::from_millis(250);
/// A packet this recent counts as "audio is flowing".
const FLOW_WINDOW_MS: u64 = 500;
const START_RATE: u32 = logic::DEFAULT_RATE;
/// The phone accepts 10..40 ms; 10 halves the packetisation delay.
const FRAME_MS: u32 = 10;

pub type SettingsFn = Arc<dyn Fn() -> MicSettings + Send + Sync>;

/// Intervals that tests shorten.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Re-read the device list / retry opening the output while there is none.
    pub recheck: Duration,
    /// Default-device guard period.
    pub guard: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { recheck: Duration::from_secs(5), guard: Duration::from_secs(2) }
    }
}

pub struct ManagerConfig {
    pub server: ServerHandle,
    pub audio_rx: mpsc::Receiver<MediaStream>,
    pub sink: Arc<dyn MicSink>,
    pub settings: SettingsFn,
    /// Persist `mic_enabled` (the settings store in the agent).
    pub set_enabled: Arc<dyn Fn(bool) + Send + Sync>,
    /// Notified when on / live / muted change, so deck keys refresh.
    pub poke: Arc<Notify>,
    /// Where `mic_defaults.json` lives.
    pub config_dir: PathBuf,
    pub timing: Timing,
}

fn mono_ms() -> u64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Default)]
struct AudioStats {
    bytes: AtomicU64,
    bad_packets: AtomicU64,
    /// `mono_ms()` of the last packet, 0 = none yet.
    last_packet_ms: AtomicU64,
}

#[derive(Clone)]
struct PhoneInfo {
    name: String,
    link: Link,
    state: PhoneMic,
    sample_rate: u32,
}

#[derive(Clone)]
struct OutputInfo {
    device_id: String,
    device_name: String,
    capture_name: String,
}

#[derive(Clone, Default)]
struct GuardInfo {
    supported: bool,
    warning: String,
    restored: Vec<Restored>,
}

#[derive(Clone)]
struct Published {
    phase: Phase,
    message: String,
    phone: Option<PhoneInfo>,
    output: Option<OutputInfo>,
    devices: Vec<MicOutputDevice>,
    cable_found: bool,
    kbps: f64,
    guard: GuardInfo,
}

impl Default for Published {
    fn default() -> Self {
        Published {
            phase: Phase::Off,
            message: String::new(),
            phone: None,
            output: None,
            devices: Vec::new(),
            cable_found: true,
            kbps: 0.0,
            guard: GuardInfo::default(),
        }
    }
}

struct Inner {
    server: ServerHandle,
    sink: Arc<dyn MicSink>,
    settings: SettingsFn,
    set_enabled: Arc<dyn Fn(bool) + Send + Sync>,
    poke: Arc<Notify>,
    config_dir: PathBuf,
    timing: Timing,
    muted: Arc<AtomicBool>,
    wake: Notify,
    published: Mutex<Published>,
    /// `mono_ms()` of the last status request: the UI is open, keep the device list fresh.
    ui_seen: AtomicU64,
    slot: AudioSlot,
    audio: Arc<AudioStats>,
    /// The running playback's counters, for live reads in `status_json`.
    live: Mutex<Option<Arc<Shared>>>,
    stopping: AtomicBool,
    stopped: AtomicBool,
}

pub struct MicManager {
    inner: Arc<Inner>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl MicManager {
    /// Spawn the driver on the current tokio runtime.
    pub fn start(cfg: ManagerConfig) -> Arc<Self> {
        let inner = Arc::new(Inner {
            server: cfg.server,
            sink: cfg.sink,
            settings: cfg.settings,
            set_enabled: cfg.set_enabled,
            poke: cfg.poke,
            config_dir: cfg.config_dir,
            timing: cfg.timing,
            muted: Arc::new(AtomicBool::new(false)),
            wake: Notify::new(),
            published: Mutex::new(Published::default()),
            ui_seen: AtomicU64::new(0),
            slot: Arc::default(),
            audio: Arc::default(),
            live: Mutex::new(None),
            stopping: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        });
        let driver = Driver::new(inner.clone());
        let task = tokio::spawn(driver.run(cfg.audio_rx));
        Arc::new(MicManager { inner, task: Mutex::new(Some(task)) })
    }

    /// Agent exit: stop the phone and the output, then let the sink clean up. Blocks up to ~1.5 s.
    pub fn shutdown(&self) {
        self.inner.stopping.store(true, Ordering::SeqCst);
        self.inner.wake.notify_one();
        let t0 = Instant::now();
        while !self.inner.stopped.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_millis(1500) {
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Some(t) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
            t.abort();
        }
        self.inner.sink.shutdown();
    }

    /// `POST /api/mic`.
    pub fn control(&self, req: MicControlReq) -> Result<(), String> {
        if let Some(on) = req.on {
            (self.inner.set_enabled)(on);
        }
        if let Some(m) = req.muted {
            self.inner.muted.store(m, Ordering::SeqCst);
        }
        self.inner.wake.notify_one();
        self.inner.poke.notify_one();
        Ok(())
    }

    fn live_phase(&self) -> Phase {
        self.inner.published.lock().unwrap_or_else(|e| e.into_inner()).phase
    }

    /// `GET /api/mic`, shape in docs/PLAN.md round 25.
    pub fn status_json(&self) -> Value {
        let inner = &self.inner;
        let first_look = inner.ui_seen.swap(mono_ms(), Ordering::Relaxed) == 0;
        if first_look {
            inner.wake.notify_one();
        }
        let p = inner.published.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let settings = (inner.settings)();
        let live = inner.live.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let (level, buffer_ms, underruns, dropped_ms) = match (&live, p.phase) {
            (Some(s), Phase::Live | Phase::Starting) => (
                s.level(),
                s.buffer_ms.load(Ordering::Relaxed),
                s.underruns.load(Ordering::Relaxed),
                s.dropped_ms.load(Ordering::Relaxed),
            ),
            (Some(s), _) => (
                0.0,
                s.buffer_ms.load(Ordering::Relaxed),
                s.underruns.load(Ordering::Relaxed),
                s.dropped_ms.load(Ordering::Relaxed),
            ),
            _ => (0.0, 0, 0, 0),
        };
        let phone = p.phone.as_ref().map(|ph| {
            json!({
                "name": ph.name,
                "link": ph.link.as_str(),
                "supported": true,
                "state": ph.state.as_str(),
                "sample_rate": ph.sample_rate,
            })
        });
        let output = p.output.as_ref().map(|o| {
            json!({
                "backend": inner.sink.backend_name(),
                "device_id": o.device_id,
                "device_name": o.device_name,
                "capture_name": o.capture_name,
            })
        });
        let devices: Vec<Value> = p
            .devices
            .iter()
            .map(|d| {
                json!({"id": d.id, "name": d.name, "virtual_cable": d.virtual_cable, "capture_name": d.capture_name})
            })
            .collect();
        json!({
            "platform": if cfg!(windows) { "windows" } else { "linux" },
            "state": p.phase.as_str(),
            "message": p.message,
            "on": settings.enabled,
            "muted": inner.muted.load(Ordering::SeqCst),
            "phone": phone,
            "output": output,
            "devices": devices,
            "cable_found": if cfg!(windows) { p.cable_found } else { true },
            "level": (f64::from(level) * 1000.0).round() / 1000.0,
            "stats": {
                "buffer_ms": buffer_ms,
                "underruns": underruns,
                "dropped_ms": dropped_ms,
                "kbps": p.kbps.round(),
            },
            "guard": {
                "enabled": settings.guard_defaults && p.guard.supported,
                "warning": p.guard.warning,
                "restored": p.guard.restored.iter().map(|r| json!({
                    "flow": r.flow, "role": r.role, "from": r.from, "to": r.to, "at": r.at,
                })).collect::<Vec<_>>(),
            },
        })
    }
}

impl MicActions for MicManager {
    fn toggle(&self) -> Result<bool, String> {
        let on = !(self.inner.settings)().enabled;
        (self.inner.set_enabled)(on);
        self.inner.wake.notify_one();
        self.inner.poke.notify_one();
        Ok(on)
    }

    fn toggle_mute(&self) -> Result<bool, String> {
        let m = !self.inner.muted.load(Ordering::SeqCst);
        self.inner.muted.store(m, Ordering::SeqCst);
        self.inner.poke.notify_one();
        Ok(m)
    }

    fn snapshot(&self) -> MicSnapshot {
        MicSnapshot {
            on: (self.inner.settings)().enabled,
            live: self.live_phase() == Phase::Live,
            muted: self.inner.muted.load(Ordering::SeqCst),
        }
    }
}

// ------------------------------------------------------------------------------------ driver

struct Driver {
    inner: Arc<Inner>,
    machine: Machine,
    playback: Option<Playback>,
    output: OutputStatus,
    out_info: Option<OutputInfo>,
    order: HashMap<String, u64>,
    next_order: u64,
    devices: Vec<MicOutputDevice>,
    devices_ok: bool,
    devices_at: Option<Instant>,
    last_open_try: Option<Instant>,
    guard_at: Instant,
    guard_supported: bool,
    guard_memory: GuardMemory,
    guard_warning: String,
    restored: Vec<Restored>,
    guard_failed: HashMap<Slot, String>,
    rate_at: Instant,
    rate_bytes: u64,
    kbps: f64,
    last_poked: (bool, bool, bool),
}

impl Driver {
    fn new(inner: Arc<Inner>) -> Self {
        let now = Instant::now();
        let guard_memory = load_memory(&inner.config_dir);
        Driver {
            machine: Machine::new(cfg!(windows)),
            inner,
            playback: None,
            output: OutputStatus::Closed,
            out_info: None,
            order: HashMap::new(),
            next_order: 1,
            devices: Vec::new(),
            devices_ok: false,
            devices_at: None,
            last_open_try: None,
            guard_at: now,
            guard_supported: true,
            guard_memory,
            guard_warning: String::new(),
            restored: Vec::new(),
            guard_failed: HashMap::new(),
            rate_at: now,
            rate_bytes: 0,
            kbps: 0.0,
            last_poked: (false, false, false),
        }
    }

    async fn run(mut self, mut audio_rx: mpsc::Receiver<MediaStream>) {
        let mut events = self.inner.server.subscribe_events();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut audio_open = true;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.inner.wake.notified() => {}
                ev = events.recv() => match ev {
                    Ok(TransportEvent::SessionConnected(s)) => self.note_session(s.session_id()),
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        events = self.inner.server.subscribe_events();
                    }
                },
                ms = audio_rx.recv(), if audio_open => match ms {
                    Some(ms) => self.on_audio(ms),
                    None => audio_open = false,
                },
            }
            if self.inner.stopping.load(Ordering::SeqCst) {
                self.finish().await;
                return;
            }
            self.step(Instant::now()).await;
        }
    }

    async fn finish(&mut self) {
        if let Some(id) = self.machine.started_session().map(str::to_string) {
            if let Some(s) = self.session(&id) {
                let _ = s.try_send_msg(nd_proto::Msg::MicStop(nd_proto::v1::MicStop {}));
            }
        }
        self.close_playback();
        self.inner.stopped.store(true, Ordering::SeqCst);
    }

    fn note_session(&mut self, id: &str) {
        if !self.order.contains_key(id) {
            self.order.insert(id.to_string(), self.next_order);
            self.next_order += 1;
        }
    }

    fn session(&self, id: &str) -> Option<SessionHandle> {
        self.inner.server.all_sessions().into_iter().find(|s| s.session_id() == id && s.is_connected())
    }

    /// An accepted audio media connection: read it while it belongs to the session we started.
    fn on_audio(&mut self, ms: MediaStream) {
        if ms.kind != MediaKind::Audio || self.machine.started_session() != Some(ms.session_id.as_str()) {
            info!(session = %ms.session_id, "dropping unexpected audio connection");
            return;
        }
        info!(session = %ms.session_id, link = ?ms.link, "mic audio connection attached");
        let (slot, stats) = (self.inner.slot.clone(), self.inner.audio.clone());
        tokio::spawn(read_audio(ms, slot, stats));
    }

    fn phone_view(&mut self) -> (Option<PhoneView>, Option<PhoneInfo>) {
        let sessions: Vec<SessionHandle> = self
            .inner
            .server
            .all_sessions()
            .into_iter()
            .filter(|s| s.is_connected() && s.capabilities().contains(&Capability::Mic))
            .collect();
        for s in &sessions {
            self.note_session(s.session_id());
        }
        let cands: Vec<Candidate> = sessions
            .iter()
            .map(|s| Candidate {
                session_id: s.session_id().to_string(),
                order: self.order.get(s.session_id()).copied().unwrap_or(0),
            })
            .collect();
        let Some(i) = logic::choose_session(&cands) else { return (None, None) };
        let s = &sessions[i];
        let (state, error, rate) = match s.last_mic_status() {
            Some(st) => (
                match MicState::try_from(st.state) {
                    Ok(MicState::Stopped) => PhoneMic::Stopped,
                    Ok(MicState::Starting) => PhoneMic::Starting,
                    Ok(MicState::Streaming) => PhoneMic::Streaming,
                    Ok(MicState::Error) => PhoneMic::Error,
                    Ok(MicState::NoPermission) => PhoneMic::NoPermission,
                    _ => PhoneMic::Unknown,
                },
                st.error,
                st.sample_rate,
            ),
            None => (PhoneMic::Unknown, String::new(), 0),
        };
        let name = {
            let n = s.peer_info().device_name.clone();
            if n.is_empty() {
                s.device_model().to_string()
            } else {
                n
            }
        };
        let view =
            PhoneView { session_id: s.session_id().to_string(), link: s.link(), state, error, sample_rate: rate };
        let info = PhoneInfo { name, link: s.link(), state, sample_rate: rate };
        (Some(view), Some(info))
    }

    async fn refresh_devices(&mut self, now: Instant) {
        let sink = self.inner.sink.clone();
        let res = tokio::task::spawn_blocking(move || sink.devices()).await;
        self.devices_at = Some(now);
        match res {
            Ok(Ok(list)) => {
                self.devices = list;
                self.devices_ok = true;
            }
            Ok(Err(e)) => {
                debug!("mic devices: {e}");
                self.devices_ok = false;
            }
            Err(_) => self.devices_ok = false,
        }
    }

    fn close_playback(&mut self) {
        *self.inner.slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.inner.live.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(pb) = self.playback.take() {
            pb.close();
        }
        self.out_info = None;
    }

    async fn open_output(&mut self, device: String, rate: u32, now: Instant, gain: f32) {
        if matches!(self.output, OutputStatus::Failed(_))
            && self.last_open_try.is_some_and(|t| now.duration_since(t) < self.inner.timing.recheck)
        {
            return;
        }
        self.close_playback();
        self.last_open_try = Some(now);
        let sink = self.inner.sink.clone();
        let dev = device.clone();
        let res = tokio::task::spawn_blocking(move || sink.open(&dev, rate)).await;
        match res {
            Ok(Ok(stream)) => {
                info!(device = %device, rate, "mic output opened");
                let pb = Playback::spawn(stream, rate, self.inner.muted.clone(), gain);
                *self.inner.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(pb.tx.clone());
                *self.inner.live.lock().unwrap_or_else(|e| e.into_inner()) = Some(pb.shared.clone());
                let found = logic::resolve_device(&self.devices, &device);
                self.out_info = Some(OutputInfo {
                    device_id: found.map(|d| d.id.clone()).unwrap_or_else(|| device.clone()),
                    device_name: found.map(|d| d.name.clone()).unwrap_or_default(),
                    capture_name: found.map(|d| d.capture_name.clone()).unwrap_or_default(),
                });
                self.playback = Some(pb);
                self.output = OutputStatus::Open { rate, device };
            }
            Ok(Err(e)) => {
                warn!("cannot open the mic output: {e}");
                self.output = OutputStatus::Failed(e.to_string());
                self.devices_at = None; // look at the device list again right away
            }
            Err(_) => self.output = OutputStatus::Failed("the output task failed".into()),
        }
    }

    async fn step(&mut self, now: Instant) {
        let settings = (self.inner.settings)();
        let (phone_view, phone_info) = self.phone_view();

        // Device list: needed while on without a healthy output, and while the UI looks at it.
        let ui_recent = mono_ms().saturating_sub(self.inner.ui_seen.load(Ordering::Relaxed)) < 15_000
            && self.inner.ui_seen.load(Ordering::Relaxed) != 0;
        let need = (settings.enabled && !matches!(self.output, OutputStatus::Open { .. })) || ui_recent;
        if self.devices_at.is_none_or(|t| need && now.duration_since(t) >= self.inner.timing.recheck) {
            self.refresh_devices(now).await;
        }
        let devices_missing =
            settings.enabled && self.devices_ok && logic::resolve_device(&self.devices, &settings.device).is_none();

        // A lost device ends the playback thread.
        if self.playback.as_ref().is_some_and(Playback::lost) {
            self.close_playback();
            self.output = OutputStatus::Failed("The audio output went away.".into());
            self.last_open_try = Some(now);
            self.devices_at = None;
        }

        let gain = logic::db_to_linear(settings.gain_db);
        if let Some(pb) = &self.playback {
            pb.shared.set_gain(gain);
        }
        let last = self.inner.audio.last_packet_ms.load(Ordering::Relaxed);
        let flowing = self.playback.is_some() && last != 0 && mono_ms().saturating_sub(last) < FLOW_WINDOW_MS;

        let effects = self.machine.step(&Input {
            now,
            enabled: settings.enabled,
            device: &settings.device,
            voice_processing: settings.voice_processing,
            phone: phone_view.as_ref(),
            devices_missing,
            output: &self.output,
            audio_flowing: flowing,
        });
        for fx in effects {
            self.apply(fx, now, gain).await;
        }

        self.guard_tick(now, &settings).await;
        self.update_rate(now);

        let guard = GuardInfo {
            supported: self.guard_supported,
            warning: self.guard_warning.clone(),
            restored: self.restored.clone(),
        };
        let cable_found = !self.devices_ok || self.devices.iter().any(|d| d.virtual_cable);
        let published = Published {
            phase: self.machine.phase,
            message: self.machine.message.clone(),
            phone: phone_info,
            output: self.out_info.clone(),
            devices: self.devices.clone(),
            cable_found,
            kbps: self.kbps,
            guard,
        };
        let snap = (settings.enabled, published.phase == Phase::Live, self.inner.muted.load(Ordering::SeqCst));
        *self.inner.published.lock().unwrap_or_else(|e| e.into_inner()) = published;
        if snap != self.last_poked {
            self.last_poked = snap;
            self.inner.poke.notify_one();
        }
    }

    async fn apply(&mut self, fx: Effect, now: Instant, gain: f32) {
        match fx {
            Effect::OpenOutput { device, rate } => self.open_output(device, rate, now, gain).await,
            Effect::CloseOutput => {
                self.close_playback();
                self.output = OutputStatus::Closed;
            }
            Effect::StartPhone { session_id, voice_processing } => {
                let Some(s) = self.session(&session_id) else { return };
                info!(voice_processing, "mic start");
                if let Err(e) =
                    s.mic_start(MicStart { sample_rate: START_RATE, frame_ms: FRAME_MS, voice_processing }).await
                {
                    warn!("MicStart failed: {e}");
                }
            }
            Effect::StopPhone { session_id } => {
                info!("mic stop");
                if let Some(s) = self.session(&session_id) {
                    if let Err(e) = s.mic_stop().await {
                        debug!("MicStop skipped: {e}");
                    }
                }
            }
        }
    }

    fn update_rate(&mut self, now: Instant) {
        let dt = now.duration_since(self.rate_at).as_secs_f64();
        if dt < 1.0 {
            return;
        }
        let cur = self.inner.audio.bytes.load(Ordering::Relaxed);
        self.kbps = cur.saturating_sub(self.rate_bytes) as f64 * 8.0 / 1000.0 / dt;
        self.rate_bytes = cur;
        self.rate_at = now;
    }

    async fn guard_tick(&mut self, now: Instant, settings: &MicSettings) {
        if !settings.guard_defaults || !self.guard_supported {
            self.guard_warning.clear();
            return;
        }
        if now.duration_since(self.guard_at) < self.inner.timing.guard {
            return;
        }
        self.guard_at = now;
        let sink = self.inner.sink.clone();
        let read = tokio::task::spawn_blocking(move || {
            let d = sink.defaults()?;
            let e = sink.endpoints()?;
            Ok::<_, PlatformError>((d, e))
        })
        .await;
        let (defaults, endpoints) = match read {
            Ok(Ok(v)) => v,
            Ok(Err(PlatformError::Unsupported(_))) => {
                self.guard_supported = false;
                return;
            }
            Ok(Err(e)) => {
                debug!("guard: {e}");
                return;
            }
            Err(_) => return,
        };
        let decision = logic::guard_decide(&self.guard_memory, &defaults, &endpoints);
        let mut warnings = decision.warnings;
        let mut dirty = false;
        for action in decision.actions {
            match action {
                GuardAction::Remember { slot, device } => {
                    self.guard_memory.slots.insert(slot, device);
                    dirty = true;
                }
                GuardAction::Restore { slot, id, from, to } => {
                    let sink = self.inner.sink.clone();
                    let id2 = id.clone();
                    let res = tokio::task::spawn_blocking(move || sink.set_default(&id2, slot.role())).await;
                    match res {
                        Ok(Ok(())) => {
                            info!(%from, %to, "default audio device restored");
                            self.guard_failed.remove(&slot);
                            logic::push_restored(
                                &mut self.restored,
                                Restored { flow: slot.flow_str(), role: slot.role_str(), from, to, at: unix_secs() },
                            );
                        }
                        other => {
                            let why = match other {
                                Ok(Err(e)) => e.to_string(),
                                _ => "task failed".into(),
                            };
                            if self.guard_failed.insert(slot, why.clone()).is_none() {
                                warn!("cannot restore the default audio device {to}: {why}");
                            }
                            warnings.push(format!("Notideck could not put {to} back as the default device ({why})."));
                        }
                    }
                }
            }
        }
        if dirty {
            save_memory(&self.inner.config_dir, &self.guard_memory);
        }
        self.guard_warning = warnings.join(" ");
    }
}

/// Reads one audio media connection into the playback queue until it ends.
async fn read_audio(mut ms: MediaStream, slot: AudioSlot, stats: Arc<AudioStats>) {
    loop {
        match ms.next_packet().await {
            Ok(Some(p)) => {
                if let Err(e) = logic::validate_packet(p.data.len()) {
                    if stats.bad_packets.fetch_add(1, Ordering::Relaxed) < 3 {
                        warn!("mic: {e}");
                    }
                    continue;
                }
                stats.bytes.fetch_add(p.data.len() as u64, Ordering::Relaxed);
                stats.last_packet_ms.store(mono_ms(), Ordering::Relaxed);
                let pcm = logic::pcm_from_le(&p.data);
                if let Some(tx) = &*slot.lock().unwrap_or_else(|e| e.into_inner()) {
                    let _ = tx.send(pcm);
                }
            }
            Ok(None) => break,
            Err(e) => {
                debug!("mic audio connection: {e}");
                break;
            }
        }
    }
    debug!("mic audio connection ended");
}

fn memory_path(dir: &std::path::Path) -> PathBuf {
    dir.join("mic_defaults.json")
}

fn load_memory(dir: &std::path::Path) -> GuardMemory {
    std::fs::read_to_string(memory_path(dir)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_memory(dir: &std::path::Path, mem: &GuardMemory) {
    let Ok(json) = serde_json::to_string_pretty(mem) else { return };
    let path = memory_path(dir);
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path)).is_err() {
        warn!("cannot save {}", path.display());
    }
}
