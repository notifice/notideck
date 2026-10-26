// SPDX-License-Identifier: GPL-3.0-or-later
//! Adapter exposing [`AgentState`] to the `nd-config` web server.

use crate::app::ShellEvent;
use crate::devices::DeviceRegistry;
use crate::discovery::DiscoveryStatus;
use crate::settings::SettingsStore;
use crate::AgentState;
use nd_config::{
    AdbInfo, BackendError, ConfigBackend, DeviceInfo, HostInfo, PairedDevice, PairingTicket, PendingDevice,
    PhoneUsbStatus, Problem, ProfileSummary, Settings, SettingsPatch, Status,
};
use nd_core::{ActionRouter, CoreError, Profile};
use nd_platform::native;
use nd_transport::{Fingerprint, HostIdentity, Link, ServerHandle};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch, Notify};

/// Mutable facts shared between the app's status task and the backend.
#[derive(Default)]
pub struct Shared {
    /// Wake the status task to recompute problems now.
    pub refresh: Notify,
    pub problems: Mutex<Vec<Problem>>,
    pub adb: Mutex<AdbInfo>,
}

/// Everything the backend needs beyond profiles (set by `app::start`).
pub struct BackendCtx {
    pub settings: Arc<SettingsStore>,
    pub router: ActionRouter,
    pub shell: broadcast::Sender<ShellEvent>,
    pub shared: Arc<Shared>,
    pub control_port: u16,
    pub ui_port: u16,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub config_dir: PathBuf,
    pub audio_backend: &'static str,
    pub media_backend: &'static str,
    pub camera: Arc<crate::camera::CameraManager>,
    pub mic: Arc<crate::mic::MicManager>,
    pub registry: Arc<DeviceRegistry>,
    pub identity: Arc<HostIdentity>,
    /// Whether the control listener is bound to all interfaces (decided at startup from
    /// `lan_enabled`; changing the setting at runtime cannot rebind it).
    pub lan_listening: bool,
    /// Effective LAN mode (`lan_enabled && lan_listening`) for the mDNS task.
    pub lan_tx: watch::Sender<bool>,
    pub discovery: Arc<DiscoveryStatus>,
    pub autoswitch: Arc<crate::autoswitch::AutoSwitchService>,
    pub mixer: Arc<nd_core::Mixer>,
    /// The OBS client (round 14). Settings edits go through [`crate::obs_cfg`] and `update_settings`.
    pub obs: Arc<nd_obs::ObsClient>,
    /// Keeps the `NOTIDECK_FAKE_OBS` server alive for the agent's lifetime.
    pub _fake_obs: Option<nd_obs::FakeObs>,
    /// Wakes the live ticker (mixer config edits re-resolve slots right away).
    pub live_poke: Arc<Notify>,
    /// The plugin host (round 19); `None` = plugins are off (`NOTIDECK_NO_PLUGINS`) or failed to start.
    pub plugins: Option<Arc<nd_plugin::PluginHost>>,
}

/// How long a listed app catalog stays fresh.
const APPS_TTL: std::time::Duration = std::time::Duration::from_secs(600);
/// Max cached app icons.
const APP_ICON_CAP: usize = 512;

#[derive(Default)]
struct AppsCache {
    apps: Vec<nd_platform::InstalledApp>,
    fetched_at: Option<std::time::Instant>,
    error: Option<String>,
    icons: std::collections::HashMap<String, Option<Vec<u8>>>,
}

pub struct AgentConfigBackend {
    state: AgentState,
    server: Option<ServerHandle>,
    ctx: Option<BackendCtx>,
    catalog: Box<dyn nd_platform::AppCatalog>,
    apps: Mutex<AppsCache>,
}

impl AgentConfigBackend {
    pub fn new(state: AgentState) -> Self {
        Self::with_catalog(state, native::app_catalog())
    }

    pub fn with_catalog(state: AgentState, catalog: Box<dyn nd_platform::AppCatalog>) -> Self {
        Self { state, server: None, ctx: None, catalog, apps: Mutex::new(AppsCache::default()) }
    }

    /// Attach the transport server so `status` can report connected phones.
    pub fn with_server(mut self, server: ServerHandle) -> Self {
        self.server = Some(server);
        self
    }

    fn plugins(&self) -> Option<&Arc<nd_plugin::PluginHost>> {
        self.ctx.as_ref()?.plugins.as_ref()
    }

    /// Run a live tick now so plugin looks and `willAppear` follow the change.
    fn poke_live(&self) {
        if let Some(c) = &self.ctx {
            c.live_poke.notify_one();
        }
    }

    /// Attach app-level context (settings, host info, fixes, shell).
    pub fn with_ctx(mut self, ctx: BackendCtx) -> Self {
        self.ctx = Some(ctx);
        self
    }
}

fn plugins_off() -> BackendError {
    BackendError::NotFound("plugins are not available".into())
}

/// Map a plugin host error to an API error.
fn map_plugin_error(e: nd_plugin::host::HostError) -> BackendError {
    match e {
        nd_plugin::host::HostError::NotFound(id) => BackendError::NotFound(format!("no plugin {id}")),
        nd_plugin::host::HostError::Invalid(m) => BackendError::Invalid(m),
        nd_plugin::host::HostError::Io(m) => BackendError::Internal(m),
    }
}

/// Map a core error to an API error: bad input is a 400, platform/IO failures are 500.
pub fn map_core_error(e: CoreError) -> BackendError {
    match e {
        CoreError::Platform(_) | CoreError::Io(_) => BackendError::Internal(e.to_string()),
        other => BackendError::Invalid(other.to_string()),
    }
}

fn detected_json(d: &nd_obs::DetectedConfig) -> serde_json::Value {
    serde_json::json!({
        "path": d.path, "exists": d.exists, "server_enabled": d.server_enabled, "server_port": d.server_port,
        "auth_required": d.auth_required, "has_password": !d.server_password.is_empty(),
    })
}

/// `GET /api/obs` body (never contains a password).
pub fn obs_status_json(client: &nd_obs::ObsClient) -> serde_json::Value {
    use nd_core::obs::ObsControl;
    let s = client.settings();
    let info = client.info();
    let snap = client.snapshot();
    let inputs: Vec<_> = snap
        .inputs
        .iter()
        .map(|i| serde_json::json!({ "name": i.name, "kind": i.kind, "audio": i.muted.is_some() }))
        .collect();
    let special: serde_json::Map<String, serde_json::Value> =
        snap.special_inputs.iter().map(|(k, v)| (k.ws_key().to_string(), serde_json::json!(v))).collect();
    let items: serde_json::Map<String, serde_json::Value> = snap
        .scene_items
        .iter()
        .map(|(scene, items)| (scene.clone(), serde_json::json!(items.iter().map(|i| &i.source).collect::<Vec<_>>())))
        .collect();
    serde_json::json!({
        "settings": { "enabled": s.enabled, "auto_detect": s.auto_detect, "host": s.host, "port": s.port,
                      "has_password": !s.password.is_empty() },
        "status": info.status,
        "endpoint": info.endpoint,
        "from_detected": info.from_detected,
        "obs_version": info.obs_version,
        "ws_version": info.ws_version,
        "last_error": info.last_error,
        "detected": nd_obs::detect::read().map(|d| detected_json(&d)),
        "studio_mode": snap.studio_mode,
        "program_scene": snap.program_scene,
        "scenes": snap.scenes,
        "inputs": inputs,
        "special": special,
        "scene_items": items,
    })
}

/// `GET /api/mixer` body for `m`: refreshes the session list and adds each app's icon hash.
pub fn mixer_status_json(m: &nd_core::Mixer) -> serde_json::Value {
    let listed = m.refresh();
    let apps: Vec<serde_json::Value> = listed
        .as_ref()
        .map(|apps| {
            apps.iter()
                .map(|a| {
                    let mut v = serde_json::to_value(a).unwrap_or_default();
                    if let Some(h) = m.icon_hash(a) {
                        v["icon_hash"] = serde_json::json!(h);
                    }
                    v
                })
                .collect()
        })
        .unwrap_or_default();
    let mut v = serde_json::json!({
        "backend": m.backend_name(),
        "available": m.available(),
        "has_peaks": m.has_app_peaks(),
        "config": m.config(),
        "apps": apps,
    });
    if listed.is_err() {
        v["error"] = serde_json::json!(m.last_error().unwrap_or_else(|| "The mixer is unavailable.".into()));
    }
    v
}

impl ConfigBackend for AgentConfigBackend {
    fn list_profiles(&self) -> Vec<ProfileSummary> {
        let active = self.state.active_profile_id();
        self.state
            .list_profiles()
            .unwrap_or_default()
            .into_iter()
            .map(|s| ProfileSummary {
                pages: self.state.get_profile(&s.id).map(|p| p.pages.len()).unwrap_or(0),
                active: s.id == active,
                id: s.id,
                name: s.name,
            })
            .collect()
    }

    fn get_profile(&self, id: &str) -> Option<Profile> {
        self.state.get_profile(id).ok()
    }

    fn put_profile(&self, profile: Profile) -> Result<(), BackendError> {
        self.state.put_profile(profile).map_err(map_core_error)
    }

    fn delete_profile(&self, id: &str) -> Result<bool, BackendError> {
        if self.state.get_profile(id).is_err() {
            return Ok(false);
        }
        if self.state.active_profile_id() == id {
            return Err(BackendError::Conflict("cannot delete the active profile; activate another one first".into()));
        }
        self.state.delete_profile(id).map_err(map_core_error)?;
        Ok(true)
    }

    fn put_icon(&self, bytes: Vec<u8>) -> Result<String, BackendError> {
        self.state.put_icon_bytes(&bytes).map_err(map_core_error)
    }

    fn get_icon(&self, hash: &str) -> Option<Vec<u8>> {
        self.state.get_icon(hash).map(|(_, data)| data)
    }

    fn activate_profile(&self, id: &str) -> Result<(), BackendError> {
        if self.state.get_profile(id).is_err() {
            return Err(BackendError::NotFound("no such profile".into()));
        }
        self.state.activate_profile(id).map_err(map_core_error)
    }

    fn status(&self) -> Status {
        Status {
            version: env!("CARGO_PKG_VERSION").to_string(),
            active_profile_id: Some(self.state.active_profile_id()),
            devices: self
                .server
                .as_ref()
                .map(|s| {
                    s.all_sessions()
                        .into_iter()
                        .filter(|d| d.is_connected())
                        .map(|d| {
                            let health = d.health();
                            DeviceInfo {
                                id: d.peer_fp().to_hex(),
                                name: d.peer_info().device_name.clone(),
                                transport: d.transport().to_string(),
                                connected: true,
                                android_sdk: d.android_sdk(),
                                app_version: d.app_version().to_string(),
                                battery_percent: health.as_ref().map(|h| h.battery_percent),
                                charging: health.as_ref().map(|h| h.charging),
                                battery_temp_c: health.as_ref().map(|h| h.battery_temp_c),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
            problems: self
                .ctx
                .as_ref()
                .map(|c| c.shared.problems.lock().unwrap_or_else(|e| e.into_inner()).clone())
                .unwrap_or_default(),
        }
    }

    fn settings(&self) -> Settings {
        self.ctx.as_ref().map(|c| c.settings.get()).unwrap_or_default()
    }

    fn update_settings(&self, patch: SettingsPatch) -> Result<Settings, BackendError> {
        match &self.ctx {
            Some(c) => {
                let appearance_changed = patch.phone_theme.is_some()
                    || patch.deck_sleep_enabled.is_some()
                    || patch.deck_sleep_idle_s.is_some()
                    || patch.deck_sleep_wake_on_camera_mic.is_some()
                    || patch.deck_sleep_clock.is_some();
                let lan_changed = patch.lan_enabled.is_some();
                let settings = c.settings.update(patch)?;
                if lan_changed {
                    let effective = settings.lan_enabled && c.lan_listening;
                    c.lan_tx.send_replace(effective);
                    if !settings.lan_enabled {
                        // The listener stays bound until restart, so enforce the switch here.
                        c.registry.cancel_ticket();
                        for s in self.server.iter().flat_map(|s| s.all_sessions()) {
                            if s.link() == Link::Lan {
                                s.close(
                                    nd_proto::v1::GoodbyeReason::Shutdown,
                                    "Wi-Fi connections were turned off on the PC",
                                );
                            }
                        }
                    }
                }
                if appearance_changed {
                    for s in self.server.iter().flat_map(|s| s.all_sessions()) {
                        if let Err(e) = s.try_send_msg(crate::app::appearance_msg(&settings)) {
                            tracing::debug!("Appearance push skipped: {e}");
                        }
                    }
                }
                Ok(settings)
            }
            None => Err(BackendError::Invalid("settings are not supported".into())),
        }
    }

    fn host_info(&self) -> HostInfo {
        let Some(c) = &self.ctx else { return HostInfo::default() };
        HostInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            control_port: c.control_port,
            ui_port: c.ui_port,
            data_dir: c.data_dir.display().to_string(),
            log_dir: c.log_dir.display().to_string(),
            config_dir: c.config_dir.display().to_string(),
            adb: c.shared.adb.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            keyboard_access: crate::app::map_access(native::keyboard_access()),
            audio_backend: c.audio_backend.to_string(),
            media_backend: c.media_backend.to_string(),
        }
    }

    fn run_fix(&self, id: &str) -> Result<(), BackendError> {
        let Some(c) = &self.ctx else { return Err(BackendError::NotFound(format!("no fix '{id}'"))) };
        match id {
            "keyboard_access" => {
                native::fix_keyboard_access()
                    .map_err(|e| BackendError::Internal(format!("could not fix keyboard access: {e}")))?;
                let injector =
                    native::input().map_err(|e| BackendError::Internal(format!("input backend failed: {e}")))?;
                c.router.set_injector(injector.into());
                c.shared.refresh.notify_one();
                Ok(())
            }
            "install_webcam" | "uninstall_webcam" => {
                // Runs through a UAC prompt; the UI polls /api/camera for the result.
                c.camera.begin_install(id == "install_webcam").map_err(BackendError::Conflict)
            }
            _ => Err(BackendError::NotFound(format!("no fix '{id}'"))),
        }
    }

    fn camera_status(&self) -> serde_json::Value {
        match &self.ctx {
            Some(c) => c.camera.status_json(),
            None => serde_json::json!({ "state": "unsupported", "message": "The webcam is not available." }),
        }
    }

    fn camera_test(&self, on: bool) -> Result<serde_json::Value, BackendError> {
        let c = self.ctx.as_ref().ok_or_else(|| BackendError::NotFound("the webcam is not available".into()))?;
        c.camera.set_test(on);
        Ok(c.camera.status_json())
    }

    fn camera_control(&self, req: nd_config::CameraControlReq) -> Result<serde_json::Value, BackendError> {
        let c = self.ctx.as_ref().ok_or_else(|| BackendError::NotFound("the webcam is not available".into()))?;
        c.camera.control(req).map_err(BackendError::Conflict)?;
        Ok(c.camera.status_json())
    }

    fn camera_preview(&self) -> Option<Vec<u8>> {
        self.ctx.as_ref()?.camera.preview_png()
    }

    fn mic_status(&self) -> serde_json::Value {
        match &self.ctx {
            Some(c) => c.mic.status_json(),
            None => serde_json::json!({ "state": "unsupported", "message": "The phone microphone is not available." }),
        }
    }

    fn mic_control(&self, req: nd_config::MicControlReq) -> Result<serde_json::Value, BackendError> {
        let c =
            self.ctx.as_ref().ok_or_else(|| BackendError::NotFound("the phone microphone is not available".into()))?;
        c.mic.control(req).map_err(BackendError::Conflict)?;
        Ok(c.mic.status_json())
    }

    fn rail_get(&self) -> Vec<nd_core::Control> {
        self.state.rail_keys()
    }

    fn rail_put(&self, keys: Vec<nd_core::Control>) -> Result<(), nd_config::RailPutError> {
        self.state.set_rail(keys).map_err(nd_config::RailPutError::Invalid)
    }

    fn autoswitch_status(&self) -> serde_json::Value {
        match &self.ctx {
            Some(c) => c.autoswitch.status_json(),
            None => serde_json::json!({ "config": nd_core::autoswitch::AutoSwitchConfig::default(), "locked": false,
                "backend": "none", "available": false, "error": "Auto-switch is not available.", "recent": [] }),
        }
    }

    fn autoswitch_put(
        &self,
        config: nd_core::autoswitch::AutoSwitchConfig,
    ) -> Result<(), nd_config::AutoSwitchPutError> {
        use crate::autoswitch::PutError;
        use nd_config::AutoSwitchPutError as E;
        let c = self
            .ctx
            .as_ref()
            .ok_or_else(|| E::Backend(BackendError::NotFound("auto-switch is not available".into())))?;
        c.autoswitch.put_config(config).map_err(|e| match e {
            PutError::Invalid(v) => E::Invalid(v),
            PutError::Io(m) => E::Backend(BackendError::Internal(m)),
        })
    }

    fn autoswitch_lock(&self, locked: bool) -> Result<(), BackendError> {
        let c = self.ctx.as_ref().ok_or_else(|| BackendError::NotFound("auto-switch is not available".into()))?;
        c.autoswitch.set_locked(locked);
        Ok(())
    }

    fn obs_status(&self) -> serde_json::Value {
        let Some(c) = &self.ctx else {
            return serde_json::json!({ "settings": null, "status": "disabled", "detected": null, "scenes": [],
                "inputs": [], "special": {}, "scene_items": {}, "studio_mode": false });
        };
        obs_status_json(&c.obs)
    }

    fn obs_put_config(&self, config: nd_config::ObsConfigPut) -> Result<(), BackendError> {
        let c = self.ctx.as_ref().ok_or_else(|| BackendError::NotFound("OBS integration is not available".into()))?;
        if config.port == 0 {
            return Err(BackendError::Invalid("the OBS port must be 1-65535".into()));
        }
        let host = config.host.trim().to_string();
        if !config.auto_detect && host.is_empty() {
            return Err(BackendError::Invalid("enter the OBS host, or use automatic detection".into()));
        }
        let old = c.obs.settings();
        let settings = nd_obs::ObsSettings {
            enabled: config.enabled,
            auto_detect: config.auto_detect,
            host,
            port: config.port,
            password: config.password.unwrap_or(old.password),
        };
        crate::obs_cfg::save(&c.config_dir, &settings)
            .map_err(|e| BackendError::Internal(format!("cannot save obs.json: {e}")))?;
        c.obs.update_settings(settings);
        Ok(())
    }

    fn obs_enable_server(&self) -> Result<serde_json::Value, BackendError> {
        let c = self.ctx.as_ref().ok_or_else(|| BackendError::NotFound("OBS integration is not available".into()))?;
        let d = nd_obs::detect::enable_server().map_err(BackendError::Conflict)?;
        c.obs.reconnect_now();
        Ok(serde_json::json!({ "detected": detected_json(&d) }))
    }

    fn obs_reconnect(&self) {
        if let Some(c) = &self.ctx {
            c.obs.reconnect_now();
        }
    }

    fn mixer_status(&self) -> serde_json::Value {
        let Some(c) = &self.ctx else {
            return serde_json::json!({ "backend": "none", "available": false, "error": "The mixer is not available.",
                "has_peaks": false, "config": nd_core::MixerConfig::default(), "apps": [] });
        };
        mixer_status_json(&c.mixer)
    }

    fn mixer_put_config(&self, config: nd_core::MixerConfig) -> Result<(), BackendError> {
        let c = self.ctx.as_ref().ok_or_else(|| BackendError::NotFound("the mixer is not available".into()))?;
        crate::mixer_cfg::save(&c.config_dir, &config)
            .map_err(|e| BackendError::Internal(format!("cannot save mixer.json: {e}")))?;
        c.mixer.set_config(config);
        c.live_poke.notify_one();
        Ok(())
    }

    fn mixer_set_app(&self, key: &str, volume: Option<f32>, muted: Option<bool>) -> Result<(), BackendError> {
        let c = self.ctx.as_ref().ok_or_else(|| BackendError::NotFound("the mixer is not available".into()))?;
        if !c.mixer.apps().iter().any(|a| a.key == key) {
            return Err(BackendError::NotFound(format!("no audio app '{key}'")));
        }
        c.mixer.set_app(key, volume, muted).map_err(map_core_error)?;
        c.live_poke.notify_one();
        Ok(())
    }

    fn apps_list(&self, refresh: bool) -> serde_json::Value {
        let mut c = self.apps.lock().unwrap_or_else(|e| e.into_inner());
        let stale = c.fetched_at.is_none_or(|t| t.elapsed() > APPS_TTL);
        if refresh || stale {
            match self.catalog.list() {
                Ok(apps) => {
                    c.apps = apps;
                    c.error = None;
                }
                Err(e) => {
                    c.apps.clear();
                    c.error = Some(e.to_string());
                }
            }
            c.icons.clear();
            c.fetched_at = Some(std::time::Instant::now());
        }
        let mut v = serde_json::json!({
            "backend": self.catalog.backend_name(),
            "available": c.error.is_none(),
            "apps": c.apps,
        });
        if let Some(e) = &c.error {
            v["error"] = serde_json::json!(e);
        }
        v
    }

    fn app_icon(&self, id: &str) -> Option<Vec<u8>> {
        let app = {
            let c = self.apps.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hit) = c.icons.get(id) {
                return hit.clone();
            }
            c.apps.iter().find(|a| a.id == id).cloned()?
        };
        // Extract outside the lock: icon extraction can be slow.
        let png = self.catalog.icon(&app).ok().flatten();
        let mut c = self.apps.lock().unwrap_or_else(|e| e.into_inner());
        if c.icons.len() >= APP_ICON_CAP {
            c.icons.clear();
        }
        c.icons.insert(id.to_string(), png.clone());
        png
    }

    fn site_icon(&self, url: &str) -> Result<(String, Vec<u8>), BackendError> {
        match crate::site_icon::fetch(url) {
            Ok(Some(icon)) => Ok((icon.content_type.to_string(), icon.bytes)),
            Ok(None) => Err(BackendError::NotFound("no icon found for that site".into())),
            Err(msg) => Err(BackendError::Invalid(msg)),
        }
    }

    fn open_folder(&self, which: &str) -> Result<(), BackendError> {
        let Some(c) = &self.ctx else { return Err(BackendError::NotFound(format!("no folder '{which}'"))) };
        let plugins_dir = c.config_dir.join("plugins");
        let dir = match which {
            "logs" => &c.log_dir,
            "config" => &c.config_dir,
            "plugins" => &plugins_dir,
            _ => return Err(BackendError::NotFound(format!("no folder '{which}'"))),
        };
        let _ = std::fs::create_dir_all(dir);
        let launcher = native::launcher().map_err(|e| BackendError::Internal(e.to_string()))?;
        let path = dir.display().to_string();
        // xdg-open takes the path as its "URL"; the Windows launcher shell-opens it.
        let res = if cfg!(windows) { launcher.launch(&path, &[]) } else { launcher.open_url(&path) };
        res.map_err(|e| BackendError::Internal(format!("cannot open folder: {e}")))
    }

    fn plugins_list(&self) -> serde_json::Value {
        match self.plugins() {
            Some(h) => serde_json::json!({ "available": true, "plugins": h.list() }),
            None => serde_json::json!({ "available": false, "plugins": [] }),
        }
    }

    fn plugin_command(&self, id: &str, cmd: &str) -> Result<(), BackendError> {
        use nd_plugin::PluginCommand as C;
        let host = self.plugins().ok_or_else(plugins_off)?;
        let cmd = match cmd {
            "enable" => C::Enable,
            "disable" => C::Disable,
            "approve" => C::Approve,
            "restart" => C::Restart,
            "uninstall" => C::Uninstall,
            other => return Err(BackendError::Invalid(format!("unknown plugin command '{other}'"))),
        };
        host.command(id, cmd).map_err(map_plugin_error)?;
        self.poke_live();
        Ok(())
    }

    fn plugins_reload(&self) {
        if let Some(h) = self.plugins() {
            h.reload();
            self.poke_live();
        }
    }

    fn plugin_install(&self, zip: Vec<u8>) -> Result<serde_json::Value, BackendError> {
        let host = self.plugins().ok_or_else(plugins_off)?;
        let info = host.install_zip(&zip).map_err(map_plugin_error)?;
        self.poke_live();
        serde_json::to_value(info).map_err(|e| BackendError::Internal(e.to_string()))
    }

    fn plugin_log(&self, id: &str) -> Option<Vec<String>> {
        self.plugins()?.log(id)
    }

    fn plugin_file(&self, id: &str, path: &str) -> Option<Vec<u8>> {
        let file = self.plugins()?.file(id, path)?;
        // Icons only: keep reads small.
        let meta = std::fs::metadata(&file).ok()?;
        (meta.len() <= 4 * 1024 * 1024).then(|| std::fs::read(file).ok()).flatten()
    }

    fn create_pairing(&self) -> Result<PairingTicket, BackendError> {
        let Some(c) = &self.ctx else { return Err(BackendError::NotFound("pairing is not supported".into())) };
        if !c.settings.get().lan_enabled {
            return Err(BackendError::Conflict("Wi-Fi connections are off. Turn them on in Settings first.".into()));
        }
        if !c.lan_listening {
            return Err(BackendError::Conflict(
                "Wi-Fi connections were turned on after Notideck started. Restart Notideck to listen on the network."
                    .into(),
            ));
        }
        crate::pairing::create_ticket(
            &c.registry,
            &c.identity,
            &nd_transport::get_host_name(),
            &crate::pairing::host_addrs(),
            c.control_port,
        )
    }

    fn cancel_pairing(&self) {
        if let Some(c) = &self.ctx {
            c.registry.cancel_ticket();
        }
    }

    fn paired_devices(&self) -> Vec<PairedDevice> {
        let Some(c) = &self.ctx else { return Vec::new() };
        let live: Vec<_> = self
            .server
            .iter()
            .flat_map(|s| s.all_sessions())
            .filter(|s| s.is_connected())
            .map(|s| (s.peer_fp().to_hex(), s.link()))
            .collect();
        c.registry
            .list()
            .into_iter()
            .map(|(d, reg_link)| {
                let link = live.iter().find(|(id, _)| *id == d.fingerprint).map(|(_, l)| *l).or(reg_link);
                PairedDevice {
                    fp_short: d.fingerprint.chars().take(8).collect(),
                    connected: link.is_some(),
                    transport: link.map(|l| l.as_str().to_string()).unwrap_or(d.last_link),
                    id: d.fingerprint,
                    name: d.name,
                    model: d.model,
                    paired_at: d.paired_at,
                    last_seen: d.last_seen,
                }
            })
            .collect()
    }

    fn remove_device(&self, id: &str) -> Result<(), BackendError> {
        let not_found = || BackendError::NotFound(format!("no device '{id}'"));
        let c = self.ctx.as_ref().ok_or_else(not_found)?;
        let fp = Fingerprint::from_hex(id).ok_or_else(not_found)?;
        if !c.registry.remove(&fp.to_hex()) {
            return Err(not_found());
        }
        if let Some(s) = &self.server {
            s.revoke(&fp);
        }
        Ok(())
    }

    fn pending_devices(&self) -> Vec<PendingDevice> {
        self.server
            .iter()
            .flat_map(|s| s.pending())
            .map(|p| PendingDevice {
                session_id: p.session_id,
                name: p.peer.device_name,
                model: p.peer.device_model,
                fp_short: p.peer.fp.short(),
            })
            .collect()
    }

    fn resolve_pending(&self, session_id: &str, allow: bool) -> Result<(), BackendError> {
        match &self.server {
            Some(s) if s.resolve_pending(session_id, allow) => Ok(()),
            _ => Err(BackendError::NotFound(format!("no pending phone '{session_id}'"))),
        }
    }

    fn shell_quit(&self) -> Result<(), BackendError> {
        match &self.ctx {
            Some(c) => {
                // Let the 204 reach the caller before the shell tears the server down.
                let tx = c.shell.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    let _ = tx.send(ShellEvent::Quit);
                });
                Ok(())
            }
            None => Err(BackendError::NotFound("quit is not supported".into())),
        }
    }

    fn phone_usb_status(&self) -> PhoneUsbStatus {
        crate::phone_usb::status(std::env::var_os("NOTIDECK_NO_ADB").is_some())
    }

    fn install_phone_app(&self, serial: &str) -> Result<(), BackendError> {
        crate::phone_usb::install(std::env::var_os("NOTIDECK_NO_ADB").is_some(), serial)
    }

    fn shell_show(&self) -> Result<(), BackendError> {
        match &self.ctx {
            Some(c) => {
                // No receivers (headless) is fine.
                let _ = c.shell.send(ShellEvent::Show);
                Ok(())
            }
            None => Err(BackendError::NotFound("no app window".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_errors_map_to_backend_kinds() {
        assert!(matches!(map_core_error(CoreError::Profile("x".into())), BackendError::Invalid(_)));
        let io = std::io::Error::other("disk");
        assert!(matches!(map_core_error(CoreError::Io(io)), BackendError::Internal(_)));
    }

    #[test]
    fn mixer_status_lists_apps_with_icon_hashes_and_errors() {
        use nd_core::mock::{MockAppAudio, MockAudio};
        let mock = MockAppAudio::new();
        mock.set_sessions(vec![
            MockAppAudio::session("sp", "Spotify.exe", "Spotify", 0.5),
            MockAppAudio::session("ch", "chrome.exe", "Chrome", 0.8),
        ]);
        mock.set_icon("Spotify.exe", b"\x89PNG\r\n\x1a\nbody");
        let mixer = nd_core::Mixer::new(Arc::new(mock.clone()), Arc::new(MockAudio::new()))
            .with_icons(Arc::new(|b: &[u8]| Some(format!("hash{}", b.len()))));
        mixer.set_config(nd_core::MixerConfig { hidden: vec!["chrome".into()], ..Default::default() });
        let v = mixer_status_json(&mixer);
        assert_eq!(v["backend"], "mock");
        assert_eq!(v["available"], true);
        assert_eq!(v["config"]["hidden"][0], "chrome");
        let apps = v["apps"].as_array().unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!((apps[0]["key"].as_str(), apps[0]["slot"].as_u64()), (Some("spotify"), Some(0)));
        assert_eq!(apps[0]["icon_hash"], "hash12");
        assert_eq!((apps[1]["key"].as_str(), apps[1]["hidden"].as_bool()), (Some("chrome"), Some(true)));
        assert!(apps[1].get("icon_hash").is_none() && v.get("error").is_none());
        mock.set_fail_list(true);
        let v = mixer_status_json(&mixer);
        assert_eq!(v["available"], false);
        assert!(v["error"].as_str().unwrap().contains("mock list failure"));
        assert_eq!(v["apps"].as_array().unwrap().len(), 0);
    }

    struct FakeCatalog {
        lists: std::sync::atomic::AtomicUsize,
        fail: bool,
    }
    impl nd_platform::AppCatalog for FakeCatalog {
        fn backend_name(&self) -> &'static str {
            "fake"
        }
        fn list(&self) -> nd_platform::Result<Vec<nd_platform::InstalledApp>> {
            self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail {
                return Err(nd_platform::PlatformError::Unsupported("installed app list"));
            }
            Ok(vec![nd_platform::InstalledApp {
                id: "a.desktop".into(),
                name: "A".into(),
                target: "a".into(),
                args: vec![],
            }])
        }
        fn icon(&self, app: &nd_platform::InstalledApp) -> nd_platform::Result<Option<Vec<u8>>> {
            Ok((app.id == "a.desktop").then(|| vec![1, 2, 3]))
        }
    }

    #[test]
    fn apps_list_caches_and_refreshes() {
        let dir = std::env::temp_dir().join(format!("nd-apps-{}", std::process::id()));
        let state = AgentState::open(nd_core::ProfileStore::new(dir.clone())).unwrap();
        let cat = FakeCatalog { lists: Default::default(), fail: false };
        let b = AgentConfigBackend::with_catalog(state, Box::new(cat));
        let v = b.apps_list(false);
        assert_eq!((v["backend"].as_str(), v["available"].as_bool()), (Some("fake"), Some(true)));
        assert_eq!(v["apps"][0]["name"], "A");
        b.apps_list(false);
        assert_eq!(b.app_icon("a.desktop"), Some(vec![1, 2, 3]));
        assert_eq!(b.app_icon("nope"), None);
        b.apps_list(true);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn plugins_are_unavailable_without_a_host() {
        let dir = std::env::temp_dir().join(format!("nd-noplug-{}", std::process::id()));
        let state = AgentState::open(nd_core::ProfileStore::new(dir.clone())).unwrap();
        let b =
            AgentConfigBackend::with_catalog(state, Box::new(FakeCatalog { lists: Default::default(), fail: false }));
        assert_eq!(b.plugins_list()["available"], false);
        assert!(matches!(b.plugin_command("x", "enable"), Err(BackendError::NotFound(_))));
        assert!(matches!(b.plugin_install(vec![1]), Err(BackendError::NotFound(_))));
        assert!(b.plugin_log("x").is_none() && b.plugin_file("x", "a.png").is_none());
        b.plugins_reload();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn apps_list_reports_errors() {
        let dir = std::env::temp_dir().join(format!("nd-apps-e-{}", std::process::id()));
        let state = AgentState::open(nd_core::ProfileStore::new(dir.clone())).unwrap();
        let b =
            AgentConfigBackend::with_catalog(state, Box::new(FakeCatalog { lists: Default::default(), fail: true }));
        let v = b.apps_list(false);
        assert_eq!(v["available"], false);
        assert!(v["error"].as_str().unwrap().contains("installed app list"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
