// SPDX-License-Identifier: GPL-3.0-or-later
//! Agent lifecycle for the desktop app (round 4 contract, see docs/PLAN.md).
//!
//! [`start`] runs the whole agent (transport, config server, live ticker) on its own tokio
//! runtime in background threads and returns an [`AgentHandle`]. The main thread stays free
//! for the GUI event loop (`gui::run`), or just waits in `--headless` mode.

use crate::camera::{self, CameraManager};
use crate::config_backend::{AgentConfigBackend, BackendCtx, Shared};
use crate::devices::DeviceRegistry;
use crate::discovery::{self, Advert, DiscoveryStatus};
use crate::mic::{ManagerConfig as MicConfig, MicManager};
use crate::settings::SettingsStore;
use crate::{instance, logging, AgentState, StateEvent};
use anyhow::{Context, Result};
use nd_config::{Access, AdbInfo, BackendError, ConfigBackend, Problem, Settings, SettingsPatch};
use nd_core::{ActionRouter, InputReaction, LiveProviders, ProfileStore};
use nd_platform::{native, KeyboardAccess};
use nd_transport::{
    AdbClient, AdbTracker, ControlServer, HostIdentity, Link, ServerHandle, TransportEvent, TrustStore,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, watch, Notify};
use tracing::{error, info, warn};

/// Startup options (CLI flags / env overrides resolved by `main`).
#[derive(Debug, Clone)]
pub struct AgentOptions {
    pub control_port: u16,
    pub ui_port: u16,
    /// Don't run the adb tracker (`NOTIDECK_NO_ADB`).
    pub no_adb: bool,
}

/// A connected phone, as shown in the tray status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSummary {
    pub name: String,
    /// "usb" or "lan".
    pub transport: String,
}

/// Snapshot published whenever devices or problems change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentStatus {
    pub devices: Vec<DeviceSummary>,
    pub problems: Vec<nd_config::Problem>,
}

/// Requests for the app shell coming from the agent side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// Show and focus the main window (second launch, `POST /api/shell/show`).
    Show,
    /// An unknown phone wants to connect over USB; carries its name. The shell should surface
    /// the window so the user can press Allow (there is no toast API in the tray stack).
    PairingPending(String),
    /// Quit the whole app (`POST /api/shell/quit`, `--quit`).
    Quit,
}

/// Handle to a running agent. Dropping it does not stop the agent; call [`AgentHandle::shutdown`].
pub struct AgentHandle {
    pub(crate) status: watch::Receiver<AgentStatus>,
    pub(crate) shell: broadcast::Sender<ShellEvent>,
    pub(crate) ui_url: String,
    pub(crate) backend: Arc<dyn ConfigBackend>,
    pub(crate) stop: Option<Box<dyn FnOnce() + Send>>,
}

impl AgentHandle {
    /// Live status for the tray.
    pub fn status(&self) -> watch::Receiver<AgentStatus> {
        self.status.clone()
    }

    /// Tokenised config UI URL (`http://127.0.0.1:<port>/?token=…`) for the webview.
    pub fn ui_url(&self) -> String {
        self.ui_url.clone()
    }

    /// Shell requests (e.g. show window).
    pub fn shell_events(&self) -> broadcast::Receiver<ShellEvent> {
        self.shell.subscribe()
    }

    /// Current persisted settings (for the tray "Start at login" check and the window theme).
    pub fn settings(&self) -> Settings {
        self.backend.settings()
    }

    /// Apply and persist settings (same path as `PUT /api/settings`). Blocking; may touch the registry.
    pub fn update_settings(&self, patch: SettingsPatch) -> Result<Settings, BackendError> {
        self.backend.update_settings(patch)
    }

    /// Stop all agent tasks and the runtime; blocks until they are down (bounded, ~2 s).
    pub fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

/// Where the shared memory region comes from: the DLL's section on Windows (polled until it
/// exists), an in-memory slot elsewhere. The slot is also returned so the Linux v4l2 sink reads
/// the very same `MemShm` (`None` on Windows).
fn shm_provider() -> (camera::ShmProvider, Option<camera::webcam::VcamMem>) {
    #[cfg(windows)]
    {
        (Arc::new(|| nd_vcam::ShmMapping::open().map(|m| Arc::new(m) as Arc<dyn nd_vcam::HasShm>)), None)
    }
    #[cfg(not(windows))]
    {
        let mem = nd_vcam::MemShm::new();
        let sink_mem = mem.clone();
        (Arc::new(move || Some(Arc::new(mem.clone()) as Arc<dyn nd_vcam::HasShm>)), Some(sink_mem))
    }
}

/// How often problems (adb, keyboard access) are re-probed.
const PROBE_INTERVAL: Duration = Duration::from_secs(10);

/// `preferred` if it is free on 127.0.0.1, otherwise an OS-assigned free port.
fn pick_port(preferred: u16, what: &str) -> Result<u16> {
    if std::net::TcpListener::bind(("127.0.0.1", preferred)).is_ok() {
        return Ok(preferred);
    }
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .with_context(|| format!("no free port for the {what} server"))?
        .port();
    warn!("{what} port {preferred} is in use (another Notideck agent?); using {port}");
    Ok(port)
}

/// `Appearance` message telling the phone which deck theme and sleep behaviour to use.
pub(crate) fn appearance_msg(settings: &nd_config::Settings) -> nd_proto::Msg {
    use nd_proto::v1::{Appearance, DeckSleep, PhoneTheme as P};
    let sleep = settings.deck_sleep();
    let theme = match settings.phone_theme {
        nd_config::PhoneTheme::Black => P::Black,
        nd_config::PhoneTheme::Graphite => P::Graphite,
        nd_config::PhoneTheme::Cream => P::Cream,
        nd_config::PhoneTheme::Lavender => P::Lavender,
        nd_config::PhoneTheme::Mint => P::Mint,
        nd_config::PhoneTheme::Dusk => P::Dusk,
        nd_config::PhoneTheme::Tide => P::Tide,
    };
    nd_proto::Msg::Appearance(Appearance {
        theme: theme as i32,
        sleep: Some(DeckSleep {
            enabled: sleep.enabled,
            idle_s: sleep.idle_s,
            wake_on_camera_mic: sleep.wake_on_camera_mic,
            show_clock: sleep.show_clock,
        }),
    })
}

pub(crate) fn map_access(k: KeyboardAccess) -> Access {
    match k {
        KeyboardAccess::Ok => Access::Ok,
        KeyboardAccess::Missing => Access::Missing,
        KeyboardAccess::Unsupported => Access::Unsupported,
    }
}

/// Pure problem list. `adb_error` is `Some` when `adb version` failed (`None` = fine or not probed).
pub fn compute_problems(adb_error: Option<&str>, keyboard: KeyboardAccess, audio_backend: &str) -> Vec<Problem> {
    let mut out = Vec::new();
    if let Some(err) = adb_error {
        out.push(Problem {
            code: "adb_missing".into(),
            message: format!(
                "adb was not found ({err}). USB is disabled until Android platform-tools are available; \
                 place adb next to Notideck or set ADB_PATH. The phone can still connect over the LAN."
            ),
            fix: None,
        });
    }
    if keyboard == KeyboardAccess::Missing {
        out.push(Problem {
            code: "keyboard_access".into(),
            message: "Notideck cannot send keystrokes yet: /dev/uinput is not accessible to your user.".into(),
            fix: Some("keyboard_access".into()),
        });
    }
    if audio_backend == "none" {
        out.push(Problem {
            code: "audio_unavailable".into(),
            message: "No audio backend was found, so volume and mute controls are disabled.".into(),
            fix: None,
        });
    }
    out
}

/// Push icons of the active profile that this session has not received yet.
async fn push_icons(handle: &ServerHandle, state: &AgentState, sent: &Mutex<HashSet<String>>) {
    let fresh: Vec<_> = {
        let mut sent = sent.lock().unwrap_or_else(|e| e.into_inner());
        state.active_icon_messages().into_iter().filter(|m| sent.insert(m.hash.clone())).collect()
    };
    for icon in fresh {
        if let Err(e) = handle.put_icon(icon).await {
            warn!("Failed to push icon: {e}");
        }
    }
}

fn summarize(
    handle: &ServerHandle,
    extra: Option<&nd_transport::SessionHandle>,
    gone: Option<&str>,
) -> Vec<DeviceSummary> {
    let mut sessions: Vec<_> = handle.all_sessions().into_iter().filter(|s| s.is_connected()).collect();
    if let Some(e) = extra {
        if e.is_connected() && !sessions.iter().any(|s| s.session_id() == e.session_id()) {
            sessions.push(e.clone());
        }
    }
    sessions
        .iter()
        .filter(|s| Some(s.session_id()) != gone)
        .map(|s| DeviceSummary { name: s.device_model().to_string(), transport: s.transport().to_string() })
        .collect()
}

/// Keeps `AgentStatus` current: device changes from transport events, problems on a timer
/// (and immediately when `shared.refresh` is poked).
async fn status_task(
    server: ServerHandle,
    tx: watch::Sender<AgentStatus>,
    shared: Arc<Shared>,
    adb: Option<AdbClient>,
    audio_backend: &'static str,
    camera: Arc<CameraManager>,
) {
    let mut events = server.subscribe_events();
    let mut ticker = tokio::time::interval(PROBE_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut devices = Vec::new();
    let mut probe = true;
    loop {
        if probe {
            let mut adb_err = None;
            if let Some(client) = &adb {
                let info = match client.version().await {
                    Ok(v) => AdbInfo { found: true, path: Some(client.path().display().to_string()), version: Some(v) },
                    Err(e) => {
                        adb_err = Some(e.to_string());
                        AdbInfo { found: false, path: Some(client.path().display().to_string()), version: None }
                    }
                };
                *shared.adb.lock().unwrap_or_else(|e| e.into_inner()) = info;
            }
            let mut problems = compute_problems(adb_err.as_deref(), native::keyboard_access(), audio_backend);
            problems.extend(camera.problems());
            *shared.problems.lock().unwrap_or_else(|e| e.into_inner()) = problems.clone();
            tx.send_if_modified(|st| {
                let changed = st.problems != problems;
                st.problems = problems;
                changed
            });
        }
        probe = false;
        let (mut extra, mut gone) = (None, None);
        tokio::select! {
            _ = ticker.tick() => { probe = true; }
            _ = shared.refresh.notified() => { probe = true; }
            _ = camera.problems_changed().notified() => { probe = true; }
            ev = events.recv() => match ev {
                Ok(TransportEvent::SessionConnected(s)) => extra = Some(s),
                Ok(TransportEvent::SessionDisconnected(id)) => gone = Some(id),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
        let next = summarize(&server, extra.as_ref(), gone.as_deref());
        if next != devices {
            devices = next.clone();
            tx.send_if_modified(|st| {
                let changed = st.devices != next;
                st.devices = next;
                changed
            });
        }
    }
}

/// Start the agent on its own runtime thread. Returns once the control and UI ports are bound.
pub fn start(opts: AgentOptions) -> Result<AgentHandle> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("nd-agent-rt")
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    let _guard = rt.enter();

    // Platform backends.
    let injector = native::input().context("input injector")?;
    let launcher: Arc<dyn nd_platform::Launcher> = native::launcher().context("launcher")?.into();
    // Audio is shared by the router (dial/slider/mute actions) and the live ticker (readback).
    let audio: Arc<dyn nd_platform::AudioControl> = native::audio().into();
    let media: Arc<dyn nd_platform::MediaInfo> = if std::env::var_os("NOTIDECK_FAKE_MEDIA").is_some() {
        warn!("NOTIDECK_FAKE_MEDIA is set: using a fake media player");
        Arc::new(crate::fake_media::FakeMedia::new())
    } else {
        native::media().into()
    };
    let audio_backend = audio.backend_name();
    let media_backend = media.backend_name();

    // Profile store, settings.
    let store = ProfileStore::open_default();
    let config_dir = store.root().to_path_buf();
    info!("Config directory: {}", config_dir.display());
    let state = AgentState::open(store)?;
    info!(profile = %state.active_profile_id(), "Active profile loaded");
    let exe = std::env::current_exe().unwrap_or_else(|_| "notideck".into());
    let settings = Arc::new(SettingsStore::open(config_dir.clone(), native::autostart(&exe, &["--minimized"])));
    let watcher = state.spawn_watcher();
    let autoswitch = crate::autoswitch::AutoSwitchService::start(config_dir.clone(), state.clone(), native::focus());
    let sent_icons: Arc<Mutex<HashSet<String>>> = Arc::default();

    // The phone only needs CONTROL_PORT on *its* side (adb reverse maps it), and the window
    // loads whatever UI URL we hand it, so a busy default port (e.g. a dev agent in WSL, whose
    // ports WSL forwards to Windows) just means we pick a free one.
    let mut opts = opts;
    opts.control_port = pick_port(opts.control_port, "control")?;
    opts.ui_port = pick_port(opts.ui_port, "config UI")?;

    // Identity, pinned-device registry and LAN mode. LAN off = loopback only (USB), which also
    // avoids the Windows Firewall prompt. The listener cannot be rebound at runtime, so
    // `lan_listening` is fixed for this run (see BackendCtx::lan_listening).
    let identity = Arc::new(HostIdentity::load_or_create(&config_dir).context("host identity")?);
    info!(host_id = %identity.host_id(), fp = %identity.fingerprint().short(), "Host identity loaded");
    let registry = Arc::new(DeviceRegistry::open(&config_dir, identity.fingerprint()));
    settings.migrate_onboarding(!registry.list().is_empty());
    let lan_listening = settings.get().lan_enabled;
    let bind_ip = if lan_listening { "0.0.0.0" } else { "127.0.0.1" };
    let trust: Arc<dyn TrustStore> = registry.clone();

    // Bind the control port (errors surface here).
    let mut server = rt
        .block_on(ControlServer::bind_tls((bind_ip, opts.control_port), identity.clone(), trust))
        .with_context(|| format!("cannot listen on control port {}", opts.control_port))?;
    // Use the port the socket really got: under WSL's NAT a port that Windows still holds can be
    // silently remapped to an ephemeral one, and mDNS, adb reverse and the QR must match it.
    opts.control_port = server.local_addr().port();
    let mut input_rx = server.take_input_receiver().context("input receiver")?;
    let media_rx = server.take_media_receiver().context("media receiver")?;
    let audio_rx = server.take_audio_receiver().context("audio receiver")?;
    let server_handle = server.handle();
    let (_server_task, server_shutdown) = server.spawn();
    info!("Notideck ControlServer (TLS) listening on {bind_ip}:{}", opts.control_port);

    // Webcam: consumer detection, phone camera, decode -> shared memory (ADR 0005).
    let cam_settings = settings.clone();
    let cam_lens_settings = settings.clone();
    let (shm_provider, vcam_mem) = shm_provider();
    let camera = camera::CameraManager::start(camera::ManagerConfig {
        server: server_handle.clone(),
        media_rx,
        settings: Arc::new(move || cam_settings.get().camera()),
        set_lens: Arc::new(move |lens| {
            if let Err(e) = cam_lens_settings.update(SettingsPatch { camera_lens: Some(lens), ..Default::default() }) {
                warn!("cannot save the camera lens: {e}");
            }
        }),
        shm_provider,
        fake_consumer: std::env::var("NOTIDECK_VCAM_FAKE_CONSUMER").ok().and_then(|v| {
            let c = camera::parse_fake_consumer(&v);
            if c.is_none() {
                warn!("NOTIDECK_VCAM_FAKE_CONSUMER={v:?} is not WIDTHxHEIGHT; ignored");
            }
            c
        }),
        decoder_factory: None,
        manage_vcam: cfg!(any(windows, target_os = "linux")),
        vcam_mem,
    });
    // Poked to re-sample live data now (layout change, mixer config edit, OBS event, mic state...).
    let live_poke = Arc::new(Notify::new());
    // Phone microphone (round 25): phone PCM -> virtual cable / virtual source.
    let mic_settings = settings.clone();
    let mic_enabled_settings = settings.clone();
    let mic = MicManager::start(MicConfig {
        server: server_handle.clone(),
        audio_rx,
        sink: native::mic_sink().into(),
        settings: Arc::new(move || mic_settings.get().mic()),
        set_enabled: Arc::new(move |on| {
            if let Err(e) = mic_enabled_settings.update(SettingsPatch { mic_enabled: Some(on), ..Default::default() }) {
                warn!("cannot save the microphone switch: {e}");
            }
        }),
        poke: live_poke.clone(),
        config_dir: config_dir.clone(),
        timing: Default::default(),
    });
    // Per-app mixer: focus = the auto-switch service's focused process, icons go into the icon store.
    let (focus_svc, icon_state) = (autoswitch.clone(), state.clone());
    let icon_importer: nd_core::mixer::IconImporter = Arc::new(move |png| icon_state.put_icon_bytes(png).ok());
    let media_for_mixer = media.clone();
    let mixer = Arc::new(
        nd_core::Mixer::new(app_audio_backend(), audio.clone())
            .with_focus(Arc::new(move || focus_svc.focused_process()))
            .with_media_app(Arc::new(move || {
                media_for_mixer.now_playing().ok().flatten().map(|np| np.app).filter(|a| !a.is_empty())
            }))
            .with_icons(icon_importer.clone()),
    );
    mixer.set_config(crate::mixer_cfg::load(&config_dir));
    let (obs, fake_obs) = start_obs(&rt, &config_dir, &live_poke);
    // Plugins (round 19): started inside the runtime; the host doubles as the router's `PluginControl`.
    let plugins = crate::plugins::start(&exe, &config_dir, icon_importer.clone(), &live_poke);
    let (plugin_host, plugin_events) = match plugins {
        Some(p) => (Some(p.host), Some(p.events)),
        None => (None, None),
    };
    let plugin_control: Arc<dyn nd_core::plugin::PluginControl> = match &plugin_host {
        Some(h) => crate::plugins::as_control(h),
        None => Arc::new(nd_core::NullPlugins),
    };
    if let Some(host) = plugin_host.clone() {
        autoswitch.set_on_focus(move |process, title| host.focus_changed(process, title));
    }
    let router = ActionRouter::new(injector.into(), launcher.clone())
        .with_plugins(plugin_control.clone())
        .with_obs(obs.clone())
        .with_audio(audio.clone())
        .with_mixer(mixer.clone())
        .with_media(media.clone())
        .with_camera(camera.clone())
        .with_mic(mic.clone())
        .with_profile_actions(autoswitch.clone());
    let providers = LiveProviders {
        audio,
        media,
        stats: native::stats().into(),
        camera: camera.clone(),
        mic: mic.clone(),
        profile_actions: autoswitch.clone(),
        mixer: mixer.clone(),
        obs: obs.clone(),
        icons: Some(icon_importer),
    };

    // mDNS advertisement, only while LAN mode is effective.
    let (lan_tx, lan_rx) = watch::channel(lan_listening);
    let discovery_status = Arc::new(DiscoveryStatus::default());
    let discovery_task = rt.spawn(discovery::run(
        Advert {
            host_id: identity.host_id().to_string(),
            host_name: nd_transport::get_host_name(),
            fp: identity.fingerprint(),
            port: opts.control_port,
        },
        lan_rx,
        discovery_status.clone(),
        crate::pairing::host_addrs,
    ));

    // Check the UI port is free before handing it to the server task.
    drop(
        std::net::TcpListener::bind(("127.0.0.1", opts.ui_port))
            .with_context(|| format!("cannot listen on UI port {}", opts.ui_port))?,
    );

    // adb tracker (the phone always dials CONTROL_PORT on its side).
    let adb_shutdown = if opts.no_adb {
        info!("NOTIDECK_NO_ADB set; not touching adb reverse");
        None
    } else {
        let tracker = AdbTracker::new(nd_proto::CONTROL_PORT, opts.control_port);
        info!("ADB device tracker active (reverse tcp:{} -> tcp:{})", nd_proto::CONTROL_PORT, opts.control_port);
        Some(tracker.spawn().1)
    };

    // Config backend + server.
    let (shell_tx, _) = broadcast::channel(8);
    let shared = Arc::new(Shared::default());
    let data_dir = instance::data_dir();
    let backend: Arc<dyn ConfigBackend> =
        Arc::new(AgentConfigBackend::new(state.clone()).with_server(server_handle.clone()).with_ctx(BackendCtx {
            settings: settings.clone(),
            router: router.clone(),
            shell: shell_tx.clone(),
            shared: shared.clone(),
            control_port: opts.control_port,
            ui_port: opts.ui_port,
            log_dir: logging::log_dir(&data_dir),
            data_dir,
            config_dir,
            audio_backend,
            media_backend,
            camera: camera.clone(),
            mic: mic.clone(),
            registry,
            identity,
            lan_listening,
            lan_tx,
            discovery: discovery_status,
            autoswitch: autoswitch.clone(),
            mixer,
            obs,
            _fake_obs: fake_obs,
            live_poke: live_poke.clone(),
            plugins: plugin_host.clone(),
        }));
    let ui_token = uuid::Uuid::new_v4().simple().to_string();
    let ui_url = format!("http://127.0.0.1:{}/?token={ui_token}", opts.ui_port);
    let serve_backend = backend.clone();
    let serve_token = ui_token.clone();
    let ui_port = opts.ui_port;
    let ui_task = rt.spawn(async move {
        if let Err(e) = nd_config::serve(serve_backend, Some(serve_token), ui_port).await {
            error!("Config UI server stopped: {e}");
        }
    });
    info!("Config UI: {ui_url}");
    if let Err(e) =
        instance::write_info(&instance::data_dir(), &instance::InstanceInfo { ui_port: opts.ui_port, token: ui_token })
    {
        warn!("cannot write instance.json: {e}");
    }

    // Status for the tray.
    let (status_tx, status_rx) = watch::channel(AgentStatus::default());
    let status_handle = rt.spawn(status_task(
        server_handle.clone(),
        status_tx,
        shared,
        (!opts.no_adb).then(AdbClient::new),
        audio_backend,
        camera.clone(),
    ));

    // Live ticker: samples data sources every second, and immediately when poked.
    let live_task = rt.spawn(crate::ticks::live_ticker_with(
        state.clone(),
        server_handle.clone(),
        providers.clone(),
        live_poke.clone(),
        plugin_control,
    ));
    // Plugin requests: feedback flashes, open-url, setSettings.
    let plugin_task = plugin_events
        .map(|rx| rt.spawn(crate::plugins::event_loop(rx, state.clone(), server_handle.clone(), launcher.clone())));
    let meter_task = rt.spawn(crate::ticks::meter_ticker(state.clone(), server_handle.clone(), providers));
    let nav = crate::nav::Nav::new(state.clone(), autoswitch.clone(), server_handle.clone());
    let nav_poke = Arc::new(Notify::new());
    {
        let (poke, nav_poke) = (live_poke.clone(), nav_poke.clone());
        autoswitch.set_on_change(move || {
            poke.notify_one();
            nav_poke.notify_one();
        });
    }
    // Paused-state changes that do not come with a layout push still reach the phone's rail.
    let nav_task = {
        let nav = nav.clone();
        rt.spawn(async move {
            loop {
                nav_poke.notified().await;
                nav.publish(false).await;
            }
        })
    };

    // Transport event routing: push layout/icons on connect, answer NeedIcon.
    let mut event_rx = server_handle.subscribe_events();
    let (ev_poke, need_handle, ev_state, ev_sent, ev_settings) =
        (live_poke.clone(), server_handle.clone(), state.clone(), sent_icons.clone(), settings.clone());
    let ev_shell = shell_tx.clone();
    let ev_nav = nav.clone();
    let ev_plugins = plugin_host.clone();
    let event_loop = rt.spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            match event {
                TransportEvent::SessionConnected(session) => {
                    // Only paired sessions are ever announced, so everything below is post-Welcome.
                    if session.link() == Link::Lan && !ev_settings.get().lan_enabled {
                        session
                            .close(nd_proto::v1::GoodbyeReason::Shutdown, "Wi-Fi connections are turned off on the PC");
                        continue;
                    }
                    info!(id = %session.session_id(), model = %session.device_model(),
                        "Client connected. Pushing icons and initial layout...");
                    if let Err(e) = session.send_msg(appearance_msg(&ev_settings.get())).await {
                        warn!("Failed to send appearance: {e}");
                    }
                    ev_sent.lock().unwrap_or_else(|e| e.into_inner()).clear();
                    for icon in ev_state.active_icon_messages() {
                        let hash = icon.hash.clone();
                        if session.put_icon(icon).await.is_ok() {
                            ev_sent.lock().unwrap_or_else(|e| e.into_inner()).insert(hash);
                        }
                    }
                    match ev_state.current_layout() {
                        Ok(layout) => {
                            if let Err(e) = session.set_layout(layout).await {
                                error!("Failed to push initial layout: {e}");
                            }
                        }
                        Err(e) => error!("No layout to push: {e}"),
                    }
                    ev_nav.send_to(&session).await;
                    ev_nav.send_rail_to(&session).await;
                    ev_poke.notify_one();
                }
                TransportEvent::SessionDisconnected(id) => info!(id = %id, "Client disconnected"),
                TransportEvent::Other(env) => {
                    match env.msg {
                        Some(nd_proto::Msg::NavRequest(req)) => {
                            let nav = ev_nav.clone();
                            tokio::spawn(async move { nav.handle_request(req).await });
                        }
                        // Round 21: session_id = "{plugin}/{session}" (see HostEvent::PlayOnPhone).
                        Some(nd_proto::Msg::PlayerClosed(closed)) => {
                            if let (Some(host), Some((plugin, session))) =
                                (&ev_plugins, closed.session_id.split_once('/'))
                            {
                                host.player_closed(plugin, session);
                            }
                        }
                        _ => {}
                    }
                }
                TransportEvent::PairingPending(p) => {
                    info!(fp = %p.peer.fp.short(), name = %p.peer.device_name, "New phone is waiting for approval");
                    // No receivers (headless) is fine.
                    let _ = ev_shell.send(ShellEvent::PairingPending(p.peer.device_name.clone()));
                }
                TransportEvent::NeedIcon(need) => {
                    for icon in ev_state.put_icons_for(&need.hashes) {
                        if let Err(e) = need_handle.put_icon(icon).await {
                            warn!("Failed to answer NeedIcon: {e}");
                        }
                    }
                }
                TransportEvent::Health(health) => info!(
                    battery = format!("{}%", health.battery_percent),
                    temp = format!("{}°C", health.battery_temp_c),
                    charging = health.charging,
                    "Device health report"
                ),
                _ => {}
            }
        }
    });

    // Live mirror: re-push the layout whenever profiles change (config UI or disk edits).
    let mut state_rx = state.subscribe();
    let (mirror_state, mirror_handle, mirror_sent, mirror_poke) =
        (state.clone(), server_handle.clone(), sent_icons.clone(), live_poke.clone());
    let mirror_autoswitch = autoswitch.clone();
    let (mirror_nav, input_nav) = (nav.clone(), nav.clone());
    let mirror_loop = rt.spawn(async move {
        loop {
            match state_rx.recv().await {
                Ok(StateEvent::LayoutChanged(layout)) => {
                    push_icons(&mirror_handle, &mirror_state, &mirror_sent).await;
                    // No phone connected is expected; it gets the layout on connect.
                    mirror_nav.push_layout(layout).await;
                    mirror_poke.notify_one();
                }
                Ok(StateEvent::RailChanged(rail)) => {
                    push_icons(&mirror_handle, &mirror_state, &mirror_sent).await;
                    mirror_nav.broadcast_rail(rail).await;
                    mirror_poke.notify_one();
                }
                Ok(StateEvent::ProfilesChanged) => mirror_autoswitch.refresh_known_profiles(),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Input loop.
    let input_handle = server_handle.clone();
    let input_loop = rt.spawn(async move {
        while let Some(input) = input_rx.recv().await {
            info!(control_id = %input.control_id, kind = ?input.kind, "Received input event");
            // Actions may block (multi-action delays, subprocesses); keep them off the reactor.
            let (st, rt_router, inp) = (state.clone(), router.clone(), input.clone());
            let res = tokio::task::spawn_blocking(move || st.handle_input(&inp, &rt_router)).await;
            let res = match res {
                Ok(r) => r,
                Err(e) => {
                    error!("Input task failed: {e}");
                    continue;
                }
            };
            match res {
                Ok(InputReaction::SetLayout(layout)) => {
                    info!(page_id = %layout.page_id, "Navigating to page");
                    push_icons(&input_handle, &state, &sent_icons).await;
                    input_nav.push_layout(layout).await;
                    live_poke.notify_one();
                }
                Ok(InputReaction::UpdateControls(updates)) => {
                    if let Err(e) = input_handle.update_controls(updates).await {
                        error!("Failed to broadcast control updates: {e}");
                    }
                }
                Ok(InputReaction::None) => {}
                Err(e) => {
                    error!("Action routing error: {e}");
                    // Flash the key red so a failed action is visible on the phone.
                    let alert = nd_proto::v1::Feedback {
                        kind: nd_proto::v1::FeedbackKind::ShowAlert as i32,
                        control_id: input.control_id.clone(),
                        text: String::new(),
                    };
                    if let Err(e) = input_handle.send_feedback(alert).await {
                        tracing::debug!("Alert feedback skipped: {e}");
                    }
                }
            }
        }
    });

    // The runtime lives on its own thread until `stop` is called.
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let camera_stop = camera.clone();
    let mic_stop = mic.clone();
    let tasks = vec![
        discovery_task,
        ui_task,
        status_handle,
        live_task,
        meter_task,
        event_loop,
        mirror_loop,
        input_loop,
        watcher,
        nav_task,
    ]
    .into_iter()
    .chain(plugin_task)
    .collect::<Vec<_>>();
    drop(_guard);
    let thread = std::thread::Builder::new()
        .name("nd-agent-main".into())
        .spawn(move || {
            let _ = stop_rx.recv();
            camera_stop.shutdown();
            mic_stop.shutdown();
            autoswitch.stop();
            // Blocks up to ~4 s while plugin processes exit; the runtime is still up for their supervisors.
            if let Some(host) = &plugin_host {
                crate::plugins::shutdown(host);
            }
            let _ = server_shutdown.send(true);
            if let Some(tx) = adb_shutdown {
                let _ = tx.send(true);
            }
            for t in &tasks {
                t.abort();
            }
            rt.shutdown_timeout(Duration::from_secs(2));
            info!("Notideck agent stopped cleanly.");
        })
        .context("cannot spawn the agent thread")?;

    let stop: Box<dyn FnOnce() + Send> = Box::new(move || {
        let _ = stop_tx.send(());
        let _ = thread.join();
    });
    Ok(AgentHandle { status: status_rx, shell: shell_tx, ui_url, backend, stop: Some(stop) })
}

/// Start the OBS client. `NOTIDECK_FAKE_OBS=1` (dev/e2e only) runs an in-process fake OBS on a free port
/// (no password) and points the client at it, ignoring (and not touching) `obs.json`.
fn start_obs(
    rt: &tokio::runtime::Runtime,
    config_dir: &std::path::Path,
    poke: &Arc<Notify>,
) -> (Arc<nd_obs::ObsClient>, Option<nd_obs::FakeObs>) {
    let on_change: Arc<dyn Fn() + Send + Sync> = {
        let poke = poke.clone();
        Arc::new(move || poke.notify_one())
    };
    let mut fake = None;
    let mut settings = crate::obs_cfg::load(config_dir);
    if std::env::var_os("NOTIDECK_FAKE_OBS").is_some() {
        match rt.block_on(nd_obs::FakeObs::start(0, None)) {
            Ok(f) => {
                let port = f.addr().port();
                warn!("NOTIDECK_FAKE_OBS is set: using a fake OBS on 127.0.0.1:{port} (obs.json ignored)");
                settings = nd_obs::ObsSettings {
                    enabled: true,
                    auto_detect: false,
                    host: "127.0.0.1".into(),
                    port,
                    password: String::new(),
                };
                fake = Some(f);
            }
            Err(e) => warn!("NOTIDECK_FAKE_OBS: cannot start the fake OBS: {e}"),
        }
    }
    (Arc::new(nd_obs::ObsClient::start(settings, on_change)), fake)
}

/// The per-app audio backend. `NOTIDECK_FAKE_MIXER=1` (dev/e2e only) swaps in three fake apps with moving peaks.
fn app_audio_backend() -> Arc<dyn nd_platform::AppAudio> {
    if std::env::var_os("NOTIDECK_FAKE_MIXER").is_none() {
        return native::app_audio().into();
    }
    use nd_core::mock::MockAppAudio;
    warn!("NOTIDECK_FAKE_MIXER is set: using fake mixer apps");
    let fake = MockAppAudio::new();
    fake.set_sessions(vec![
        MockAppAudio::session("1", "spotify.exe", "Spotify", 0.7),
        MockAppAudio::session("2", "chrome.exe", "Google Chrome", 0.45),
        MockAppAudio::session("3", "discord.exe", "Discord", 1.0),
    ]);
    let animated = fake.clone();
    std::thread::spawn(move || {
        let mut t = 0.0f32;
        loop {
            t += 0.08;
            animated.set_peak("1", (0.55 + 0.4 * (t * 3.1).sin()).max(0.0));
            animated.set_peak("2", 0.0);
            animated.set_peak("3", ((t * 1.3).sin() * (t * 7.0).sin()).abs());
            std::thread::sleep(Duration::from_millis(80));
        }
    });
    Arc::new(fake)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_msg_maps_every_phone_theme() {
        use nd_config::PhoneTheme as C;
        use nd_proto::v1::PhoneTheme as P;
        let cases = [
            (C::Black, P::Black),
            (C::Graphite, P::Graphite),
            (C::Cream, P::Cream),
            (C::Lavender, P::Lavender),
            (C::Mint, P::Mint),
            (C::Dusk, P::Dusk),
            (C::Tide, P::Tide),
        ];
        for (c, p) in cases {
            let settings = nd_config::Settings { phone_theme: c, ..Default::default() };
            match appearance_msg(&settings) {
                nd_proto::Msg::Appearance(a) => assert_eq!(a.theme, p as i32),
                other => panic!("unexpected message {other:?}"),
            }
        }
    }

    #[test]
    fn appearance_msg_carries_the_sleep_block() {
        let settings = nd_config::Settings {
            deck_sleep_enabled: false,
            deck_sleep_idle_s: 5,
            deck_sleep_wake_on_camera_mic: false,
            deck_sleep_clock: true,
            ..Default::default()
        };
        match appearance_msg(&settings) {
            nd_proto::Msg::Appearance(a) => {
                let s = a.sleep.expect("sleep block");
                assert!(!s.enabled && !s.wake_on_camera_mic && s.show_clock);
                assert_eq!(s.idle_s, 15);
            }
            other => panic!("unexpected message {other:?}"),
        }
        match appearance_msg(&nd_config::Settings::default()) {
            nd_proto::Msg::Appearance(a) => {
                let s = a.sleep.expect("sleep block");
                assert!(s.enabled && s.wake_on_camera_mic && s.show_clock);
                assert_eq!(s.idle_s, 120);
            }
            other => panic!("unexpected message {other:?}"),
        }
    }

    #[test]
    fn no_problems_when_all_is_well() {
        assert!(compute_problems(None, KeyboardAccess::Ok, "pactl").is_empty());
        assert!(compute_problems(None, KeyboardAccess::Unsupported, "wasapi").is_empty());
    }

    #[test]
    fn each_problem_has_its_code_and_fix() {
        let p = compute_problems(Some("not found"), KeyboardAccess::Missing, "none");
        let codes: Vec<_> = p.iter().map(|p| p.code.as_str()).collect();
        assert_eq!(codes, ["adb_missing", "keyboard_access", "audio_unavailable"]);
        assert!(p[0].message.contains("USB is disabled") && p[0].fix.is_none());
        assert_eq!(p[1].fix.as_deref(), Some("keyboard_access"));
        assert!(p[2].fix.is_none());
    }
}
