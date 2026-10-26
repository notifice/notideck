// SPDX-License-Identifier: GPL-3.0-or-later
//! The webcam manager (ADR 0005, SPEC §7.3).
//!
//! Watches the shared memory for a consumer (an app that opened "Notideck Camera"), starts the
//! phone camera at a fitting mode, decodes the H.264 stream, scales it to the consumer's size and
//! writes frames to shared memory. Stops the phone 5 s after the consumer leaves.
//!
//! * [`logic`] — pure decisions and the state machine.
//! * [`queue`] / [`pipeline`] — media connection -> decode thread -> shared memory.
//! * [`webcam`] — Windows install state, elevation and the MF virtual camera object.

pub mod logic;
pub mod pipeline;
pub mod queue;
pub mod stats;
pub mod webcam;

use self::logic::{choose_session, Candidate, Effect, Input, Machine, Phase, PhoneCam, PhoneView, Running};
use self::pipeline::{preview_png, DecoderFactory, Pipeline, PipelineCtx, Preview, ShmSlot, Target};
use nd_config::{CameraControlReq, CameraLens, CameraSettings, Problem};
use nd_core::{CameraActions, CameraSnapshot};
use nd_proto::v1::{CameraControl, CameraImage, CameraReadout, CameraState, Capability, LensCaps};
use nd_proto::Msg;
use nd_transport::{MediaStream, ServerHandle, SessionHandle};
use nd_vcam::HasShm;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};
use tracing::{info, warn};

const TICK: Duration = Duration::from_millis(250);
const SHM_POLL: Duration = Duration::from_millis(250);
/// The DLL bumps its heartbeat on every sample; this long without one means it is gone.
const CONSUMER_STALE_MS: u64 = 15_000;
const TEST_SIZE: (u32, u32, u32) = (1280, 720, 30);

pub type SettingsFn = Arc<dyn Fn() -> CameraSettings + Send + Sync>;
pub type ShmProvider = Arc<dyn Fn() -> Option<Arc<dyn HasShm>> + Send + Sync>;

pub struct ManagerConfig {
    pub server: ServerHandle,
    pub media_rx: mpsc::Receiver<MediaStream>,
    pub settings: SettingsFn,
    /// Persist a lens choice made from a deck key (the settings store in the agent).
    pub set_lens: Arc<dyn Fn(CameraLens) + Send + Sync>,
    /// Finds the shared memory region; polled at 4 Hz until it returns `Some`.
    pub shm_provider: ShmProvider,
    /// Dev override (`NOTIDECK_VCAM_FAKE_CONSUMER=WxH`): pretend an app is using the camera.
    pub fake_consumer: Option<(u32, u32)>,
    /// Defaults to `nd_media::default_decoder`.
    pub decoder_factory: Option<DecoderFactory>,
    /// Create the Windows virtual camera object / start the Linux v4l2 sink when installed.
    pub manage_vcam: bool,
    /// Linux: the same `MemShm` the `shm_provider` returns; the v4l2 sink reads frames from it.
    pub vcam_mem: Option<webcam::VcamMem>,
}

/// Parse `NOTIDECK_VCAM_FAKE_CONSUMER` (`1280x720`).
pub fn parse_fake_consumer(v: &str) -> Option<(u32, u32)> {
    let (w, h) = v.trim().split_once(['x', 'X'])?;
    let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    ((2..=1920).contains(&w) && (2..=1080).contains(&h)).then_some((w & !1, h & !1))
}

#[derive(Clone, Default)]
struct RateView {
    fps_in: f64,
    fps_out: f64,
    kbps: f64,
}

#[derive(Clone)]
struct PhoneInfo {
    session_id: String,
    name: String,
    link: nd_transport::Link,
    modes: Vec<nd_proto::v1::CameraMode>,
    lens_caps: Vec<LensCaps>,
    state: PhoneCam,
}

/// What the API reports; rewritten by the driver every tick.
#[derive(Clone)]
struct Published {
    phase: Phase,
    message: String,
    /// An app (or the dev override) is using the camera.
    real_consumer: Option<(u32, u32, u32)>,
    phone: Option<PhoneInfo>,
    /// Latest phone readout, only while streaming.
    readout: Option<CameraReadout>,
    running: Option<Running>,
    rates: RateView,
    decode_ms: f64,
    age_ms: f64,
    dropped: u64,
    decode_errors: u64,
}

impl Default for Published {
    fn default() -> Self {
        Published {
            phase: Phase::Idle,
            message: String::new(),
            real_consumer: None,
            phone: None,
            readout: None,
            running: None,
            rates: RateView::default(),
            decode_ms: 0.0,
            age_ms: 0.0,
            dropped: 0,
            decode_errors: 0,
        }
    }
}

#[derive(Default)]
struct InstallJob {
    running: bool,
    error: Option<String>,
}

struct Inner {
    server: ServerHandle,
    settings: SettingsFn,
    set_lens: Arc<dyn Fn(CameraLens) + Send + Sync>,
    provider: ShmProvider,
    fake_consumer: Option<(u32, u32)>,
    ctx: PipelineCtx,
    test_on: AtomicBool,
    paused: AtomicBool,
    torch: AtomicBool,
    zoom_bits: AtomicU32,
    wake: Notify,
    published: Mutex<Published>,
    /// Session of the running stream, for direct control messages.
    active: Mutex<Option<SessionHandle>>,
    vcam: webcam::VcamHost,
    manage_vcam: bool,
    install_cache: Mutex<Option<(Instant, webcam::InstallInfo)>>,
    install_job: Mutex<InstallJob>,
    decoder: Mutex<Option<Result<String, String>>>,
    problems_refresh: Notify,
}

pub struct CameraManager {
    inner: Arc<Inner>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl CameraManager {
    /// Spawn the driver on the current tokio runtime.
    pub fn start(cfg: ManagerConfig) -> Arc<Self> {
        let factory: DecoderFactory =
            cfg.decoder_factory.clone().unwrap_or_else(|| Arc::new(nd_media::default_decoder));
        let ctx = PipelineCtx {
            shm: ShmSlot::default(),
            target: Arc::new(Target::default()),
            write_enabled: Arc::new(AtomicBool::new(true)),
            preview: Arc::new(Preview::default()),
            factory: factory.clone(),
        };
        let vcam = webcam::VcamHost::spawn(cfg.vcam_mem, cfg.settings.clone());
        let inner = Arc::new(Inner {
            server: cfg.server,
            settings: cfg.settings,
            set_lens: cfg.set_lens,
            provider: cfg.shm_provider,
            fake_consumer: cfg.fake_consumer,
            ctx,
            test_on: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            torch: AtomicBool::new(false),
            zoom_bits: AtomicU32::new(1.0f32.to_bits()),
            wake: Notify::new(),
            published: Mutex::new(Published::default()),
            active: Mutex::new(None),
            vcam,
            manage_vcam: cfg.manage_vcam,
            install_cache: Mutex::new(None),
            install_job: Mutex::new(InstallJob::default()),
            decoder: Mutex::new(None),
            problems_refresh: Notify::new(),
        });

        // Prove a decoder can be created (Media Feature Pack missing on N editions, etc.).
        let probe = inner.clone();
        std::thread::spawn(move || {
            let res = (factory)().map(|d| d.name().to_string()).map_err(|e| e.to_string());
            match &res {
                Ok(n) => info!(decoder = %n, "H.264 decoder available"),
                Err(e) => warn!("no H.264 decoder: {e}"),
            }
            *probe.decoder.lock().unwrap_or_else(|e| e.into_inner()) = Some(res);
            probe.problems_refresh.notify_one();
        });

        // Linux: the host thread re-checks the loopback device itself, so always ask it.
        if inner.manage_vcam && (cfg!(target_os = "linux") || inner.install_info(true).state == "installed") {
            inner.vcam.ensure();
        }

        let driver = Driver::new(inner.clone());
        let task = tokio::spawn(driver.run(cfg.media_rx));
        Arc::new(CameraManager { inner, task: Mutex::new(Some(task)) })
    }

    pub fn shutdown(&self) {
        if let Some(t) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
            t.abort();
        }
        self.inner.vcam.remove();
    }

    /// Notified when the problem list may have changed (decoder probe, install finished).
    pub fn problems_changed(&self) -> &Notify {
        &self.inner.problems_refresh
    }

    /// Webcam-related entries for the problem list.
    pub fn problems(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        if self.inner.manage_vcam {
            let info = self.inner.install_info(false);
            match info.state {
                "module_missing" => out.push(Problem {
                    code: "webcam_module_missing".into(),
                    message: "The v4l2loopback kernel module is not installed for the running kernel, so the \
                              Notideck webcam cannot be created. Install it: Arch/CachyOS `v4l2loopback-dkms` (or \
                              your kernel's own v4l2loopback module package), Debian/Ubuntu `v4l2loopback-dkms`, \
                              Fedora `v4l2loopback` from RPM Fusion. Then reopen this page."
                        .into(),
                    fix: None,
                }),
                "not_installed" => out.push(Problem {
                    code: "webcam_not_installed".into(),
                    message: if cfg!(target_os = "linux") {
                        "The Notideck webcam is not set up yet. Set it up once (it asks for your administrator \
                         password) so Zoom, Discord, OBS and browsers can use your phone as a camera."
                    } else {
                        "The Notideck webcam is not installed yet. Install it once (it asks for administrator \
                         permission) so Zoom, Teams, Discord and other apps can use your phone as a camera."
                    }
                    .into(),
                    fix: Some("install_webcam".into()),
                }),
                "outdated" => out.push(Problem {
                    code: "webcam_outdated".into(),
                    message: "The installed Notideck webcam is older than this version of Notideck. Update it once \
                              (administrator permission needed)."
                        .into(),
                    fix: Some("install_webcam".into()),
                }),
                _ => {}
            }
            if let Some(e) = self.inner.vcam.error() {
                out.push(Problem {
                    code: "webcam_failed".into(),
                    message: format!("The Notideck Camera could not be started: {e}"),
                    fix: None,
                });
            }
        }
        if let Some(Err(e)) = &*self.inner.decoder.lock().unwrap_or_else(|e| e.into_inner()) {
            out.push(Problem {
                code: "h264_decoder_missing".into(),
                message: format!(
                    "No H.264 decoder is available ({e}). On Windows N editions install the Media Feature Pack, \
                     then restart Notideck."
                ),
                fix: None,
            });
        }
        out
    }

    /// Run the elevated installer in the background (`POST /api/fix/install_webcam`).
    pub fn begin_install(self: &Arc<Self>, install: bool) -> Result<(), String> {
        {
            let mut job = self.inner.install_job.lock().unwrap_or_else(|e| e.into_inner());
            if job.running {
                return Err("The webcam installer is already running.".into());
            }
            job.running = true;
            job.error = None;
        }
        let this = self.clone();
        let spawned = std::thread::Builder::new().name("nd-vcam-install".into()).spawn(move || {
            if !install {
                this.inner.vcam.remove();
                std::thread::sleep(Duration::from_millis(300));
            }
            let res = webcam::run_elevated(install);
            let state = this.inner.install_info(true).state;
            if let Err(e) = &res {
                warn!("webcam {}: {e}", if install { "install" } else { "uninstall" });
            } else if install && matches!(state, "installed" | "outdated") {
                this.inner.vcam.ensure();
            } else if res.is_err() && !install && cfg!(target_os = "linux") {
                this.inner.vcam.ensure(); // the uninstall did not happen; bring the sink back
            }
            {
                let mut job = this.inner.install_job.lock().unwrap_or_else(|e| e.into_inner());
                job.running = false;
                job.error = res.err();
            }
            this.inner.problems_refresh.notify_one();
        });
        if let Err(e) = spawned {
            let mut job = self.inner.install_job.lock().unwrap_or_else(|e| e.into_inner());
            job.running = false;
            return Err(format!("cannot start the installer: {e}"));
        }
        Ok(())
    }

    /// `POST /api/camera/test`.
    pub fn set_test(&self, on: bool) {
        self.inner.test_on.store(on, Ordering::SeqCst);
        self.inner.wake.notify_one();
    }

    /// `POST /api/camera/control`.
    pub fn control(&self, req: CameraControlReq) -> Result<(), String> {
        if let Some(l) = req.lens {
            (self.inner.set_lens)(l);
            self.inner.wake.notify_one();
        }
        if let Some(t) = req.torch {
            self.inner.set_torch(t);
        }
        if let Some(z) = req.zoom {
            self.inner.set_zoom(z);
        }
        if req.keyframe == Some(true) {
            self.inner.send_control(CameraControl { request_keyframe: true, ..Default::default() });
        }
        Ok(())
    }

    pub fn preview_png(&self) -> Option<Vec<u8>> {
        preview_png(&self.inner.ctx.preview.request()?)
    }

    /// `GET /api/camera`.
    pub fn status_json(&self) -> Value {
        let p = self.inner.published.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let settings = (self.inner.settings)();
        let info = self.inner.install_info(false);
        let test = self.inner.test_on.load(Ordering::SeqCst);
        let paused = self.inner.paused.load(Ordering::SeqCst);
        let consumer = match (p.real_consumer, test) {
            (Some((w, h, fps)), _) => {
                Some(json!({"active": true, "width": w, "height": h, "fps": fps, "source": "app"}))
            }
            (None, true) => Some(
                json!({"active": true, "width": TEST_SIZE.0, "height": TEST_SIZE.1, "fps": TEST_SIZE.2, "source": "test"}),
            ),
            _ => None,
        };
        let lens_name = |l: i32| match nd_proto::v1::Lens::try_from(l) {
            Ok(nd_proto::v1::Lens::Front) => "front",
            Ok(nd_proto::v1::Lens::External) => "external",
            _ => "back",
        };
        let phone = p.phone.as_ref().map(|ph| {
            json!({
                "session_id": ph.session_id,
                "name": ph.name,
                "camera_state": match ph.state {
                    PhoneCam::Unknown => "unknown", PhoneCam::Stopped => "stopped", PhoneCam::Starting => "starting",
                    PhoneCam::Streaming => "streaming", PhoneCam::Throttled => "throttled", PhoneCam::Error => "error",
                },
                "link": ph.link.as_str(),
                "lens_caps": ph.lens_caps.iter().map(|c| lens_caps_json(c, lens_name(c.lens))).collect::<Vec<_>>(),
                "supported_modes": ph.modes.iter().map(|m| json!({
                    "lens": lens_name(m.lens), "width": m.width, "height": m.height,
                    "fps": if m.fps.is_empty() { vec![30] } else { m.fps.clone() },
                })).collect::<Vec<_>>(),
            })
        });
        let active_mode = p.running.as_ref().map(|r| {
            json!({
                "width": r.mode.w, "height": r.mode.h, "fps": r.mode.fps,
                "bitrate_kbps": r.bitrate_bps / 1000, "lens": if r.lens == CameraLens::Front { "front" } else { "back" },
                "mirror": r.mirror, "link": r.link.as_str(),
            })
        });
        let job = self.inner.install_job.lock().unwrap_or_else(|e| e.into_inner());
        let decoder = match &*self.inner.decoder.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(Ok(n)) => json!({"name": n}),
            Some(Err(e)) => json!({"error": e}),
            None => Value::Null,
        };
        json!({
            "state": p.phase.as_str(),
            "message": p.message,
            "paused": paused,
            "test": test,
            "consumer": consumer,
            "phone": phone,
            "readout": p.readout.map(|r| json!({
                "iso": r.iso, "exposure_us": r.exposure_us, "focus_permille": r.focus_permille, "focused": r.focused,
            })),
            "image": serde_json::to_value(&settings.image).unwrap_or(Value::Null),
            "active_mode": active_mode,
            "stats": {
                "fps_in": round1(p.rates.fps_in),
                "fps_out": round1(p.rates.fps_out),
                "kbps": p.rates.kbps.round(),
                "decode_ms": round1(p.decode_ms),
                "age_ms": p.age_ms.round(),
                "dropped": p.dropped,
                "decode_errors": p.decode_errors,
            },
            "lens": if settings.lens == CameraLens::Front { "front" } else { "back" },
            "torch": self.inner.torch.load(Ordering::SeqCst),
            "zoom": self.inner.zoom(),
            "installed": info.state,
            "platform": std::env::consts::OS,
            "dll_present": info.app_dll_present,
            "install": {"running": job.running, "error": job.error},
            "decoder": decoder,
            "shm": self.inner.ctx.shm.is_set(),
        })
    }
}

fn lens_caps_json(c: &LensCaps, lens: &str) -> Value {
    use nd_proto::v1::{Antibanding, WhiteBalance};
    let wb: Vec<&str> = c
        .white_balances
        .iter()
        .filter_map(|&w| match WhiteBalance::try_from(w).ok()? {
            WhiteBalance::Auto => Some("auto"),
            WhiteBalance::Incandescent => Some("incandescent"),
            WhiteBalance::Fluorescent => Some("fluorescent"),
            WhiteBalance::WarmFluorescent => Some("warm_fluorescent"),
            WhiteBalance::Daylight => Some("daylight"),
            WhiteBalance::CloudyDaylight => Some("cloudy_daylight"),
            WhiteBalance::Twilight => Some("twilight"),
            WhiteBalance::Shade => Some("shade"),
            WhiteBalance::Unspecified => None,
        })
        .collect();
    let ab: Vec<&str> = c
        .antibandings
        .iter()
        .filter_map(|&a| match Antibanding::try_from(a).ok()? {
            Antibanding::Auto => Some("auto"),
            Antibanding::Off => Some("off"),
            Antibanding::Antibanding50hz => Some("50hz"),
            Antibanding::Antibanding60hz => Some("60hz"),
            Antibanding::Unspecified => None,
        })
        .collect();
    json!({
        "lens": lens, "focus_manual": c.focus_manual, "focus_lock": c.focus_lock,
        "exposure_manual": c.exposure_manual, "iso_min": c.iso_min, "iso_max": c.iso_max,
        "exposure_us_min": c.exposure_us_min, "exposure_us_max": c.exposure_us_max,
        "ev_min": c.ev_min, "ev_max": c.ev_max, "ev_step": c.ev_step, "ae_lock": c.ae_lock,
        "awb_lock": c.awb_lock, "white_balances": wb, "antibandings": ab,
        "stabilization": c.stabilization, "zoom_max": c.zoom_max,
    })
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

impl Inner {
    fn zoom(&self) -> f32 {
        f32::from_bits(self.zoom_bits.load(Ordering::SeqCst))
    }

    fn install_info(&self, force: bool) -> webcam::InstallInfo {
        let mut c = self.install_cache.lock().unwrap_or_else(|e| e.into_inner());
        if !force {
            if let Some((t, info)) = &*c {
                if t.elapsed() < Duration::from_secs(5) {
                    return info.clone();
                }
            }
        }
        let info = webcam::install_info();
        *c = Some((Instant::now(), info.clone()));
        info
    }

    fn send_control(&self, ctl: CameraControl) -> bool {
        let session = self.active.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match session {
            Some(s) => s.try_send_msg(Msg::CameraControl(ctl)).is_ok(),
            None => false,
        }
    }

    fn set_torch(&self, on: bool) {
        self.torch.store(on, Ordering::SeqCst);
        self.send_control(CameraControl { torch: Some(on), ..Default::default() });
    }

    fn set_zoom(&self, z: f32) -> f32 {
        let z = if z.is_finite() { z.clamp(nd_core::camera::MIN_ZOOM, nd_core::camera::MAX_ZOOM) } else { 1.0 };
        self.zoom_bits.store(z.to_bits(), Ordering::SeqCst);
        self.send_control(CameraControl { zoom_ratio: Some(z), ..Default::default() });
        z
    }
}

impl CameraActions for CameraManager {
    fn toggle(&self) -> Result<bool, String> {
        let real = self.inner.published.lock().unwrap_or_else(|e| e.into_inner()).real_consumer.is_some();
        let on = if real {
            // An app is using the camera: pause = placeholder, resume = frames again.
            let paused = !self.inner.paused.load(Ordering::SeqCst);
            self.inner.paused.store(paused, Ordering::SeqCst);
            !paused
        } else {
            let on = !self.inner.test_on.load(Ordering::SeqCst);
            self.inner.test_on.store(on, Ordering::SeqCst);
            on
        };
        self.inner.wake.notify_one();
        Ok(on)
    }

    fn switch_lens(&self) -> Result<bool, String> {
        let next = (self.inner.settings)().lens.toggled();
        (self.inner.set_lens)(next);
        self.inner.wake.notify_one();
        Ok(next == CameraLens::Front)
    }

    fn toggle_torch(&self) -> Result<bool, String> {
        let on = !self.inner.torch.load(Ordering::SeqCst);
        self.inner.set_torch(on);
        Ok(on)
    }

    fn zoom_by(&self, delta: f32) -> Result<f32, String> {
        Ok(self.inner.set_zoom(self.inner.zoom() + delta))
    }

    fn snapshot(&self) -> CameraSnapshot {
        let real = self.inner.published.lock().unwrap_or_else(|e| e.into_inner()).real_consumer.is_some();
        let paused = self.inner.paused.load(Ordering::SeqCst);
        CameraSnapshot {
            active: self.inner.test_on.load(Ordering::SeqCst) || (real && !paused),
            torch: self.inner.torch.load(Ordering::SeqCst),
            front_lens: (self.inner.settings)().lens == CameraLens::Front,
            zoom: self.inner.zoom(),
        }
    }
}

// ------------------------------------------------------------------------------------ driver

struct Driver {
    inner: Arc<Inner>,
    machine: Machine,
    pipeline: Option<Pipeline>,
    next_shm_poll: Instant,
    last_keyframe: Instant,
    controls_applied: bool,
    /// The image settings the active phone camera was last given (CameraStart or CameraControl).
    image_sent: Option<CameraImage>,
    rate_at: Instant,
    rate_prev: (u64, u64, u64, u64),
    rates: RateView,
    consumer_gone_since: Option<Instant>,
}

/// The consumer described by the shared memory, if its heartbeat is fresh.
pub fn consumer_from_shm(shm: &dyn HasShm) -> Option<(u32, u32, u32)> {
    let s = shm.shm();
    if !s.is_valid() {
        return None;
    }
    let (w, h, fps) = s.wanted()?;
    let beat = s.header().consumer_heartbeat_ms.load(Ordering::Relaxed);
    if nd_vcam::shm::now_ms().saturating_sub(beat) > CONSUMER_STALE_MS || w < 2 || h < 2 {
        return None;
    }
    Some((w.min(nd_vcam::shm::MAX_WIDTH), h.min(nd_vcam::shm::MAX_HEIGHT), if fps == 0 { 30 } else { fps }))
}

impl Driver {
    fn new(inner: Arc<Inner>) -> Self {
        let now = Instant::now();
        Driver {
            inner,
            machine: Machine::default(),
            pipeline: None,
            next_shm_poll: now,
            last_keyframe: now - Duration::from_secs(5),
            controls_applied: false,
            image_sent: None,
            rate_at: now,
            rate_prev: (0, 0, 0, 0),
            rates: RateView::default(),
            consumer_gone_since: None,
        }
    }

    async fn run(mut self, mut media_rx: mpsc::Receiver<MediaStream>) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut media_open = true;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.inner.wake.notified() => {}
                ms = media_rx.recv(), if media_open => match ms {
                    Some(ms) => self.on_media(ms),
                    None => media_open = false,
                },
            }
            self.step(Instant::now()).await;
        }
    }

    fn on_media(&mut self, ms: MediaStream) {
        let wanted = self.machine.running.as_ref().is_some_and(|r| r.session_id == ms.session_id);
        if !wanted {
            info!(session = %ms.session_id, "dropping unexpected media connection");
            return; // dropping closes it
        }
        info!(session = %ms.session_id, link = ?ms.link, "media connection attached");
        if let Some(old) = self.pipeline.take() {
            old.shutdown();
        }
        self.controls_applied = false;
        self.pipeline = Some(Pipeline::spawn(ms, self.inner.ctx.clone()));
    }

    fn real_consumer(&mut self, now: Instant) -> Option<(u32, u32, u32)> {
        let inner = &self.inner;
        if !inner.ctx.shm.is_set() && now >= self.next_shm_poll {
            self.next_shm_poll = now + SHM_POLL;
            if let Some(s) = (inner.provider)() {
                info!("shared memory region found");
                inner.ctx.shm.set(s);
            }
        }
        let from_shm = inner.ctx.shm.get().and_then(|s| consumer_from_shm(&*s));
        from_shm.or(inner.fake_consumer.map(|(w, h)| (w, h, 30)))
    }

    fn phone_view(&self) -> (Option<PhoneView>, Option<PhoneInfo>, Option<SessionHandle>) {
        let sessions: Vec<SessionHandle> = self
            .inner
            .server
            .all_sessions()
            .into_iter()
            .filter(|s| s.is_connected() && s.capabilities().contains(&Capability::Camera))
            .collect();
        let active_id = self.inner.server.active_session().map(|s| s.session_id().to_string());
        let cands: Vec<Candidate> = sessions
            .iter()
            .map(|s| Candidate {
                session_id: s.session_id().to_string(),
                link: s.link(),
                active: Some(s.session_id()) == active_id.as_deref(),
            })
            .collect();
        let Some(i) = choose_session(&cands) else { return (None, None, None) };
        let s = sessions[i].clone();
        let status = s.last_camera_status();
        let (modes, lens_caps, state, error) = match &status {
            Some(st) => (
                st.supported_modes.clone(),
                st.lens_caps.clone(),
                match CameraState::try_from(st.state) {
                    Ok(CameraState::Stopped) => PhoneCam::Stopped,
                    Ok(CameraState::Starting) => PhoneCam::Starting,
                    Ok(CameraState::Streaming) => PhoneCam::Streaming,
                    Ok(CameraState::Throttled) => PhoneCam::Throttled,
                    Ok(CameraState::Error) => PhoneCam::Error,
                    _ => PhoneCam::Unknown,
                },
                st.error.clone(),
            ),
            None => (Vec::new(), Vec::new(), PhoneCam::Unknown, String::new()),
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
            PhoneView { session_id: s.session_id().to_string(), link: s.link(), modes: modes.clone(), state, error };
        let info = PhoneInfo { session_id: s.session_id().to_string(), name, link: s.link(), modes, lens_caps, state };
        (Some(view), Some(info), Some(s))
    }

    async fn step(&mut self, now: Instant) {
        let settings = (self.inner.settings)();
        let real = self.real_consumer(now);

        // A paused flag belongs to one app session: forget it once the app has really left.
        if real.is_none() {
            let since = *self.consumer_gone_since.get_or_insert(now);
            if now.duration_since(since) >= logic::CONSUMER_GRACE {
                self.inner.paused.store(false, Ordering::SeqCst);
            }
        } else {
            self.consumer_gone_since = None;
        }
        let paused = self.inner.paused.load(Ordering::SeqCst);
        let test = self.inner.test_on.load(Ordering::SeqCst).then_some(TEST_SIZE);
        let want_real = real.filter(|_| settings.auto_start && !paused);
        let want = want_real.or(test).map(|(w, h, _)| (w, h));
        if let Some((w, h, _)) = real.or(test) {
            self.inner.ctx.target.set(w, h);
        }
        self.inner.ctx.write_enabled.store(want.is_some(), Ordering::Relaxed);

        let (phone_view, phone_info, session) = self.phone_view();
        let (media_ended, frames_flowing, dec_err) = match &self.pipeline {
            Some(p) => (
                p.ended.load(Ordering::Relaxed) && p.decoder_error().is_none(),
                p.stats.frames_decoded.load(Ordering::Relaxed) > 0,
                p.decoder_error(),
            ),
            None => (false, false, None),
        };
        let input = Input {
            want,
            phone: phone_view.as_ref(),
            settings: &settings,
            media_ended,
            frames_flowing,
            decoder_error: dec_err.as_deref(),
        };
        let effects = self.machine.step(now, &input);
        for fx in effects {
            self.apply(fx).await;
        }

        // Once frames flow, restore torch / zoom the user already chose.
        if self.machine.phase == Phase::Streaming && !self.controls_applied {
            self.controls_applied = true;
            let (torch, zoom) = (self.inner.torch.load(Ordering::SeqCst), self.inner.zoom());
            if torch || zoom > 1.0 {
                self.inner.send_control(CameraControl {
                    torch: torch.then_some(true),
                    zoom_ratio: (zoom > 1.0).then_some(zoom),
                    ..Default::default()
                });
            }
        }

        // Live image settings: send the whole set once whenever it differs from what the phone has.
        if self.machine.phase == Phase::Streaming {
            let want = logic::image_proto(&settings.image);
            if self.image_sent.as_ref() != Some(&want)
                && self.inner.send_control(CameraControl { image: Some(want), ..Default::default() })
            {
                self.image_sent = Some(want);
            }
        }

        // Key frame requests (fresh attach, dropped packets, decode errors), at most 1/s.
        if let Some(p) = &self.pipeline {
            if p.need_keyframe.load(Ordering::Relaxed) && self.last_keyframe.elapsed() >= Duration::from_secs(1) {
                if let Some(s) = self.inner.active.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                    if s.try_send_msg(Msg::CameraControl(CameraControl {
                        request_keyframe: true,
                        ..Default::default()
                    }))
                    .is_ok()
                    {
                        p.need_keyframe.store(false, Ordering::Relaxed);
                        self.last_keyframe = Instant::now();
                    }
                }
            }
        }

        self.update_rates(now);
        let readout =
            session.as_ref().filter(|_| self.machine.phase == Phase::Streaming).and_then(|s| s.last_camera_readout());
        let mut published = Published {
            readout,
            phase: self.machine.phase,
            message: self.machine.message.clone(),
            real_consumer: real,
            phone: phone_info,
            running: self.machine.running.clone(),
            rates: self.rates.clone(),
            ..Published::default()
        };
        if let Some(p) = &self.pipeline {
            published.decode_ms = p.stats.decode_us.load(Ordering::Relaxed) as f64 / 1000.0;
            published.age_ms = p.stats.age_us.load(Ordering::Relaxed) as f64 / 1000.0;
            published.dropped = p.stats.dropped.load(Ordering::Relaxed);
            published.decode_errors = p.stats.decode_errors.load(Ordering::Relaxed);
        }
        *self.inner.published.lock().unwrap_or_else(|e| e.into_inner()) = published;
    }

    fn update_rates(&mut self, now: Instant) {
        let dt = now.duration_since(self.rate_at).as_secs_f64();
        if dt < 1.0 {
            return;
        }
        let cur = match &self.pipeline {
            Some(p) => (
                p.stats.packets_in.load(Ordering::Relaxed),
                p.stats.frames_out.load(Ordering::Relaxed),
                p.stats.bytes_in.load(Ordering::Relaxed),
                0,
            ),
            None => (0, 0, 0, 0),
        };
        let d = |a: u64, b: u64| a.saturating_sub(b) as f64;
        self.rates = RateView {
            fps_in: d(cur.0, self.rate_prev.0) / dt,
            fps_out: d(cur.1, self.rate_prev.1) / dt,
            kbps: d(cur.2, self.rate_prev.2) * 8.0 / 1000.0 / dt,
        };
        self.rate_prev = cur;
        self.rate_at = now;
    }

    async fn apply(&mut self, fx: Effect) {
        match fx {
            Effect::Start { session_id, start } => {
                let Some(s) = self.session(&session_id) else { return };
                info!(
                    "camera start: {}x{}@{} {} kbit/s lens={} mirror={}",
                    start.width,
                    start.height,
                    start.fps,
                    start.bitrate_bps / 1000,
                    start.lens,
                    start.mirror
                );
                *self.inner.active.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.clone());
                self.controls_applied = false;
                self.image_sent = start.image;
                if let Err(e) = s.camera_start(start).await {
                    warn!("CameraStart failed: {e}");
                }
            }
            Effect::Stop { session_id } => {
                info!("camera stop");
                if let Some(p) = self.pipeline.take() {
                    p.shutdown();
                }
                self.inner.ctx.preview.clear();
                self.image_sent = None;
                *self.inner.active.lock().unwrap_or_else(|e| e.into_inner()) = None;
                if let Some(s) = self.session(&session_id) {
                    if let Err(e) = s.camera_stop().await {
                        tracing::debug!("CameraStop skipped: {e}");
                    }
                }
                if self.machine.running.is_none() {
                    // The phone turns the torch off with the camera; forget zoom/torch too.
                    self.inner.torch.store(false, Ordering::SeqCst);
                    self.inner.zoom_bits.store(1.0f32.to_bits(), Ordering::SeqCst);
                }
            }
            Effect::SetLens { session_id, lens } => {
                if let Some(s) = self.session(&session_id) {
                    info!(lens = ?lens, "camera lens change");
                    let ctl = CameraControl { lens: Some(lens as i32), ..Default::default() };
                    if let Err(e) = s.camera_control(ctl).await {
                        warn!("lens change failed: {e}");
                    }
                }
            }
        }
    }

    fn session(&self, id: &str) -> Option<SessionHandle> {
        self.inner.server.all_sessions().into_iter().find(|s| s.session_id() == id && s.is_connected())
    }
}
