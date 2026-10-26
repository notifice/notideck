// SPDX-License-Identifier: GPL-3.0-or-later
//! Config web server (SPEC §3.3).
//!
//! Serves the embedded single-page UI from `host/ui` and a small REST API.
//! The storage layer is abstracted behind [`ConfigBackend`], which the agent
//! implements on top of its profile store and icon store.
//!
//! Security: bind to loopback only, reject foreign `Host` headers (DNS
//! rebinding), and optionally require a session token (`X-ND-Token` header or
//! `?token=` query) on every `/api` call.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use include_dir::{include_dir, Dir};
use nd_core::model::Profile;
use serde::{Deserialize, Serialize};

/// Default config UI port (SPEC §3.3).
pub const DEFAULT_PORT: u16 = 47801;

/// Maximum accepted icon upload size in bytes.
const MAX_ICON_BYTES: usize = 2 * 1024 * 1024;
/// Maximum accepted profile JSON size in bytes.
const MAX_PROFILE_BYTES: usize = 4 * 1024 * 1024;
/// Upper bound for `POST /api/plugins/install` (a raw .zip).
const MAX_PLUGIN_ZIP_BYTES: usize = 20 * 1024 * 1024;

static UI: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../ui");

/// Lightweight listing entry for the profile switcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
    pub pages: usize,
    pub active: bool,
}

/// A phone known to the agent.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    /// "usb" or "lan".
    pub transport: String,
    pub connected: bool,
    #[serde(default)]
    pub android_sdk: u32,
    #[serde(default)]
    pub app_version: String,
    /// Last `DeviceHealth` report, if the phone sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_percent: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_temp_c: Option<f32>,
}

/// Something the user should know about or fix (shown in the app).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Problem {
    /// Stable id, e.g. `adb_missing`, `keyboard_access`, `audio_unavailable`.
    pub code: String,
    pub message: String,
    /// Id for `POST /api/fix/:id` when the app can fix it itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

/// Agent status shown in the UI header.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Status {
    pub version: String,
    pub active_profile_id: Option<String>,
    pub devices: Vec<DeviceInfo>,
    #[serde(default)]
    pub problems: Vec<Problem>,
}

/// UI colour theme.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

/// User preferences persisted by the agent (`settings.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default)]
    pub theme: Theme,
    /// Deck look on the phone. Black is the default (AMOLED pixels off).
    #[serde(default)]
    pub phone_theme: PhoneTheme,
    /// Accept phones over Wi-Fi (bind 0.0.0.0). Off = loopback/USB only. ADR 0004 §9.
    #[serde(default = "default_true")]
    pub lan_enabled: bool,
    /// Which phone lens the webcam uses.
    #[serde(default)]
    pub camera_lens: CameraLens,
    /// `"auto"` or `"WxH@fps"` (see [`parse_camera_mode`]).
    #[serde(default = "default_camera_mode")]
    pub camera_mode: String,
    /// Fixed encoder bitrate in kbit/s; 0 = automatic (by resolution and link).
    #[serde(default)]
    pub camera_bitrate_kbps: u32,
    #[serde(default)]
    pub camera_mirror: MirrorMode,
    /// Start the phone camera when an app opens "Notideck Camera".
    #[serde(default = "default_true")]
    pub camera_auto_start: bool,
    /// Focus / exposure / white balance etc. (round 15). Applied live and on every camera start.
    #[serde(default)]
    pub camera_image: CameraImageSettings,
    /// The first-run setup wizard was finished or skipped (round 23). A missing field reads as
    /// `false`; the agent sets it at startup when a phone is already paired (existing installs).
    #[serde(default)]
    pub onboarding_done: bool,
    /// Round 25: the phone microphone is switched on. Remembered across restarts: the mic starts
    /// whenever a phone with CAPABILITY_MIC connects while this is true.
    #[serde(default)]
    pub mic_enabled: bool,
    /// Output device id from `GET /api/mic` `devices[].id`; `""` = auto (first virtual cable on
    /// Windows, the Notideck Microphone source on Linux).
    #[serde(default)]
    pub mic_device: String,
    /// Phone-side noise suppression / AGC / echo cancel (`MicStart.voice_processing`).
    #[serde(default = "default_true")]
    pub mic_voice_processing: bool,
    /// PC-side gain in dB, clamped to `MIC_GAIN_DB_MIN..=MIC_GAIN_DB_MAX`.
    #[serde(default)]
    pub mic_gain_db: i32,
    /// Windows: put the default playback/recording devices back when Windows (or a cable install)
    /// switches them to a virtual cable.
    #[serde(default = "default_true")]
    pub mic_guard_defaults: bool,
    /// Round 29: the phone blacks out the deck after `deck_sleep_idle_s` without a touch.
    #[serde(default = "default_true")]
    pub deck_sleep_enabled: bool,
    /// Idle time before the deck sleeps, clamped to `DECK_SLEEP_IDLE_S_MIN..=DECK_SLEEP_IDLE_S_MAX`.
    #[serde(default = "default_deck_sleep_idle_s")]
    pub deck_sleep_idle_s: u32,
    /// CameraStart / MicStart from the host wake the sleeping deck.
    #[serde(default = "default_true")]
    pub deck_sleep_wake_on_camera_mic: bool,
    /// Faint drifting clock on the black screen.
    #[serde(default = "default_true")]
    pub deck_sleep_clock: bool,
}

/// Webcam image settings, the JSON form of proto `CameraImage` (`settings.json`, `GET/PATCH /api/settings`).
/// The phone clamps every value to what the lens supports (`/api/camera` -> `phone.lens_caps`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct CameraImageSettings {
    pub focus: FocusMode,
    /// Manual focus: 0 = infinity .. 1000 = closest.
    pub focus_permille: u32,
    pub exposure: ExposureMode,
    /// Auto exposure: EV index (multiply by `lens_caps.ev_step` for EV).
    pub exposure_comp: i32,
    pub ae_lock: bool,
    /// Manual exposure ISO; 0 = take the phone's current value.
    pub iso: u32,
    /// Manual exposure time in µs (capped at 1/fps by the phone); 0 = take the phone's current value.
    pub exposure_us: u32,
    pub white_balance: WhiteBalance,
    pub awb_lock: bool,
    pub antibanding: Antibanding,
    pub stabilization: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FocusMode {
    #[default]
    Continuous,
    Locked,
    Manual,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExposureMode {
    #[default]
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WhiteBalance {
    #[default]
    Auto,
    Incandescent,
    Fluorescent,
    WarmFluorescent,
    Daylight,
    CloudyDaylight,
    Twilight,
    Shade,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Antibanding {
    #[default]
    Auto,
    Off,
    #[serde(rename = "50hz")]
    Hz50,
    #[serde(rename = "60hz")]
    Hz60,
}

impl CameraImageSettings {
    /// Range checks for `PATCH /api/settings` (hardware limits are the phone's job).
    pub fn validate(&self) -> Result<(), String> {
        if self.focus_permille > 1000 {
            return Err("focus_permille must be 0-1000".into());
        }
        if !(-100..=100).contains(&self.exposure_comp) {
            return Err("exposure_comp must be -100..100".into());
        }
        if self.iso > 409_600 {
            return Err("iso must be 0-409600".into());
        }
        if self.exposure_us > 10_000_000 {
            return Err("exposure_us must be 0-10000000".into());
        }
        Ok(())
    }
}

fn default_camera_mode() -> String {
    "auto".into()
}

/// Phone lens used by the webcam.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CameraLens {
    #[default]
    Back,
    Front,
}

impl CameraLens {
    pub fn toggled(self) -> Self {
        match self {
            Self::Back => Self::Front,
            Self::Front => Self::Back,
        }
    }
}

/// Mirror the picture horizontally. `Auto` mirrors the front lens only.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MirrorMode {
    #[default]
    Auto,
    On,
    Off,
}

/// The camera part of [`Settings`], as the camera manager reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraSettings {
    pub lens: CameraLens,
    pub mode: String,
    pub bitrate_kbps: u32,
    pub mirror: MirrorMode,
    pub auto_start: bool,
    pub image: CameraImageSettings,
}

impl Default for CameraSettings {
    fn default() -> Self {
        Settings::default().camera()
    }
}

impl Settings {
    pub fn camera(&self) -> CameraSettings {
        CameraSettings {
            lens: self.camera_lens,
            mode: self.camera_mode.clone(),
            bitrate_kbps: self.camera_bitrate_kbps,
            mirror: self.camera_mirror,
            auto_start: self.camera_auto_start,
            image: self.camera_image.clone(),
        }
    }
}

pub const MIC_GAIN_DB_MIN: i32 = -12;
pub const MIC_GAIN_DB_MAX: i32 = 24;

pub const DECK_SLEEP_IDLE_S_MIN: u32 = 15;
pub const DECK_SLEEP_IDLE_S_MAX: u32 = 3600;
pub const DECK_SLEEP_IDLE_S_DEFAULT: u32 = 120;

fn default_deck_sleep_idle_s() -> u32 {
    DECK_SLEEP_IDLE_S_DEFAULT
}

/// The microphone part of [`Settings`], as the mic manager reads it (round 25).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicSettings {
    pub enabled: bool,
    pub device: String,
    pub voice_processing: bool,
    pub gain_db: i32,
    pub guard_defaults: bool,
}

/// The deck sleep part of [`Settings`], as sent to the phone (round 29).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeckSleepSettings {
    pub enabled: bool,
    pub idle_s: u32,
    pub wake_on_camera_mic: bool,
    pub show_clock: bool,
}

impl Settings {
    /// The deck sleep block of `Appearance` (round 29); the idle time is clamped.
    pub fn deck_sleep(&self) -> DeckSleepSettings {
        DeckSleepSettings {
            enabled: self.deck_sleep_enabled,
            idle_s: self.deck_sleep_idle_s.clamp(DECK_SLEEP_IDLE_S_MIN, DECK_SLEEP_IDLE_S_MAX),
            wake_on_camera_mic: self.deck_sleep_wake_on_camera_mic,
            show_clock: self.deck_sleep_clock,
        }
    }

    pub fn mic(&self) -> MicSettings {
        MicSettings {
            enabled: self.mic_enabled,
            device: self.mic_device.clone(),
            voice_processing: self.mic_voice_processing,
            gain_db: self.mic_gain_db.clamp(MIC_GAIN_DB_MIN, MIC_GAIN_DB_MAX),
            guard_defaults: self.mic_guard_defaults,
        }
    }
}

/// Parse `"WxH@fps"` (e.g. `1280x720@30`). `"auto"` and malformed input give `None`.
pub fn parse_camera_mode(s: &str) -> Option<(u32, u32, u32)> {
    let (size, fps) = s.split_once('@')?;
    let (w, h) = size.split_once('x')?;
    let (w, h, fps) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?, fps.parse::<u32>().ok()?);
    ((2..=4096).contains(&w) && (2..=4096).contains(&h) && (1..=120).contains(&fps)).then_some((w, h, fps))
}

/// Whether `s` is acceptable as `camera_mode`.
pub fn valid_camera_mode(s: &str) -> bool {
    s == "auto" || parse_camera_mode(s).is_some()
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            autostart: false,
            start_minimized: false,
            theme: Theme::default(),
            phone_theme: PhoneTheme::default(),
            lan_enabled: true,
            camera_lens: CameraLens::default(),
            camera_mode: default_camera_mode(),
            camera_bitrate_kbps: 0,
            camera_mirror: MirrorMode::default(),
            camera_auto_start: true,
            camera_image: CameraImageSettings::default(),
            onboarding_done: false,
            mic_enabled: false,
            mic_device: String::new(),
            mic_voice_processing: true,
            mic_gain_db: 0,
            mic_guard_defaults: true,
            deck_sleep_enabled: true,
            deck_sleep_idle_s: DECK_SLEEP_IDLE_S_DEFAULT,
            deck_sleep_wake_on_camera_mic: true,
            deck_sleep_clock: true,
        }
    }
}

/// Phone deck theme, sent to the phone as `Appearance`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhoneTheme {
    #[default]
    Black,
    Graphite,
    Cream,
    Lavender,
    Mint,
    Dusk,
    Tide,
}

/// Partial update for [`Settings`] (`PUT /api/settings`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SettingsPatch {
    pub autostart: Option<bool>,
    pub start_minimized: Option<bool>,
    pub theme: Option<Theme>,
    pub phone_theme: Option<PhoneTheme>,
    pub lan_enabled: Option<bool>,
    pub camera_lens: Option<CameraLens>,
    pub camera_mode: Option<String>,
    pub camera_bitrate_kbps: Option<u32>,
    pub camera_mirror: Option<MirrorMode>,
    pub camera_auto_start: Option<bool>,
    /// Replaces the whole image settings set.
    pub camera_image: Option<CameraImageSettings>,
    pub onboarding_done: Option<bool>,
    pub mic_enabled: Option<bool>,
    pub mic_device: Option<String>,
    pub mic_voice_processing: Option<bool>,
    pub mic_gain_db: Option<i32>,
    pub mic_guard_defaults: Option<bool>,
    pub deck_sleep_enabled: Option<bool>,
    pub deck_sleep_idle_s: Option<u32>,
    pub deck_sleep_wake_on_camera_mic: Option<bool>,
    pub deck_sleep_clock: Option<bool>,
}

/// One phone adb sees over USB (`GET /api/phone/usb`, round 23).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsbPhone {
    pub serial: String,
    /// `ro.product.model`, empty when unknown (e.g. unauthorized).
    pub model: String,
    /// adb state: `"device"` (ready), `"unauthorized"` (accept the prompt on the phone), `"offline"`, ...
    pub state: String,
    /// `dev.notideck` is installed; `None` when it cannot be checked (not in `device` state).
    pub app_installed: Option<bool>,
    /// Installed `versionName`, when installed.
    pub app_version: Option<String>,
}

/// `GET /api/phone/usb`: what the setup wizard needs to install the phone app over USB.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PhoneUsbStatus {
    /// adb could be run at all.
    pub adb_ok: bool,
    /// A bundled phone app was found (`notideck-phone.apk` next to the exe, or in the
    /// `NOTIDECK_APK` path).
    pub apk_available: bool,
    pub phones: Vec<UsbPhone>,
}

/// `POST /api/phone/install` body: `adb -s <serial> install -r <bundled apk>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhoneInstallReq {
    pub serial: String,
}

/// `POST /api/camera/test` body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraTestReq {
    pub on: bool,
}

/// `POST /api/camera/control` body. Only set fields are applied.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CameraControlReq {
    pub torch: Option<bool>,
    /// Zoom ratio 1.0..8.0.
    pub zoom: Option<f32>,
    pub lens: Option<CameraLens>,
    /// Ask the phone for a key frame.
    pub keyframe: Option<bool>,
}

/// `POST /api/mic` body (round 25). Only set fields are applied.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MicControlReq {
    /// Switch the phone mic on/off (also saved as `settings.mic_enabled`).
    pub on: Option<bool>,
    /// PC-side mute (not saved).
    pub muted: Option<bool>,
}

// ---------------------------------------------------------------- pairing (round 5, ADR 0004)

/// A live QR pairing code (`POST /api/pairing`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PairingTicket {
    /// `notideck://pair?...` (contains the one-time secret: never log it).
    pub uri: String,
    /// QR code of `uri` as a standalone `<svg>` document, server-rendered.
    pub qr_svg: String,
    /// Unix seconds.
    pub expires_at: u64,
    /// LAN addresses written into the code, for the "phone must be on the same Wi-Fi" hint.
    pub addrs: Vec<String>,
    pub port: u16,
}

/// A pinned phone (`GET /api/devices/paired`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PairedDevice {
    /// Stable registry id: the lowercase hex cert fingerprint.
    pub id: String,
    pub name: String,
    pub model: String,
    /// First 8 hex chars of the fingerprint, for display.
    pub fp_short: String,
    /// Unix seconds.
    pub paired_at: u64,
    /// Unix seconds; 0 = never connected since pairing.
    pub last_seen: u64,
    pub connected: bool,
    /// "usb" | "lan" | "" (current link when connected, else the last one).
    pub transport: String,
}

/// An unknown phone on USB waiting for Allow/Deny (`GET /api/devices/pending`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PendingDevice {
    pub session_id: String,
    pub name: String,
    pub model: String,
    pub fp_short: String,
}

/// Availability of a host capability.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Ok,
    Missing,
    #[default]
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdbInfo {
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
}

/// Host diagnostics for the Settings and Devices tabs (`GET /api/host`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostInfo {
    pub version: String,
    /// e.g. "windows", "linux".
    pub os: String,
    pub control_port: u16,
    pub ui_port: u16,
    pub data_dir: String,
    pub log_dir: String,
    pub config_dir: String,
    pub adb: AdbInfo,
    pub keyboard_access: Access,
    /// e.g. "wasapi", "pactl", "wpctl", "none".
    pub audio_backend: String,
    /// e.g. "gsmtc", "mpris", "none".
    pub media_backend: String,
}

/// Failure reported by a [`ConfigBackend`], mapped to an HTTP status by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// 404
    NotFound(String),
    /// 409
    Conflict(String),
    /// 400
    Invalid(String),
    /// 500
    Internal(String),
}

impl BackendError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(m) | Self::Conflict(m) | Self::Invalid(m) | Self::Internal(m) => m,
        }
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for BackendError {}

impl IntoResponse for BackendError {
    fn into_response(self) -> Response {
        err(self.status(), self.message())
    }
}

/// Storage and control surface the config server needs from the agent.
///
/// Methods are synchronous; implementations should be quick (in-memory state
/// plus small file writes).
pub trait ConfigBackend: Send + Sync + 'static {
    fn list_profiles(&self) -> Vec<ProfileSummary>;
    fn get_profile(&self, id: &str) -> Option<Profile>;
    /// Create or replace a profile. Should also live-push it if active.
    fn put_profile(&self, profile: Profile) -> Result<(), BackendError>;
    /// Returns `Ok(true)` if a profile was removed.
    fn delete_profile(&self, id: &str) -> Result<bool, BackendError>;
    /// Store icon bytes (PNG/WebP/JPEG), returning the content hash.
    fn put_icon(&self, bytes: Vec<u8>) -> Result<String, BackendError>;
    fn get_icon(&self, hash: &str) -> Option<Vec<u8>>;
    fn activate_profile(&self, id: &str) -> Result<(), BackendError>;
    fn status(&self) -> Status;

    // ---- app shell / settings (round 4). Defaults keep test backends small.

    fn settings(&self) -> Settings {
        Settings::default()
    }
    /// Apply and persist a partial update (also toggles OS autostart). Returns the new settings.
    fn update_settings(&self, _patch: SettingsPatch) -> Result<Settings, BackendError> {
        Err(BackendError::Invalid("settings are not supported".into()))
    }
    fn host_info(&self) -> HostInfo {
        HostInfo::default()
    }
    /// Run a fix named by [`Problem::fix`] (e.g. `keyboard_access`).
    fn run_fix(&self, id: &str) -> Result<(), BackendError> {
        Err(BackendError::NotFound(format!("no fix '{id}'")))
    }
    /// Open `logs` or `config` in the OS file manager.
    fn open_folder(&self, which: &str) -> Result<(), BackendError> {
        Err(BackendError::NotFound(format!("no folder '{which}'")))
    }
    /// Ask the app shell to show and focus its window (second-instance launch).
    fn shell_show(&self) -> Result<(), BackendError> {
        Err(BackendError::NotFound("no app window".into()))
    }
    /// Quit the whole app (`Notideck.exe --quit`, used by the installer before it replaces files).
    /// Reply first, then shut down shortly after.
    fn shell_quit(&self) -> Result<(), BackendError> {
        Err(BackendError::NotFound("quit is not supported".into()))
    }

    // ---- first-run setup (round 23). Routes:
    //   GET  /api/phone/usb               -> PhoneUsbStatus
    //   POST /api/phone/install {serial}  -> 204 | 404 no apk / unknown serial | 409 adb failed (message)
    // Blocking calls (adb); the server runs them on a blocking thread.

    fn phone_usb_status(&self) -> PhoneUsbStatus {
        PhoneUsbStatus::default()
    }
    fn install_phone_app(&self, serial: &str) -> Result<(), BackendError> {
        Err(BackendError::NotFound(format!("cannot install on '{serial}'")))
    }

    // ---- pairing (round 5). Routes:
    //   POST   /api/pairing                          -> PairingTicket (replaces any live one)
    //   DELETE /api/pairing                          -> 204
    //   GET    /api/devices/paired                   -> [PairedDevice]
    //   DELETE /api/devices/paired/:id               -> 204 | 404 (revokes + closes the session)
    //   GET    /api/devices/pending                  -> [PendingDevice]
    //   POST   /api/devices/pending/:session/approve -> 204 | 404
    //   POST   /api/devices/pending/:session/deny    -> 204 | 404

    /// Issue a fresh single-use QR ticket (5 min). 409 when LAN is disabled.
    fn create_pairing(&self) -> Result<PairingTicket, BackendError> {
        Err(BackendError::NotFound("pairing is not supported".into()))
    }
    fn cancel_pairing(&self) {}
    fn paired_devices(&self) -> Vec<PairedDevice> {
        Vec::new()
    }
    fn remove_device(&self, id: &str) -> Result<(), BackendError> {
        Err(BackendError::NotFound(format!("no device '{id}'")))
    }
    fn pending_devices(&self) -> Vec<PendingDevice> {
        Vec::new()
    }
    fn resolve_pending(&self, session_id: &str, _allow: bool) -> Result<(), BackendError> {
        Err(BackendError::NotFound(format!("no pending phone '{session_id}'")))
    }

    // ---- webcam (round 6, ADR 0005). Routes:
    //   GET  /api/camera              -> status JSON (see the agent's camera manager)
    //   POST /api/camera/test {on}    -> status JSON; toggles a 1280x720 test consumer
    //   POST /api/camera/control      -> status JSON; {torch?, zoom?, lens?, keyframe?}
    //   (round 15) status JSON also has:
    //     phone.lens_caps: [{lens:"back"|"front"|"external", focus_manual, focus_lock, exposure_manual,
    //       iso_min, iso_max, exposure_us_min, exposure_us_max, ev_min, ev_max, ev_step, ae_lock, awb_lock,
    //       white_balances:[WhiteBalance snake_case], antibandings:["auto"|"off"|"50hz"|"60hz"],
    //       stabilization, zoom_max}]   ([] from older phones)
    //     readout: {iso, exposure_us, focus_permille, focused} | null   (latest phone CameraReadout while streaming)
    //     image: CameraImageSettings  (same as settings.camera_image)
    //   Image settings are changed with PATCH /api/settings {camera_image: {...whole object...}}.
    //   GET  /api/camera/preview.png  -> small PNG of the last frame | 404

    fn camera_status(&self) -> serde_json::Value {
        serde_json::json!({ "state": "unsupported", "message": "The webcam is not available." })
    }
    fn camera_test(&self, _on: bool) -> Result<serde_json::Value, BackendError> {
        Err(BackendError::NotFound("the webcam is not available".into()))
    }
    fn camera_control(&self, _req: CameraControlReq) -> Result<serde_json::Value, BackendError> {
        Err(BackendError::NotFound("the webcam is not available".into()))
    }
    fn camera_preview(&self) -> Option<Vec<u8>> {
        None
    }

    // ---- phone microphone (round 25). Routes:
    //   GET  /api/mic                 -> status JSON (shape in docs/PLAN.md round 25)
    //   POST /api/mic {on?, muted?}   -> status JSON
    //   Device, gain, voice processing, guard: PATCH /api/settings {mic_*}.

    fn mic_status(&self) -> serde_json::Value {
        serde_json::json!({ "state": "unsupported", "message": "The phone microphone is not available." })
    }
    fn mic_control(&self, _req: MicControlReq) -> Result<serde_json::Value, BackendError> {
        Err(BackendError::NotFound("the phone microphone is not available".into()))
    }

    // ---- auto profile switching (round 8, M5). Routes:
    //   GET  /api/autoswitch       -> { config, locked, backend, available, error?, current?, recent, active_rule_id? }
    //   PUT  /api/autoswitch       -> 204 | 400 { errors: [RuleError] }
    //   POST /api/autoswitch/lock  -> 204   body { locked }

    fn autoswitch_status(&self) -> serde_json::Value {
        serde_json::json!({
            "config": nd_core::autoswitch::AutoSwitchConfig::default(),
            "locked": false,
            "backend": "none",
            "available": false,
            "error": "Auto-switch is not supported here.",
            "recent": [],
        })
    }
    /// Validate, persist and apply a new auto-switch configuration.
    fn autoswitch_put(&self, _config: nd_core::autoswitch::AutoSwitchConfig) -> Result<(), AutoSwitchPutError> {
        Err(AutoSwitchPutError::Backend(BackendError::NotFound("auto-switch is not supported".into())))
    }
    fn autoswitch_lock(&self, _locked: bool) -> Result<(), BackendError> {
        Err(BackendError::NotFound("auto-switch is not supported".into()))
    }

    // ---- per-app mixer (round 12, M4). Routes:
    //   GET  /api/mixer         -> { backend, available, error?, has_peaks, config: MixerConfig,
    //                                apps: [MixerApp + icon_hash?] }   (visible in slot order, then hidden)
    //   PUT  /api/mixer/config  -> 204          body MixerConfig (persisted to mixer.json)
    //   POST /api/mixer/app     -> 204 | 404    body { key, volume?: f32, muted?: bool }

    fn mixer_status(&self) -> serde_json::Value {
        serde_json::json!({
            "backend": "none",
            "available": false,
            "error": "The per-app mixer is not supported here.",
            "has_peaks": false,
            "config": nd_core::MixerConfig::default(),
            "apps": [],
        })
    }
    fn mixer_put_config(&self, _config: nd_core::MixerConfig) -> Result<(), BackendError> {
        Err(BackendError::NotFound("the mixer is not supported".into()))
    }
    fn mixer_set_app(&self, _key: &str, _volume: Option<f32>, _muted: Option<bool>) -> Result<(), BackendError> {
        Err(BackendError::NotFound("the mixer is not supported".into()))
    }

    // ---- OBS Studio (round 14, M8). Routes (R14-3 adds them):
    //   GET  /api/obs                -> { settings: { enabled, auto_detect, host, port, has_password },
    //                                     status, endpoint, from_detected, obs_version?, ws_version?, last_error?,
    //                                     detected: { path, exists, server_enabled, server_port, auth_required,
    //                                                 has_password } | null,
    //                                     studio_mode, program_scene?, scenes: [string],
    //                                     inputs: [{ name, kind, audio: bool }], special: { mic1: "Mic/Aux", … },
    //                                     scene_items: { "<scene>": ["<source>", …] } }
    //                                   (status = nd_core::obs::ObsStatus snake_case; lists empty when not connected)
    //   PUT  /api/obs/config         -> 204 | 400   body ObsConfigPut (persisted to obs.json, client reconnects)
    //   POST /api/obs/enable_server  -> 200 { detected } | 409 { error }   (writes OBS's websocket settings; the
    //                                   UI then tells the user to start / restart OBS)
    //   POST /api/obs/reconnect      -> 204

    fn obs_status(&self) -> serde_json::Value {
        serde_json::json!({ "settings": null, "status": "disabled", "detected": null, "scenes": [], "inputs": [],
                            "special": {}, "scene_items": {}, "studio_mode": false })
    }
    fn obs_put_config(&self, _config: ObsConfigPut) -> Result<(), BackendError> {
        Err(BackendError::NotFound("OBS integration is not available".into()))
    }
    fn obs_enable_server(&self) -> Result<serde_json::Value, BackendError> {
        Err(BackendError::NotFound("OBS integration is not available".into()))
    }
    fn obs_reconnect(&self) {}

    // ---- installed-app picker (round 13). Routes:
    //   GET /api/apps[?refresh=1] -> { backend, available, error?, apps: [InstalledApp] }   (cached; refresh re-lists)
    //   GET /api/apps/icon?id=<id> -> image/png | 404

    fn apps_list(&self, _refresh: bool) -> serde_json::Value {
        serde_json::json!({ "backend": "none", "available": false, "error": "The app list is not supported here.", "apps": [] })
    }
    fn app_icon(&self, _id: &str) -> Option<Vec<u8>> {
        None
    }

    // ---- website icons for "Open URL" keys. Route:
    //   GET /api/site-icon?url=<http(s) URL> -> image bytes (Content-Type set) | 400 bad url | 404 no icon
    // Returns (content type, bytes). Blocking (network): the route runs it in spawn_blocking.
    fn site_icon(&self, _url: &str) -> Result<(String, Vec<u8>), BackendError> {
        Err(BackendError::NotFound("website icons are not supported here".into()))
    }

    // ---- side rail pinned keys (round 11). Routes:
    //   GET /api/rail -> { keys: [Control] }
    //   PUT /api/rail -> 204 | 400 { errors: [string] }     body { keys: [Control] }

    fn rail_get(&self) -> Vec<nd_core::Control> {
        Vec::new()
    }
    /// Validate, persist and push a new rail. `Err(messages)` becomes a 400.
    fn rail_put(&self, _keys: Vec<nd_core::Control>) -> Result<(), RailPutError> {
        Err(RailPutError::Backend(BackendError::NotFound("the side rail is not supported".into())))
    }

    // ---- plugins (round 19, M9; docs/PLUGINS.md). Routes:
    //   GET    /api/plugins                    -> { available, plugins: [nd_plugin::PluginInfo] }
    //   GET    /api/plugins/:id/file?path=…    -> bytes (png / jpeg / svg+xml by extension) | 404
    //   GET    /api/plugins/:id/log            -> { lines: [string] } | 404
    //   POST   /api/plugins/:id/:cmd           -> 204 | 404 | 400    cmd = enable|disable|approve|restart
    //   DELETE /api/plugins/:id                -> 204 | 404 | 400    (user plugins only)
    //   POST   /api/plugins/reload             -> 204
    //   POST   /api/plugins/install            -> 200 PluginInfo | 400 { error }   body: raw .zip (≤ 20 MB)
    //   POST   /api/open/plugins               -> opens the user plugin folder (existing open route)

    /// `{ available, plugins: [PluginInfo] }`.
    fn plugins_list(&self) -> serde_json::Value {
        serde_json::json!({ "available": false, "plugins": [] })
    }
    /// `cmd`: `enable` | `disable` | `approve` | `restart` | `uninstall`.
    fn plugin_command(&self, _id: &str, _cmd: &str) -> Result<(), BackendError> {
        Err(BackendError::NotFound("plugins are not available".into()))
    }
    fn plugins_reload(&self) {}
    /// Returns the installed plugin's `PluginInfo` as JSON.
    fn plugin_install(&self, _zip: Vec<u8>) -> Result<serde_json::Value, BackendError> {
        Err(BackendError::NotFound("plugins are not available".into()))
    }
    fn plugin_log(&self, _id: &str) -> Option<Vec<String>> {
        None
    }
    /// Bytes of a file inside the plugin folder.
    fn plugin_file(&self, _id: &str, _path: &str) -> Option<Vec<u8>> {
        None
    }
}

/// Body of `PUT /api/obs/config` (round 14). `password` absent = keep the stored one; `""` clears it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ObsConfigPut {
    pub enabled: bool,
    pub auto_detect: bool,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub password: Option<String>,
}

/// Failure of [`ConfigBackend::rail_put`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RailPutError {
    /// 400 with `{ "errors": [...] }`.
    Invalid(Vec<String>),
    Backend(BackendError),
}

/// Failure of [`ConfigBackend::autoswitch_put`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoSwitchPutError {
    /// 400 with `{ "errors": [...] }`.
    Invalid(Vec<nd_core::autoswitch::RuleError>),
    Backend(BackendError),
}

/// `POST /api/autoswitch/lock` body.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct AutoSwitchLockReq {
    pub locked: bool,
}

/// `POST /api/mixer/app` body.
#[derive(Debug, Clone, Deserialize)]
pub struct MixerAppReq {
    pub key: String,
    #[serde(default)]
    pub volume: Option<f32>,
    #[serde(default)]
    pub muted: Option<bool>,
}

#[derive(Clone)]
struct AppState {
    backend: Arc<dyn ConfigBackend>,
    token: Option<Arc<str>>,
    port: u16,
}

/// Build the axum router. `port` is used for Host header validation.
pub fn router(backend: Arc<dyn ConfigBackend>, token: Option<String>, port: u16) -> Router {
    let state = AppState { backend, token: token.map(Arc::from), port };
    let api = Router::new()
        .route("/profiles", get(list_profiles))
        .route("/profiles/:id", get(get_profile).put(put_profile).delete(delete_profile))
        .route("/profiles/:id/activate", post(activate_profile))
        .route("/icons", post(post_icon))
        .route("/icons/:hash", get(get_icon))
        .route("/status", get(get_status))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/host", get(get_host))
        .route("/fix/:id", post(run_fix))
        .route("/open/:which", post(open_folder))
        .route("/shell/show", post(shell_show))
        .route("/shell/quit", post(shell_quit))
        .route("/phone/usb", get(get_phone_usb))
        .route("/phone/install", post(post_phone_install))
        .route("/pairing", post(post_pairing).delete(delete_pairing))
        .route("/camera", get(get_camera))
        .route("/camera/test", post(post_camera_test))
        .route("/camera/control", post(post_camera_control))
        .route("/camera/preview.png", get(get_camera_preview))
        .route("/mic", get(get_mic).post(post_mic))
        .route("/rail", get(get_rail).put(put_rail))
        .route("/autoswitch", get(get_autoswitch).put(put_autoswitch))
        .route("/autoswitch/lock", post(post_autoswitch_lock))
        .route("/apps", get(get_apps))
        .route("/apps/icon", get(get_app_icon))
        .route("/site-icon", get(get_site_icon))
        .route("/mixer", get(get_mixer))
        .route("/mixer/config", axum::routing::put(put_mixer_config))
        .route("/mixer/app", post(post_mixer_app))
        .route("/obs", get(get_obs))
        .route("/obs/config", axum::routing::put(put_obs_config))
        .route("/obs/enable_server", post(post_obs_enable_server))
        .route("/obs/reconnect", post(post_obs_reconnect))
        .route("/plugins", get(get_plugins))
        .route("/plugins/reload", post(post_plugins_reload))
        .route("/plugins/install", post(post_plugin_install).layer(DefaultBodyLimit::max(MAX_PLUGIN_ZIP_BYTES)))
        .route("/plugins/:id", delete(delete_plugin))
        .route("/plugins/:id/file", get(get_plugin_file))
        .route("/plugins/:id/log", get(get_plugin_log))
        .route("/plugins/:id/:cmd", post(post_plugin_cmd))
        .route("/devices/paired", get(get_paired))
        .route("/devices/paired/:id", delete(delete_paired))
        .route("/devices/pending", get(get_pending))
        .route("/devices/pending/:session/approve", post(approve_pending))
        .route("/devices/pending/:session/deny", post(deny_pending))
        .layer(DefaultBodyLimit::max(MAX_PROFILE_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .nest("/api", api)
        .fallback(get(static_file))
        .layer(middleware::from_fn_with_state(state.clone(), check_host))
        .with_state(state)
}

/// Bind to `127.0.0.1:port` and serve until the future is dropped.
pub async fn serve(backend: Arc<dyn ConfigBackend>, token: Option<String>, port: u16) -> std::io::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("config UI on http://{addr}");
    axum::serve(listener, router(backend, token, port)).await
}

// ---- middleware ----

async fn check_host(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| {
            let p = st.port;
            h == format!("127.0.0.1:{p}") || h == format!("localhost:{p}")
        })
        .unwrap_or(false);
    if ok {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "bad host").into_response()
    }
}

async fn require_token(State(st): State<AppState>, req: Request, next: Next) -> Response {
    if let Some(expected) = &st.token {
        let from_header = req.headers().get("x-nd-token").and_then(|h| h.to_str().ok()).map(str::to_owned);
        let from_query =
            req.uri().query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=").map(str::to_owned)));
        let given = from_header.or(from_query).unwrap_or_default();
        if !constant_time_eq(given.as_bytes(), expected.as_bytes()) {
            return err(StatusCode::UNAUTHORIZED, "missing or wrong token");
        }
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---- helpers ----

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    }
}

// ---- handlers ----

async fn list_profiles(State(st): State<AppState>) -> Response {
    Json(st.backend.list_profiles()).into_response()
}

async fn get_profile(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match st.backend.get_profile(&id) {
        Some(p) => Json(p).into_response(),
        None => err(StatusCode::NOT_FOUND, "no such profile"),
    }
}

async fn put_profile(State(st): State<AppState>, Path(id): Path<String>, Json(mut profile): Json<Profile>) -> Response {
    if !valid_id(&id) {
        return err(StatusCode::BAD_REQUEST, "invalid profile id");
    }
    profile.id = id;
    match st.backend.put_profile(profile) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

async fn delete_profile(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match st.backend.delete_profile(&id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "no such profile"),
        Err(e) => e.into_response(),
    }
}

async fn activate_profile(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match st.backend.activate_profile(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

async fn post_icon(State(st): State<AppState>, body: Bytes) -> Response {
    if body.is_empty() || body.len() > MAX_ICON_BYTES {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "icon must be 1 B to 2 MiB");
    }
    if sniff_image(&body).is_none() {
        return err(StatusCode::UNSUPPORTED_MEDIA_TYPE, "PNG, WebP or JPEG only");
    }
    match st.backend.put_icon(body.to_vec()) {
        Ok(hash) => Json(serde_json::json!({ "hash": hash })).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn get_icon(State(st): State<AppState>, Path(hash): Path<String>) -> Response {
    if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return err(StatusCode::BAD_REQUEST, "invalid hash");
    }
    match st.backend.get_icon(&hash) {
        Some(bytes) => {
            let mime = sniff_image(&bytes).unwrap_or("application/octet-stream");
            (
                [
                    (header::CONTENT_TYPE, HeaderValue::from_static(mime)),
                    (header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable")),
                ],
                bytes,
            )
                .into_response()
        }
        None => err(StatusCode::NOT_FOUND, "no such icon"),
    }
}

async fn get_status(State(st): State<AppState>) -> Response {
    Json(st.backend.status()).into_response()
}

async fn get_settings(State(st): State<AppState>) -> Response {
    Json(st.backend.settings()).into_response()
}

async fn put_settings(State(st): State<AppState>, Json(patch): Json<SettingsPatch>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.update_settings(patch)).await {
        Ok(Ok(settings)) => Json(settings).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "settings task failed"),
    }
}

async fn get_camera(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.camera_status()).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "camera task failed"),
    }
}

async fn post_camera_test(State(st): State<AppState>, Json(req): Json<CameraTestReq>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.camera_test(req.on)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "camera task failed"),
    }
}

async fn post_camera_control(State(st): State<AppState>, Json(req): Json<CameraControlReq>) -> Response {
    if req.zoom.is_some_and(|z| !z.is_finite()) {
        return err(StatusCode::BAD_REQUEST, "invalid zoom");
    }
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.camera_control(req)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "camera task failed"),
    }
}

async fn get_mic(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.mic_status()).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "mic task failed"),
    }
}

async fn post_mic(State(st): State<AppState>, Json(req): Json<MicControlReq>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.mic_control(req)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "mic task failed"),
    }
}

async fn get_rail(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.rail_get()).await {
        Ok(keys) => Json(serde_json::json!({ "keys": keys })).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "rail task failed"),
    }
}

async fn put_rail(State(st): State<AppState>, Json(cfg): Json<nd_core::RailConfig>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.rail_put(cfg.keys)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(RailPutError::Invalid(errors))) => {
            (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "errors": errors }))).into_response()
        }
        Ok(Err(RailPutError::Backend(e))) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "rail task failed"),
    }
}

async fn get_autoswitch(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.autoswitch_status()).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "auto-switch task failed"),
    }
}

async fn put_autoswitch(
    State(st): State<AppState>,
    Json(cfg): Json<nd_core::autoswitch::AutoSwitchConfig>,
) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.autoswitch_put(cfg)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(AutoSwitchPutError::Invalid(errors))) => {
            (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "errors": errors }))).into_response()
        }
        Ok(Err(AutoSwitchPutError::Backend(e))) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "auto-switch task failed"),
    }
}

async fn post_autoswitch_lock(State(st): State<AppState>, Json(req): Json<AutoSwitchLockReq>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.autoswitch_lock(req.locked)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "auto-switch task failed"),
    }
}

async fn get_apps(State(st): State<AppState>, Query(q): Query<std::collections::HashMap<String, String>>) -> Response {
    let refresh = q.get("refresh").is_some_and(|v| v == "1" || v == "true");
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.apps_list(refresh)).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "app list task failed"),
    }
}

/// PNG unless the bytes look like SVG (Linux icon themes ship both).
fn sniff_icon_type(bytes: &[u8]) -> &'static str {
    let head = &bytes[..bytes.len().min(1024)];
    if !bytes.starts_with(b"\x89PNG") && (head.starts_with(b"<?xml") || head.windows(4).any(|w| w == b"<svg")) {
        "image/svg+xml"
    } else {
        "image/png"
    }
}

async fn get_app_icon(
    State(st): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(id) = q.get("id").filter(|i| !i.is_empty() && i.len() <= 1024).cloned() else {
        return err(StatusCode::BAD_REQUEST, "missing id");
    };
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.app_icon(&id)).await {
        Ok(Some(bytes)) => (
            [
                (header::CONTENT_TYPE, HeaderValue::from_static(sniff_icon_type(&bytes))),
                (header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600")),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "no icon for that app"),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "app icon task failed"),
    }
}

async fn get_site_icon(
    State(st): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(url) = q.get("url").filter(|u| !u.is_empty() && u.len() <= 2048).cloned() else {
        return err(StatusCode::BAD_REQUEST, "missing url");
    };
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.site_icon(&url)).await {
        Ok(Ok((ctype, bytes))) => match HeaderValue::from_str(&ctype) {
            Ok(ct) => (
                [
                    (header::CONTENT_TYPE, ct),
                    (header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600")),
                ],
                bytes,
            )
                .into_response(),
            Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "bad icon content type"),
        },
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "site icon task failed"),
    }
}

// ---- plugins (round 19) ----

/// Plugin ids are reverse-DNS style (`dev.notideck.clock`).
fn valid_plugin_id(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

async fn get_plugins(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.plugins_list()).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "plugin list task failed"),
    }
}

async fn get_plugin_file(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(path) = q.get("path").filter(|p| !p.is_empty() && p.len() <= 512).cloned() else {
        return err(StatusCode::BAD_REQUEST, "missing path");
    };
    if !valid_plugin_id(&id) {
        return err(StatusCode::BAD_REQUEST, "invalid plugin id");
    }
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        _ => return err(StatusCode::NOT_FOUND, "not an image file"),
    };
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.plugin_file(&id, &path)).await {
        Ok(Some(bytes)) => (
            [
                (header::CONTENT_TYPE, HeaderValue::from_static(mime)),
                (header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600")),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "no such plugin file"),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "plugin file task failed"),
    }
}

async fn get_plugin_log(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    if !valid_plugin_id(&id) {
        return err(StatusCode::BAD_REQUEST, "invalid plugin id");
    }
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.plugin_log(&id)).await {
        Ok(Some(lines)) => Json(serde_json::json!({ "lines": lines })).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "no such plugin"),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "plugin log task failed"),
    }
}

async fn run_plugin_command(st: AppState, id: String, cmd: &'static str) -> Response {
    if !valid_plugin_id(&id) {
        return err(StatusCode::BAD_REQUEST, "invalid plugin id");
    }
    let backend = st.backend.clone();
    // May block for a few seconds while the plugin process stops.
    match tokio::task::spawn_blocking(move || backend.plugin_command(&id, cmd)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "plugin command task failed"),
    }
}

async fn post_plugin_cmd(State(st): State<AppState>, Path((id, cmd)): Path<(String, String)>) -> Response {
    let cmd = match cmd.as_str() {
        "enable" => "enable",
        "disable" => "disable",
        "approve" => "approve",
        "restart" => "restart",
        _ => return err(StatusCode::BAD_REQUEST, "unknown plugin command"),
    };
    run_plugin_command(st, id, cmd).await
}

async fn delete_plugin(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    run_plugin_command(st, id, "uninstall").await
}

async fn post_plugins_reload(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.plugins_reload()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "plugin reload task failed"),
    }
}

async fn post_plugin_install(State(st): State<AppState>, body: Bytes) -> Response {
    if body.is_empty() {
        return err(StatusCode::BAD_REQUEST, "empty body; send the .zip bytes");
    }
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.plugin_install(body.to_vec())).await {
        Ok(Ok(info)) => Json(info).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "plugin install task failed"),
    }
}

async fn get_mixer(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.mixer_status()).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "mixer task failed"),
    }
}

async fn put_mixer_config(State(st): State<AppState>, Json(cfg): Json<nd_core::MixerConfig>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.mixer_put_config(cfg)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "mixer task failed"),
    }
}

async fn post_mixer_app(State(st): State<AppState>, Json(req): Json<MixerAppReq>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.mixer_set_app(&req.key, req.volume, req.muted)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "mixer task failed"),
    }
}

async fn get_obs(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.obs_status()).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "OBS task failed"),
    }
}

async fn put_obs_config(State(st): State<AppState>, Json(cfg): Json<ObsConfigPut>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.obs_put_config(cfg)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "OBS task failed"),
    }
}

async fn post_obs_enable_server(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.obs_enable_server()).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "OBS task failed"),
    }
}

async fn post_obs_reconnect(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.obs_reconnect()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "OBS task failed"),
    }
}

async fn get_camera_preview(State(st): State<AppState>) -> Response {
    match st.backend.camera_preview() {
        Some(png) => ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-store")], png).into_response(),
        None => err(StatusCode::NOT_FOUND, "no frame yet"),
    }
}

async fn get_host(State(st): State<AppState>) -> Response {
    Json(st.backend.host_info()).into_response()
}

fn valid_fix_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

/// May block for a long time (polkit dialog); intentionally not time-limited.
async fn run_fix(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    if !valid_fix_id(&id) {
        return err(StatusCode::BAD_REQUEST, "invalid fix id");
    }
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.run_fix(&id)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "fix task failed"),
    }
}

async fn open_folder(State(st): State<AppState>, Path(which): Path<String>) -> Response {
    if !matches!(which.as_str(), "logs" | "config" | "plugins") {
        return err(StatusCode::NOT_FOUND, "unknown folder");
    }
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.open_folder(&which)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "open task failed"),
    }
}

async fn shell_show(State(st): State<AppState>) -> Response {
    match st.backend.shell_show() {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

async fn shell_quit(State(st): State<AppState>) -> Response {
    match st.backend.shell_quit() {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

async fn get_phone_usb(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.phone_usb_status()).await {
        Ok(s) => Json(s).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "adb task failed"),
    }
}

async fn post_phone_install(State(st): State<AppState>, Json(req): Json<PhoneInstallReq>) -> Response {
    if !valid_serial(&req.serial) {
        return err(StatusCode::BAD_REQUEST, "bad serial");
    }
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.install_phone_app(&req.serial)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "install task failed"),
    }
}

/// adb serials: alphanumerics plus `.:-_` (USB serials, `ip:port`, `emulator-5554`).
fn valid_serial(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || b".:-_".contains(&c))
}

/// Ids and session ids are short opaque tokens (hex / uuid-like); reject anything else early.
fn valid_token(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

async fn post_pairing(State(st): State<AppState>) -> Response {
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.create_pairing()).await {
        Ok(Ok(ticket)) => Json(ticket).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "pairing task failed"),
    }
}

async fn delete_pairing(State(st): State<AppState>) -> Response {
    st.backend.cancel_pairing();
    StatusCode::NO_CONTENT.into_response()
}

async fn get_paired(State(st): State<AppState>) -> Response {
    Json(st.backend.paired_devices()).into_response()
}

async fn delete_paired(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    if !valid_token(&id) {
        return err(StatusCode::NOT_FOUND, "no such device");
    }
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.remove_device(&id)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "remove task failed"),
    }
}

async fn get_pending(State(st): State<AppState>) -> Response {
    Json(st.backend.pending_devices()).into_response()
}

async fn resolve(st: AppState, session: String, allow: bool) -> Response {
    if !valid_token(&session) {
        return err(StatusCode::NOT_FOUND, "no such pending phone");
    }
    let backend = st.backend.clone();
    match tokio::task::spawn_blocking(move || backend.resolve_pending(&session, allow)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "resolve task failed"),
    }
}

async fn approve_pending(State(st): State<AppState>, Path(session): Path<String>) -> Response {
    resolve(st, session, true).await
}

async fn deny_pending(State(st): State<AppState>, Path(session): Path<String>) -> Response {
    resolve(st, session, false).await
}

/// Render `data` as a standalone QR `<svg>` document: black modules on a white rect with a
/// 4-module quiet zone, so it scans in both UI themes. Empty string if `data` cannot be encoded.
pub fn qr_svg(data: &str) -> String {
    use qrcode::{Color, EcLevel, QrCode};
    let Ok(code) = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M) else {
        return String::new();
    };
    let n = code.width();
    let colors = code.to_colors();
    const QUIET: usize = 4;
    let size = n + 2 * QUIET;
    let mut path = String::new();
    for y in 0..n {
        let mut x = 0;
        while x < n {
            if colors[y * n + x] == Color::Dark {
                let start = x;
                while x < n && colors[y * n + x] == Color::Dark {
                    x += 1;
                }
                path.push_str(&format!("M{} {}h{}v1h-{}z", start + QUIET, y + QUIET, x - start, x - start));
            } else {
                x += 1;
            }
        }
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size} {size}\" shape-rendering=\"crispEdges\" role=\"img\" aria-label=\"Pairing QR code\">\
<rect width=\"{size}\" height=\"{size}\" fill=\"#fff\"/><path d=\"{path}\" fill=\"#000\"/></svg>"
    )
}

async fn static_file(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if path.contains("..") {
        return err(StatusCode::BAD_REQUEST, "bad path");
    }
    match UI.get_file(path) {
        Some(f) => {
            let mime = match path.rsplit('.').next().unwrap_or("") {
                "html" => "text/html; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "js" | "mjs" => "text/javascript; charset=utf-8",
                "svg" => "image/svg+xml",
                "json" => "application/json",
                "png" => "image/png",
                "woff2" => "font/woff2",
                _ => "application/octet-stream",
            };
            (
                [
                    (header::CONTENT_TYPE, HeaderValue::from_static(mime)),
                    (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
                    (
                        header::CONTENT_SECURITY_POLICY,
                        HeaderValue::from_static(
                            "default-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'",
                        ),
                    ),
                ],
                f.contents(),
            )
                .into_response()
        }
        None => err(StatusCode::NOT_FOUND, "not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_theme_serde_round_trips_all_variants() {
        let all = [
            (PhoneTheme::Black, "black"),
            (PhoneTheme::Graphite, "graphite"),
            (PhoneTheme::Cream, "cream"),
            (PhoneTheme::Lavender, "lavender"),
            (PhoneTheme::Mint, "mint"),
            (PhoneTheme::Dusk, "dusk"),
            (PhoneTheme::Tide, "tide"),
        ];
        for (t, name) in all {
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(json, format!("\"{name}\""));
            assert_eq!(serde_json::from_str::<PhoneTheme>(&json).unwrap(), t);
        }
        assert!(serde_json::from_str::<PhoneTheme>("\"neon\"").is_err());
    }

    #[test]
    fn backend_errors_map_to_status_codes() {
        let cases = [
            (BackendError::NotFound("a".into()), StatusCode::NOT_FOUND),
            (BackendError::Conflict("b".into()), StatusCode::CONFLICT),
            (BackendError::Invalid("c".into()), StatusCode::BAD_REQUEST),
            (BackendError::Internal("d".into()), StatusCode::INTERNAL_SERVER_ERROR),
        ];
        for (e, code) in cases {
            let msg = e.message().to_string();
            let resp = e.into_response();
            assert_eq!(resp.status(), code);
            assert!(!msg.is_empty());
        }
    }

    // ---- HTTP route tests ----

    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::sync::Mutex;
    use tower::ServiceExt;

    const PORT: u16 = 47801;
    const TOKEN: &str = "sekrit";

    #[derive(Default)]
    struct Fake {
        settings: Mutex<Settings>,
        calls: Mutex<Vec<String>>,
        paired: Mutex<Vec<PairedDevice>>,
        pending: Mutex<Vec<PendingDevice>>,
        lan_off: Mutex<bool>,
        rail: Mutex<Vec<nd_core::Control>>,
    }

    impl ConfigBackend for Fake {
        fn list_profiles(&self) -> Vec<ProfileSummary> {
            vec![]
        }
        fn get_profile(&self, _: &str) -> Option<Profile> {
            None
        }
        fn put_profile(&self, _: Profile) -> Result<(), BackendError> {
            Ok(())
        }
        fn delete_profile(&self, _: &str) -> Result<bool, BackendError> {
            Ok(false)
        }
        fn put_icon(&self, _: Vec<u8>) -> Result<String, BackendError> {
            Ok("00".into())
        }
        fn get_icon(&self, _: &str) -> Option<Vec<u8>> {
            None
        }
        fn activate_profile(&self, _: &str) -> Result<(), BackendError> {
            Ok(())
        }
        fn status(&self) -> Status {
            Status::default()
        }
        fn settings(&self) -> Settings {
            self.settings.lock().unwrap().clone()
        }
        fn update_settings(&self, p: SettingsPatch) -> Result<Settings, BackendError> {
            let mut s = self.settings.lock().unwrap();
            if let Some(v) = p.autostart {
                s.autostart = v;
            }
            if let Some(v) = p.start_minimized {
                s.start_minimized = v;
            }
            if let Some(v) = p.theme {
                s.theme = v;
            }
            if let Some(v) = p.lan_enabled {
                s.lan_enabled = v;
                *self.lan_off.lock().unwrap() = !v;
            }
            if let Some(v) = p.camera_lens {
                s.camera_lens = v;
            }
            Ok(s.clone())
        }
        fn create_pairing(&self) -> Result<PairingTicket, BackendError> {
            if *self.lan_off.lock().unwrap() {
                return Err(BackendError::Conflict("Wi-Fi connections are off".into()));
            }
            self.calls.lock().unwrap().push("pair".into());
            Ok(PairingTicket {
                uri: "notideck://pair?x=1".into(),
                qr_svg: qr_svg("notideck://pair?x=1"),
                expires_at: 1_700_000_300,
                addrs: vec!["192.168.1.5".into()],
                port: 47802,
            })
        }
        fn cancel_pairing(&self) {
            self.calls.lock().unwrap().push("cancel".into());
        }
        fn paired_devices(&self) -> Vec<PairedDevice> {
            self.paired.lock().unwrap().clone()
        }
        fn remove_device(&self, id: &str) -> Result<(), BackendError> {
            let mut p = self.paired.lock().unwrap();
            let before = p.len();
            p.retain(|d| d.id != id);
            if p.len() == before {
                return Err(BackendError::NotFound(format!("no device '{id}'")));
            }
            Ok(())
        }
        fn pending_devices(&self) -> Vec<PendingDevice> {
            self.pending.lock().unwrap().clone()
        }
        fn resolve_pending(&self, session_id: &str, allow: bool) -> Result<(), BackendError> {
            let mut p = self.pending.lock().unwrap();
            let Some(i) = p.iter().position(|d| d.session_id == session_id) else {
                return Err(BackendError::NotFound(format!("no pending phone '{session_id}'")));
            };
            let d = p.remove(i);
            if allow {
                self.paired.lock().unwrap().push(PairedDevice {
                    id: d.session_id.clone(),
                    name: d.name,
                    ..Default::default()
                });
            }
            self.calls.lock().unwrap().push(format!("{}:{session_id}", if allow { "allow" } else { "deny" }));
            Ok(())
        }
        fn rail_get(&self) -> Vec<nd_core::Control> {
            self.rail.lock().unwrap().clone()
        }
        fn rail_put(&self, keys: Vec<nd_core::Control>) -> Result<(), RailPutError> {
            let errors = nd_core::validate_rail(&keys);
            if !errors.is_empty() {
                return Err(RailPutError::Invalid(errors));
            }
            *self.rail.lock().unwrap() = keys;
            self.calls.lock().unwrap().push("rail-put".into());
            Ok(())
        }
        fn autoswitch_status(&self) -> serde_json::Value {
            serde_json::json!({ "config": { "enabled": true, "rules": [] }, "locked": false,
                "backend": "fake", "available": true, "recent": [] })
        }
        fn autoswitch_put(&self, c: nd_core::autoswitch::AutoSwitchConfig) -> Result<(), AutoSwitchPutError> {
            if c.rules.iter().any(|r| r.pattern.is_empty()) {
                return Err(AutoSwitchPutError::Invalid(vec![nd_core::autoswitch::RuleError {
                    rule_id: Some("r1".into()),
                    message: "empty pattern".into(),
                }]));
            }
            self.calls.lock().unwrap().push(format!("as-put:{}", c.rules.len()));
            Ok(())
        }
        fn autoswitch_lock(&self, locked: bool) -> Result<(), BackendError> {
            self.calls.lock().unwrap().push(format!("as-lock:{locked}"));
            Ok(())
        }
        fn apps_list(&self, refresh: bool) -> serde_json::Value {
            self.calls.lock().unwrap().push(format!("apps:{refresh}"));
            serde_json::json!({ "backend": "fake", "available": true, "apps": [{"id": "a b", "name": "A", "target": "a"}] })
        }
        fn app_icon(&self, id: &str) -> Option<Vec<u8>> {
            (id == "a b").then(|| b"\x89PNG\r\n\x1a\nx".to_vec())
        }
        fn site_icon(&self, url: &str) -> Result<(String, Vec<u8>), BackendError> {
            match url {
                "https://ok.example/" => Ok(("image/png".into(), b"\x89PNG\r\n\x1a\nx".to_vec())),
                "https://none.example/" => Err(BackendError::NotFound("no icon".into())),
                _ => Err(BackendError::Invalid("bad url".into())),
            }
        }
        fn mixer_status(&self) -> serde_json::Value {
            serde_json::json!({ "backend": "fake", "available": true, "has_peaks": false,
                "config": nd_core::MixerConfig::default(), "apps": [] })
        }
        fn mixer_put_config(&self, c: nd_core::MixerConfig) -> Result<(), BackendError> {
            self.calls.lock().unwrap().push(format!("mx-cfg:{}", c.hidden.join(",")));
            Ok(())
        }
        fn mixer_set_app(&self, key: &str, volume: Option<f32>, muted: Option<bool>) -> Result<(), BackendError> {
            if key != "spotify" {
                return Err(BackendError::NotFound(format!("no app '{key}'")));
            }
            self.calls.lock().unwrap().push(format!("mx-app:{key}:{volume:?}:{muted:?}"));
            Ok(())
        }
        fn obs_status(&self) -> serde_json::Value {
            serde_json::json!({ "status": "connected", "scenes": ["Main"] })
        }
        fn obs_put_config(&self, c: ObsConfigPut) -> Result<(), BackendError> {
            if c.port == 0 {
                return Err(BackendError::Invalid("bad port".into()));
            }
            self.calls.lock().unwrap().push(format!("obs-cfg:{}:{}:{:?}", c.enabled, c.port, c.password));
            Ok(())
        }
        fn obs_enable_server(&self) -> Result<serde_json::Value, BackendError> {
            self.calls.lock().unwrap().push("obs-enable".into());
            if self.calls.lock().unwrap().len() > 3 {
                return Err(BackendError::Conflict("OBS Studio has not been run".into()));
            }
            Ok(serde_json::json!({ "detected": { "server_enabled": true } }))
        }
        fn obs_reconnect(&self) {
            self.calls.lock().unwrap().push("obs-reconnect".into());
        }
        fn host_info(&self) -> HostInfo {
            HostInfo {
                version: "1.2.3".into(),
                os: "linux".into(),
                ui_port: PORT,
                keyboard_access: Access::Missing,
                ..Default::default()
            }
        }
        fn run_fix(&self, id: &str) -> Result<(), BackendError> {
            self.calls.lock().unwrap().push(format!("fix:{id}"));
            if id == "boom" {
                return Err(BackendError::Internal("boom".into()));
            }
            Ok(())
        }
        fn open_folder(&self, which: &str) -> Result<(), BackendError> {
            self.calls.lock().unwrap().push(format!("open:{which}"));
            Ok(())
        }
        fn plugins_list(&self) -> serde_json::Value {
            serde_json::json!({ "available": true, "plugins": [{ "id": "a.b", "name": "A" }] })
        }
        fn plugin_command(&self, id: &str, cmd: &str) -> Result<(), BackendError> {
            match id {
                "missing" => Err(BackendError::NotFound("no plugin missing".into())),
                "bundled" if cmd == "uninstall" => Err(BackendError::Invalid("bundled".into())),
                _ => {
                    self.calls.lock().unwrap().push(format!("plugin:{id}:{cmd}"));
                    Ok(())
                }
            }
        }
        fn plugins_reload(&self) {
            self.calls.lock().unwrap().push("plugins-reload".into());
        }
        fn plugin_install(&self, zip: Vec<u8>) -> Result<serde_json::Value, BackendError> {
            if zip.starts_with(b"bad") {
                return Err(BackendError::Invalid("not a zip".into()));
            }
            self.calls.lock().unwrap().push(format!("install:{}", zip.len()));
            Ok(serde_json::json!({ "id": "a.b" }))
        }
        fn plugin_log(&self, id: &str) -> Option<Vec<String>> {
            (id == "a.b").then(|| vec!["hello".to_string()])
        }
        fn plugin_file(&self, id: &str, path: &str) -> Option<Vec<u8>> {
            ((id == "a.b") && (path == "icons/x.svg" || path == "icon.png")).then(|| b"BYTES".to_vec())
        }
        fn shell_show(&self) -> Result<(), BackendError> {
            self.calls.lock().unwrap().push("show".into());
            Ok(())
        }
        fn shell_quit(&self) -> Result<(), BackendError> {
            self.calls.lock().unwrap().push("quit".into());
            Ok(())
        }
        fn phone_usb_status(&self) -> PhoneUsbStatus {
            PhoneUsbStatus {
                adb_ok: true,
                apk_available: true,
                phones: vec![UsbPhone {
                    serial: "TEST0SERIAL".into(),
                    model: "F8331".into(),
                    state: "device".into(),
                    app_installed: Some(false),
                    app_version: None,
                }],
            }
        }
        fn install_phone_app(&self, serial: &str) -> Result<(), BackendError> {
            match serial {
                "TEST0SERIAL" => {
                    self.calls.lock().unwrap().push(format!("adb-install:{serial}"));
                    Ok(())
                }
                "FAILS" => Err(BackendError::Conflict("INSTALL_FAILED_VERSION_DOWNGRADE".into())),
                _ => Err(BackendError::NotFound(format!("no phone '{serial}'"))),
            }
        }
    }

    /// Backend relying on every default trait method.
    struct Bare;
    impl ConfigBackend for Bare {
        fn list_profiles(&self) -> Vec<ProfileSummary> {
            vec![]
        }
        fn get_profile(&self, _: &str) -> Option<Profile> {
            None
        }
        fn put_profile(&self, _: Profile) -> Result<(), BackendError> {
            Ok(())
        }
        fn delete_profile(&self, _: &str) -> Result<bool, BackendError> {
            Ok(false)
        }
        fn put_icon(&self, _: Vec<u8>) -> Result<String, BackendError> {
            Ok("00".into())
        }
        fn get_icon(&self, _: &str) -> Option<Vec<u8>> {
            None
        }
        fn activate_profile(&self, _: &str) -> Result<(), BackendError> {
            Ok(())
        }
        fn status(&self) -> Status {
            Status::default()
        }
    }

    async fn call(
        backend: Arc<dyn ConfigBackend>,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<&str>,
    ) -> (StatusCode, String) {
        let app = router(backend, Some(TOKEN.into()), PORT);
        let mut b = axum::http::Request::builder().method(method).uri(uri).header("host", format!("127.0.0.1:{PORT}"));
        if let Some(t) = token {
            b = b.header("x-nd-token", t);
        }
        let req = match body {
            Some(j) => b.header("content-type", "application/json").body(Body::from(j.to_string())),
            None => b.body(Body::empty()),
        }
        .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn ok(f: &Arc<Fake>, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, String) {
        call(f.clone(), method, uri, Some(TOKEN), body).await
    }

    #[tokio::test]
    async fn new_routes_require_token() {
        let f = Arc::new(Fake::default());
        for (m, u, b) in [
            ("GET", "/api/settings", None),
            ("PUT", "/api/settings", Some("{}")),
            ("GET", "/api/host", None),
            ("POST", "/api/fix/keyboard_access", None),
            ("POST", "/api/open/logs", None),
            ("POST", "/api/shell/show", None),
            ("POST", "/api/pairing", None),
            ("DELETE", "/api/pairing", None),
            ("GET", "/api/rail", None),
            ("PUT", "/api/rail", Some(r#"{"keys":[]}"#)),
            ("GET", "/api/autoswitch", None),
            ("PUT", "/api/autoswitch", Some("{}")),
            ("POST", "/api/autoswitch/lock", Some(r#"{"locked":true}"#)),
            ("GET", "/api/apps", None),
            ("GET", "/api/apps/icon?id=x", None),
            ("GET", "/api/site-icon?url=https%3A%2F%2Fok.example%2F", None),
            ("GET", "/api/mixer", None),
            ("PUT", "/api/mixer/config", Some("{}")),
            ("POST", "/api/mixer/app", Some(r#"{"key":"spotify"}"#)),
            ("GET", "/api/obs", None),
            ("PUT", "/api/obs/config", Some(r#"{"enabled":true,"auto_detect":true,"host":"h","port":1}"#)),
            ("POST", "/api/obs/enable_server", None),
            ("POST", "/api/obs/reconnect", None),
            ("GET", "/api/plugins", None),
            ("GET", "/api/plugins/a.b/file?path=icon.png", None),
            ("GET", "/api/plugins/a.b/log", None),
            ("POST", "/api/plugins/a.b/enable", None),
            ("DELETE", "/api/plugins/a.b", None),
            ("POST", "/api/plugins/reload", None),
            ("POST", "/api/plugins/install", Some("zip")),
            ("POST", "/api/open/plugins", None),
            ("GET", "/api/devices/paired", None),
            ("DELETE", "/api/devices/paired/abc", None),
            ("GET", "/api/devices/pending", None),
            ("POST", "/api/devices/pending/s1/approve", None),
            ("POST", "/api/devices/pending/s1/deny", None),
        ] {
            let (s, _) = call(f.clone(), m, u, None, b).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "{m} {u}");
            let (s, _) = call(f.clone(), m, u, Some("wrong"), b).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "{m} {u}");
        }
        assert!(f.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn deck_sleep_defaults_clamp_and_serde() {
        let d = Settings::default().deck_sleep();
        assert_eq!(d, DeckSleepSettings { enabled: true, idle_s: 120, wake_on_camera_mic: true, show_clock: true });
        // an older settings.json without the fields reads as the defaults
        let old: Settings = serde_json::from_str(r#"{"autostart":true}"#).unwrap();
        assert_eq!(old.deck_sleep(), d);
        let low = Settings { deck_sleep_idle_s: 1, ..Default::default() };
        assert_eq!(low.deck_sleep().idle_s, 15);
        let high = Settings { deck_sleep_idle_s: 99_999, ..Default::default() };
        assert_eq!(high.deck_sleep().idle_s, 3600);
        let s = Settings {
            deck_sleep_enabled: false,
            deck_sleep_idle_s: 300,
            deck_sleep_clock: false,
            ..Default::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[tokio::test]
    async fn settings_patch_round_trip() {
        let f = Arc::new(Fake::default());
        let (s, b) = ok(&f, "GET", "/api/settings", None).await;
        assert_eq!(s, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"autostart": false, "start_minimized": false, "theme": "system", "phone_theme": "black", "lan_enabled": true,
                "camera_lens": "back", "camera_mode": "auto", "camera_bitrate_kbps": 0, "camera_mirror": "auto", "camera_auto_start": true,
                "camera_image": {"focus": "continuous", "focus_permille": 0, "exposure": "auto", "exposure_comp": 0,
                    "ae_lock": false, "iso": 0, "exposure_us": 0, "white_balance": "auto", "awb_lock": false,
                    "antibanding": "auto", "stabilization": false}, "onboarding_done": false,
                "mic_enabled": false, "mic_device": "", "mic_voice_processing": true, "mic_gain_db": 0, "mic_guard_defaults": true,
                "deck_sleep_enabled": true, "deck_sleep_idle_s": 120, "deck_sleep_wake_on_camera_mic": true, "deck_sleep_clock": true})
        );
        let (s, b) = ok(&f, "PUT", "/api/settings", Some(r#"{"autostart":true,"theme":"dark"}"#)).await;
        assert_eq!(s, StatusCode::OK);
        let got: Settings = serde_json::from_str(&b).unwrap();
        assert_eq!(got, Settings { autostart: true, start_minimized: false, theme: Theme::Dark, ..Default::default() });
        let (_, b) = ok(&f, "GET", "/api/settings", None).await;
        assert_eq!(serde_json::from_str::<Settings>(&b).unwrap(), got);
        // partial patch leaves others alone
        let (_, b) = ok(&f, "PUT", "/api/settings", Some(r#"{"start_minimized":true}"#)).await;
        let got: Settings = serde_json::from_str(&b).unwrap();
        assert!(got.autostart && got.start_minimized && got.theme == Theme::Dark);
        let (s, _) = ok(&f, "PUT", "/api/settings", Some(r#"{"theme":"purple"}"#)).await;
        assert!(s.is_client_error());
    }

    #[tokio::test]
    async fn host_info_route() {
        let f = Arc::new(Fake::default());
        let (s, b) = ok(&f, "GET", "/api/host", None).await;
        assert_eq!(s, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["version"], "1.2.3");
        assert_eq!(v["keyboard_access"], "missing");
        assert_eq!(v["adb"]["found"], false);
    }

    #[tokio::test]
    async fn fix_route() {
        let f = Arc::new(Fake::default());
        let (s, _) = ok(&f, "POST", "/api/fix/keyboard_access", None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert_eq!(f.calls.lock().unwrap().as_slice(), ["fix:keyboard_access"]);
        for bad in ["Bad", "a-b", "a.b", "x%20y", &"a".repeat(33)] {
            let (s, _) = ok(&f, "POST", &format!("/api/fix/{bad}"), None).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
        }
        assert_eq!(f.calls.lock().unwrap().len(), 1);
        let (s, _) = ok(&f, "POST", "/api/fix/boom", None).await;
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn open_route() {
        let f = Arc::new(Fake::default());
        for w in ["logs", "config"] {
            let (s, _) = ok(&f, "POST", &format!("/api/open/{w}"), None).await;
            assert_eq!(s, StatusCode::NO_CONTENT);
        }
        let (s, _) = ok(&f, "POST", "/api/open/etc", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(f.calls.lock().unwrap().as_slice(), ["open:logs", "open:config"]);
    }

    #[tokio::test]
    async fn plugin_routes() {
        let f = Arc::new(Fake::default());
        let (s, b) = ok(&f, "GET", "/api/plugins", None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(b.contains("\"available\":true"));
        let (s, b) = ok(&f, "GET", "/api/plugins/a.b/log", None).await;
        assert_eq!((s, b.as_str()), (StatusCode::OK, r#"{"lines":["hello"]}"#));
        assert_eq!(ok(&f, "GET", "/api/plugins/zzz/log", None).await.0, StatusCode::NOT_FOUND);
        for cmd in ["enable", "disable", "approve", "restart"] {
            let (s, _) = ok(&f, "POST", &format!("/api/plugins/a.b/{cmd}"), None).await;
            assert_eq!(s, StatusCode::NO_CONTENT, "{cmd}");
        }
        assert_eq!(ok(&f, "POST", "/api/plugins/a.b/uninstall", None).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(ok(&f, "POST", "/api/plugins/missing/enable", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(ok(&f, "DELETE", "/api/plugins/a.b", None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(ok(&f, "DELETE", "/api/plugins/bundled", None).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(ok(&f, "POST", "/api/plugins/reload", None).await.0, StatusCode::NO_CONTENT);
        let (s, b) = ok(&f, "POST", "/api/plugins/install", Some("PKzipbytes")).await;
        assert_eq!((s, b.as_str()), (StatusCode::OK, r#"{"id":"a.b"}"#));
        assert_eq!(ok(&f, "POST", "/api/plugins/install", Some("bad zip")).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(ok(&f, "POST", "/api/plugins/install", Some("")).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(ok(&f, "POST", "/api/open/plugins", None).await.0, StatusCode::NO_CONTENT);
        let calls = f.calls.lock().unwrap().clone();
        assert!(calls.contains(&"plugin:a.b:uninstall".to_string()) && calls.contains(&"plugins-reload".to_string()));
        assert!(calls.contains(&"install:10".to_string()) && calls.contains(&"open:plugins".to_string()));
    }

    #[tokio::test]
    async fn plugin_file_route_types_and_token_query() {
        let f = Arc::new(Fake::default());
        let get = |uri: &str| {
            let uri = uri.to_string();
            let f = f.clone();
            async move {
                let app = router(f, Some(TOKEN.into()), PORT);
                let req = axum::http::Request::builder()
                    .uri(uri)
                    .header("host", format!("127.0.0.1:{PORT}"))
                    .body(Body::empty())
                    .unwrap();
                let resp = app.oneshot(req).await.unwrap();
                let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).map(str::to_owned);
                let cc = resp.headers().contains_key("cache-control");
                (resp.status(), ct, cc)
            }
        };
        // <img> style: token in the query string.
        let (s, ct, cc) = get(&format!("/api/plugins/a.b/file?path=icons%2Fx.svg&token={TOKEN}")).await;
        assert_eq!((s, ct.as_deref(), cc), (StatusCode::OK, Some("image/svg+xml"), true));
        let (s, ct, _) = get(&format!("/api/plugins/a.b/file?path=icon.png&token={TOKEN}")).await;
        assert_eq!((s, ct.as_deref()), (StatusCode::OK, Some("image/png")));
        assert_eq!(get("/api/plugins/a.b/file?path=icon.png").await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(
            get(&format!("/api/plugins/a.b/file?path=manifest.json&token={TOKEN}")).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(get(&format!("/api/plugins/a.b/file?token={TOKEN}")).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(get(&format!("/api/plugins/a.b/file?path=nope.png&token={TOKEN}")).await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn plugin_install_has_its_own_20mb_limit() {
        let f = Arc::new(Fake::default());
        // Just over the global 4 MB limit but under 20 MB: accepted on the install route only.
        let mid = "x".repeat(5 * 1024 * 1024);
        let (s, _) = ok(&f, "POST", "/api/plugins/install", Some(&mid)).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = ok(&f, "POST", "/api/icons", Some(&mid)).await;
        assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
        let big = "x".repeat(21 * 1024 * 1024);
        let (s, _) = ok(&f, "POST", "/api/plugins/install", Some(&big)).await;
        assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn bare_backend_reports_plugins_unavailable() {
        let (s, b) = call(Arc::new(Bare), "GET", "/api/plugins", Some(TOKEN), None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(b.contains("\"available\":false"));
        let (s, _) = call(Arc::new(Bare), "POST", "/api/plugins/a.b/enable", Some(TOKEN), None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn shell_show_route() {
        let f = Arc::new(Fake::default());
        let (s, _) = ok(&f, "POST", "/api/shell/show", None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert_eq!(f.calls.lock().unwrap().as_slice(), ["show"]);
    }

    #[tokio::test]
    async fn shell_quit_route() {
        let f = Arc::new(Fake::default());
        let (s, _) = ok(&f, "POST", "/api/shell/quit", None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert_eq!(f.calls.lock().unwrap().as_slice(), ["quit"]);
        // Needs the token like every other route; a backend without quit says 404.
        let (s, _) = call(f.clone(), "POST", "/api/shell/quit", None, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _) = call(Arc::new(Bare), "POST", "/api/shell/quit", Some(TOKEN), None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn phone_usb_and_install_routes() {
        let f = Arc::new(Fake::default());
        let (s, b) = ok(&f, "GET", "/api/phone/usb", None).await;
        assert_eq!(s, StatusCode::OK);
        let st: PhoneUsbStatus = serde_json::from_str(&b).unwrap();
        assert!(st.adb_ok && st.apk_available && st.phones[0].serial == "TEST0SERIAL");
        let post = |serial: &str| format!(r#"{{"serial":"{serial}"}}"#);
        let (s, _) = ok(&f, "POST", "/api/phone/install", Some(&post("TEST0SERIAL"))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert_eq!(f.calls.lock().unwrap().as_slice(), ["adb-install:TEST0SERIAL"]);
        let (s, _) = ok(&f, "POST", "/api/phone/install", Some(&post("GONE"))).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, b) = ok(&f, "POST", "/api/phone/install", Some(&post("FAILS"))).await;
        assert_eq!(s, StatusCode::CONFLICT);
        assert!(b.contains("INSTALL_FAILED_VERSION_DOWNGRADE"));
        for bad in ["", "a b", "x;rm -rf", "../x", &"a".repeat(65)] {
            let (s, _) = ok(&f, "POST", "/api/phone/install", Some(&post(bad))).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "serial {bad:?}");
        }
        // The default backend lists nothing and cannot install.
        let (s, b) = call(Arc::new(Bare), "GET", "/api/phone/usb", Some(TOKEN), None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(b.contains("\"phones\":[]"));
    }

    #[tokio::test]
    async fn pairing_routes() {
        let f = Arc::new(Fake::default());
        let (s, b) = ok(&f, "POST", "/api/pairing", None).await;
        assert_eq!(s, StatusCode::OK);
        let t: PairingTicket = serde_json::from_str(&b).unwrap();
        assert_eq!(t.port, 47802);
        assert!(t.qr_svg.starts_with("<svg"));
        let (s, _) = ok(&f, "DELETE", "/api/pairing", None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        // LAN off -> 409
        ok(&f, "PUT", "/api/settings", Some(r#"{"lan_enabled":false}"#)).await;
        let (s, _) = ok(&f, "POST", "/api/pairing", None).await;
        assert_eq!(s, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn pending_and_paired_routes() {
        let f = Arc::new(Fake::default());
        for id in ["s1", "s2"] {
            f.pending.lock().unwrap().push(PendingDevice {
                session_id: id.into(),
                name: format!("Phone {id}"),
                ..Default::default()
            });
        }
        let (_, b) = ok(&f, "GET", "/api/devices/pending", None).await;
        assert_eq!(serde_json::from_str::<Vec<PendingDevice>>(&b).unwrap().len(), 2);
        let (s, _) = ok(&f, "POST", "/api/devices/pending/s1/approve", None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _) = ok(&f, "POST", "/api/devices/pending/s2/deny", None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _) = ok(&f, "POST", "/api/devices/pending/s2/deny", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = ok(&f, "POST", "/api/devices/pending/bad%20id/deny", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (_, b) = ok(&f, "GET", "/api/devices/paired", None).await;
        let paired: Vec<PairedDevice> = serde_json::from_str(&b).unwrap();
        assert_eq!(paired.len(), 1);
        let (s, _) = ok(&f, "DELETE", "/api/devices/paired/s1", None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _) = ok(&f, "DELETE", "/api/devices/paired/s1", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[test]
    fn qr_svg_is_standalone_and_scannable() {
        let svg = qr_svg("notideck://pair?v=1&h=abc");
        assert!(svg.starts_with("<svg xmlns=") && svg.ends_with("</svg>"));
        assert!(svg.contains("crispEdges") && svg.contains("fill=\"#fff\"") && svg.contains("<path"));
        // quiet zone: no dark module starts before offset 4
        assert!(!svg.contains("M0 ") && !svg.contains("M1 ") && !svg.contains("M3 "));
        assert!(qr_svg(&"x".repeat(10_000)).is_empty());
    }

    #[tokio::test]
    async fn autoswitch_routes() {
        let f = Arc::new(Fake::default());
        let (s, body) = ok(&f, "GET", "/api/autoswitch", None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("\"backend\":\"fake\""));
        let good = r#"{"enabled":true,"rules":[{"id":"r1","kind":"process","pattern":"obs","profile_id":"p"}]}"#;
        assert_eq!(ok(&f, "PUT", "/api/autoswitch", Some(good)).await.0, StatusCode::NO_CONTENT);
        let bad = r#"{"enabled":true,"rules":[{"id":"r1","kind":"process","pattern":"","profile_id":"p"}]}"#;
        let (s, body) = ok(&f, "PUT", "/api/autoswitch", Some(bad)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["errors"][0]["rule_id"], "r1");
        assert_eq!(ok(&f, "POST", "/api/autoswitch/lock", Some(r#"{"locked":true}"#)).await.0, StatusCode::NO_CONTENT);
        assert_eq!(*f.calls.lock().unwrap(), ["as-put:1", "as-lock:true"]);
    }

    #[tokio::test]
    async fn site_icon_route() {
        let f = Arc::new(Fake::default());
        assert_eq!(ok(&f, "GET", "/api/site-icon?url=https%3A%2F%2Fok.example%2F", None).await.0, StatusCode::OK);
        assert_eq!(
            ok(&f, "GET", "/api/site-icon?url=https%3A%2F%2Fnone.example%2F", None).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(ok(&f, "GET", "/api/site-icon?url=ftp%3A%2F%2Fx", None).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(ok(&f, "GET", "/api/site-icon", None).await.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn apps_routes() {
        let f = Arc::new(Fake::default());
        let (s, body) = ok(&f, "GET", "/api/apps", None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("\"backend\":\"fake\""));
        assert_eq!(ok(&f, "GET", "/api/apps?refresh=1", None).await.0, StatusCode::OK);
        assert_eq!(*f.calls.lock().unwrap(), ["apps:false", "apps:true"]);
        assert_eq!(ok(&f, "GET", "/api/apps/icon?id=a%20b", None).await.0, StatusCode::OK);
        assert_eq!(ok(&f, "GET", "/api/apps/icon?id=zzz", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(ok(&f, "GET", "/api/apps/icon", None).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(call(f.clone(), "GET", "/api/apps/icon?id=a%20b&token=sekrit", None, None).await.0, StatusCode::OK);
        assert_eq!(
            call(f.clone(), "GET", "/api/apps/icon?id=a%20b&token=bad", None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn mixer_routes() {
        let f = Arc::new(Fake::default());
        let (s, body) = ok(&f, "GET", "/api/mixer", None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("\"backend\":\"fake\""));
        let cfg = r#"{"order":["a"],"hidden":["x","y"],"show_system":true}"#;
        assert_eq!(ok(&f, "PUT", "/api/mixer/config", Some(cfg)).await.0, StatusCode::NO_CONTENT);
        assert_eq!(ok(&f, "PUT", "/api/mixer/config", Some("{}")).await.0, StatusCode::NO_CONTENT);
        let app = r#"{"key":"spotify","volume":0.5}"#;
        assert_eq!(ok(&f, "POST", "/api/mixer/app", Some(app)).await.0, StatusCode::NO_CONTENT);
        let gone = r#"{"key":"nope","muted":true}"#;
        assert_eq!(ok(&f, "POST", "/api/mixer/app", Some(gone)).await.0, StatusCode::NOT_FOUND);
        assert_eq!(*f.calls.lock().unwrap(), ["mx-cfg:x,y", "mx-cfg:", "mx-app:spotify:Some(0.5):None"]);
    }

    #[tokio::test]
    async fn obs_routes() {
        let f = Arc::new(Fake::default());
        let (s, body) = ok(&f, "GET", "/api/obs", None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("\"connected\""));
        let keep = r#"{"enabled":true,"auto_detect":false,"host":"127.0.0.1","port":4455}"#;
        assert_eq!(ok(&f, "PUT", "/api/obs/config", Some(keep)).await.0, StatusCode::NO_CONTENT);
        let clear = r#"{"enabled":false,"auto_detect":true,"host":"","port":1,"password":""}"#;
        assert_eq!(ok(&f, "PUT", "/api/obs/config", Some(clear)).await.0, StatusCode::NO_CONTENT);
        let bad = r#"{"enabled":true,"auto_detect":true,"host":"x","port":0}"#;
        assert_eq!(ok(&f, "PUT", "/api/obs/config", Some(bad)).await.0, StatusCode::BAD_REQUEST);
        let (s, body) = ok(&f, "POST", "/api/obs/enable_server", None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("server_enabled"));
        assert_eq!(ok(&f, "POST", "/api/obs/reconnect", None).await.0, StatusCode::NO_CONTENT);
        // The fake refuses the second enable (more than 3 calls recorded) with a 409.
        let (s, body) = ok(&f, "POST", "/api/obs/enable_server", None).await;
        assert_eq!(s, StatusCode::CONFLICT);
        assert!(body.contains("has not been run"));
        assert_eq!(
            *f.calls.lock().unwrap(),
            ["obs-cfg:true:4455:None", "obs-cfg:false:1:Some(\"\")", "obs-enable", "obs-reconnect", "obs-enable"]
        );
    }

    #[tokio::test]
    async fn rail_routes() {
        let f = Arc::new(Fake::default());
        let (s, body) = ok(&f, "GET", "/api/rail", None).await;
        assert_eq!((s, body.as_str()), (StatusCode::OK, r#"{"keys":[]}"#));
        let key = |id: &str, kind: &str| {
            format!(r#"{{"id":"{id}","kind":"{kind}","col":0,"row":0,"span_w":1,"span_h":1,"title":"T"}}"#)
        };
        let good = format!(r#"{{"keys":[{}]}}"#, key("a", "key"));
        assert_eq!(ok(&f, "PUT", "/api/rail", Some(&good)).await.0, StatusCode::NO_CONTENT);
        let (_, body) = ok(&f, "GET", "/api/rail", None).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["keys"][0]["id"], "a");
        let bad = format!(r#"{{"keys":[{}]}}"#, key("a", "tile"));
        let (s, body) = ok(&f, "PUT", "/api/rail", Some(&bad)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["errors"][0].as_str().unwrap().contains("must be a Key"));
        assert_eq!(*f.calls.lock().unwrap(), ["rail-put"]);
    }

    #[tokio::test]
    async fn default_trait_impls_give_documented_errors() {
        let b: Arc<dyn ConfigBackend> = Arc::new(Bare);
        let c = |m, u, body| call(b.clone(), m, u, Some(TOKEN), body);
        assert_eq!(c("POST", "/api/shell/show", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("POST", "/api/fix/foo", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("POST", "/api/open/logs", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("PUT", "/api/settings", Some("{}")).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(c("GET", "/api/settings", None).await.0, StatusCode::OK);
        assert_eq!(c("GET", "/api/host", None).await.0, StatusCode::OK);
        assert_eq!(c("POST", "/api/pairing", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("DELETE", "/api/pairing", None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(c("GET", "/api/devices/paired", None).await.0, StatusCode::OK);
        assert_eq!(c("GET", "/api/devices/pending", None).await.0, StatusCode::OK);
        assert_eq!(c("DELETE", "/api/devices/paired/x", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("POST", "/api/devices/pending/x/approve", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("GET", "/api/camera", None).await.0, StatusCode::OK);
        assert_eq!(c("POST", "/api/camera/test", Some(r#"{"on":true}"#)).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("POST", "/api/camera/control", Some(r#"{"torch":true}"#)).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("GET", "/api/camera/preview.png", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("GET", "/api/mic", None).await.0, StatusCode::OK);
        assert_eq!(c("POST", "/api/mic", Some(r#"{"on":true}"#)).await.0, StatusCode::NOT_FOUND);
        let (s, body) = c("GET", "/api/autoswitch", None).await;
        assert!(s == StatusCode::OK && body.contains("\"available\":false"));
        assert_eq!(c("PUT", "/api/autoswitch", Some(r#"{"rules":[]}"#)).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("POST", "/api/autoswitch/lock", Some(r#"{"locked":true}"#)).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("GET", "/api/rail", None).await.0, StatusCode::OK);
        assert_eq!(c("PUT", "/api/rail", Some(r#"{"keys":[]}"#)).await.0, StatusCode::NOT_FOUND);
        let (s, body) = c("GET", "/api/mixer", None).await;
        assert!(s == StatusCode::OK && body.contains("\"available\":false"));
        assert_eq!(c("PUT", "/api/mixer/config", Some("{}")).await.0, StatusCode::NOT_FOUND);
        assert_eq!(c("POST", "/api/mixer/app", Some(r#"{"key":"a"}"#)).await.0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn camera_mode_parsing() {
        assert_eq!(parse_camera_mode("1280x720@30"), Some((1280, 720, 30)));
        for bad in ["auto", "1280x720", "0x720@30", "1280x720@0", "axb@c", "1280x720@999", ""] {
            assert_eq!(parse_camera_mode(bad), None, "{bad}");
        }
        assert!(valid_camera_mode("auto") && valid_camera_mode("640x360@30") && !valid_camera_mode("big"));
    }

    #[test]
    fn camera_settings_defaults_and_old_files() {
        let d = Settings::default();
        assert!(d.camera_auto_start && d.camera_mode == "auto" && d.camera_bitrate_kbps == 0);
        // A settings.json from before the webcam existed still loads.
        let old: Settings = serde_json::from_str(r#"{"autostart":true}"#).unwrap();
        assert!(old.autostart && old.camera_auto_start && old.camera_lens == CameraLens::Back);
        assert_eq!(CameraLens::Back.toggled(), CameraLens::Front);
    }
}
