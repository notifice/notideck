// SPDX-License-Identifier: GPL-3.0-or-later
//! OS abstraction layer (SPEC §3.2, §6.5).
//!
//! The traits and value types in this file are the **contract** between
//! `nd-core` (pure logic) and the per-OS backends. Changing them affects
//! every crate; record such changes in SPEC §14.
//!
//! Backends live in `linux` / `windows` modules behind the `native` feature
//! and are constructed through [`native::input`] / [`native::launcher`].

use serde::{Deserialize, Serialize};

#[cfg(feature = "native")]
pub mod native;

pub mod mic;
pub use mic::{
    is_virtual_cable_name, AudioEndpoint, DefaultDevices, DeviceFlow, DeviceRole, MicOutputDevice, MicSink, MicStream,
    NullMicSink,
};

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("not supported on this platform: {0}")]
    Unsupported(&'static str),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, PlatformError>;

/// Logical key, independent of OS keycodes. Serialized in snake_case
/// (e.g. `"ctrl"`, `"f13"`, `"media_play_pause"`) inside profile JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    // modifiers
    Ctrl,
    Shift,
    Alt,
    Meta,
    // letters
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    // digits (top row)
    #[serde(rename = "0")]
    Num0,
    #[serde(rename = "1")]
    Num1,
    #[serde(rename = "2")]
    Num2,
    #[serde(rename = "3")]
    Num3,
    #[serde(rename = "4")]
    Num4,
    #[serde(rename = "5")]
    Num5,
    #[serde(rename = "6")]
    Num6,
    #[serde(rename = "7")]
    Num7,
    #[serde(rename = "8")]
    Num8,
    #[serde(rename = "9")]
    Num9,
    // function keys
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    // editing / navigation
    Enter,
    Escape,
    Backspace,
    Tab,
    Space,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    PrintScreen,
    CapsLock,
    // punctuation (US layout positions)
    Minus,
    Equal,
    LeftBracket,
    RightBracket,
    Backslash,
    Semicolon,
    Apostrophe,
    Grave,
    Comma,
    Period,
    Slash,
    // media
    MediaPlayPause,
    MediaNext,
    MediaPrev,
    MediaStop,
    VolumeUp,
    VolumeDown,
    VolumeMute,
}

impl Key {
    pub fn is_modifier(self) -> bool {
        matches!(self, Key::Ctrl | Key::Shift | Key::Alt | Key::Meta)
    }
}

/// A chord: hold `modifiers` in order, tap `key`, release in reverse order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyCombo {
    #[serde(default)]
    pub modifiers: Vec<Key>,
    pub key: Key,
}

/// Synthesises keyboard input on the host.
pub trait InputInjector: Send + Sync {
    /// Press and release a combo (modifiers down, key down/up, modifiers up).
    fn key_combo(&self, combo: &KeyCombo) -> Result<()>;
    /// Type arbitrary Unicode text. Backends may fall back to per-char key events.
    fn type_text(&self, text: &str) -> Result<()>;
}

/// Opens things on the host.
pub trait Launcher: Send + Sync {
    /// Open a URL in the default browser (`xdg-open` / `ShellExecuteW`).
    fn open_url(&self, url: &str) -> Result<()>;
    /// Open a file/app with its default handler, or spawn an executable with args.
    /// Must not block on the child process.
    fn launch(&self, target: &str, args: &[String]) -> Result<()>;
}

// ------------------------------------------------------------------ live data (M4)

/// Which default audio endpoint an audio action or data source targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioTarget {
    /// Default output device (speakers / headphones).
    #[default]
    Output,
    /// Default input device (microphone).
    Input,
}

/// Master volume and mute of the default endpoints (SPEC §6.4 Audio, §6.5).
/// Volumes are linear `0.0..=1.0`; backends clamp out-of-range values.
pub trait AudioControl: Send + Sync {
    /// Short backend id for diagnostics, e.g. `wasapi`, `pactl`, `wpctl`, `none`.
    fn backend_name(&self) -> &'static str {
        "unknown"
    }
    fn volume(&self, target: AudioTarget) -> Result<f32>;
    fn set_volume(&self, target: AudioTarget, volume: f32) -> Result<()>;
    fn muted(&self, target: AudioTarget) -> Result<bool>;
    fn set_muted(&self, target: AudioTarget, muted: bool) -> Result<()>;
    /// Current peak level of the default endpoint, linear `0.0..=1.0` (round 12, M4 meters).
    /// Must be cheap (called ~12 times a second while a meter is visible). Default: `Unsupported`.
    fn peak(&self, _target: AudioTarget) -> Result<f32> {
        Err(PlatformError::Unsupported("audio peak meter"))
    }
}

// ------------------------------------------------------------------ per-app mixer (round 12, M4)

/// One audio session (Windows WASAPI session / PulseAudio sink-input) on the default output device.
/// Several sessions can belong to one app; `nd_core::mixer` groups them by [`app_key`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioSession {
    /// Opaque id, stable while the session exists. Windows: session instance identifier;
    /// Linux: sink-input index as a decimal string.
    pub id: String,
    pub pid: Option<u32>,
    /// Executable image name without directory, same convention as [`FocusedApp::process`]:
    /// `Spotify.exe` on Windows, `spotify` on Linux (`application.process.binary`). Empty for system sounds.
    pub process: String,
    /// Human name: session display name, else the exe's FileDescription (Windows) / `application.name`
    /// (Linux), else the process stem. `System sounds` for the system session.
    pub display_name: String,
    /// Linear `0.0..=1.0` (Linux: average of the channel volumes, clamped to 1.0).
    pub volume: f32,
    pub muted: bool,
    /// Currently playing (Windows `AudioSessionStateActive`; Linux: not corked).
    pub active: bool,
    /// Windows system-sounds session.
    pub is_system: bool,
    /// Where an icon can come from: Windows full exe path; Linux `application.icon_name`. None if unknown.
    pub icon_hint: Option<String>,
}

/// Grouping key of an app: file-name part of `process`, lowercased, trailing `.exe` removed;
/// `"system"` for the system-sounds session. `C:\x\Spotify.exe` → `spotify`, `firefox-bin` → `firefox-bin`.
pub fn app_key(process: &str, is_system: bool) -> String {
    if is_system {
        return "system".into();
    }
    let name = process.rsplit(['/', '\\']).next().unwrap_or(process).to_lowercase();
    name.strip_suffix(".exe").map(str::to_owned).unwrap_or(name)
}

/// Per-app volume control on the default output device (SPEC §6.4 Audio, per-app mixer).
/// Implementations may be called from several threads; COM backends marshal to their own thread.
pub trait AppAudio: Send + Sync {
    /// Short backend id for diagnostics: `wasapi`, `pactl`, `none`.
    fn backend_name(&self) -> &'static str;
    /// All non-expired sessions of the default render device, in backend order.
    fn sessions(&self) -> Result<Vec<AudioSession>>;
    /// Unknown ids are an error (`Other`); the caller re-lists and retries at most once.
    fn set_session_volume(&self, id: &str, volume: f32) -> Result<()>;
    fn set_session_muted(&self, id: &str, muted: bool) -> Result<()>;
    /// Whether [`AppAudio::peaks`] returns real data. Default false.
    fn has_peaks(&self) -> bool {
        false
    }
    /// Peak level per id (same order), linear `0.0..=1.0`; `None` for unknown ids. Must be cheap: no
    /// re-enumeration per call (called ~12 times a second). Default: `Unsupported`.
    fn peaks(&self, _ids: &[String]) -> Result<Vec<Option<f32>>> {
        Err(PlatformError::Unsupported("session peak meters"))
    }
    /// App icon as PNG bytes (square, 64×64 preferred) for a session, or `Ok(None)`. May be slow (shell
    /// calls); the caller caches per app key. Default: `Ok(None)`.
    fn app_icon(&self, _session: &AudioSession) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

/// Fallback when no per-app backend works: `sessions` is `Unsupported`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullAppAudio;

impl AppAudio for NullAppAudio {
    fn backend_name(&self) -> &'static str {
        "none"
    }
    fn sessions(&self) -> Result<Vec<AudioSession>> {
        Err(PlatformError::Unsupported("per-app audio"))
    }
    fn set_session_volume(&self, _: &str, _: f32) -> Result<()> {
        Err(PlatformError::Unsupported("per-app audio"))
    }
    fn set_session_muted(&self, _: &str, _: bool) -> Result<()> {
        Err(PlatformError::Unsupported("per-app audio"))
    }
}

/// What the active media player reports (MPRIS / GSMTC).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    /// Playback position as a fraction of track length, if the player reports both.
    pub progress: Option<f32>,
    /// Round 16: position and length in seconds, when known. GSMTC positions are interpolated from
    /// the player's last update while playing, so they move every second.
    #[serde(default)]
    pub position_s: Option<f64>,
    #[serde(default)]
    pub length_s: Option<f64>,
    /// Round 16: player id for diagnostics/display (GSMTC `SourceAppUserModelId`, MPRIS bus name suffix).
    #[serde(default)]
    pub app: String,
    /// Round 16: album art as PNG bytes, at most 256x256 (scaled down by the backend), or `None`.
    /// Backends cache it per track (title + artist + album), so repeated polls return the same `Arc`
    /// without re-reading or re-encoding.
    #[serde(skip)]
    pub art: Option<std::sync::Arc<[u8]>>,
    /// Round 17: shuffle on/off, `None` when the player does not report it (GSMTC `IsShuffleActive`,
    /// MPRIS `Shuffle`).
    #[serde(default)]
    pub shuffle: Option<bool>,
    /// Round 17: repeat mode, `None` when not reported (GSMTC `AutoRepeatMode`, MPRIS `LoopStatus`).
    #[serde(default)]
    pub repeat: Option<RepeatMode>,
    /// Round 17: the player accepts [`MediaCommand::Seek`] / [`MediaCommand::SeekBy`] (GSMTC
    /// `IsPlaybackPositionEnabled`, MPRIS `CanSeek`). False when unknown.
    #[serde(default)]
    pub can_seek: bool,
}

/// Round 17: player repeat mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    /// Repeat the current track (GSMTC `Track`, MPRIS `Track`).
    Track,
    /// Repeat the playlist (GSMTC `List`, MPRIS `Playlist`).
    List,
}

/// Round 17: a command for the current media session (the one [`MediaInfo::now_playing`] reports).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum MediaCommand {
    PlayPause,
    Play,
    Pause,
    Next,
    Prev,
    Stop,
    /// Absolute position in seconds from the track start (clamped to `0..=length` by the backend).
    Seek {
        position_s: f64,
    },
    /// Relative seek in seconds (negative = back).
    SeekBy {
        delta_s: f64,
    },
    Shuffle {
        on: bool,
    },
    Repeat {
        mode: RepeatMode,
    },
}

/// Now-playing info and, from round 17, control of the current media session.
/// `Ok(None)` from `now_playing` means no player is active.
pub trait MediaInfo: Send + Sync {
    /// Short backend id for diagnostics, e.g. `gsmtc`, `mpris`, `none`.
    fn backend_name(&self) -> &'static str {
        "unknown"
    }
    fn now_playing(&self) -> Result<Option<NowPlaying>>;
    /// Round 17: send `cmd` to the session `now_playing` reports (same selection rule).
    /// No active player → `Err(Other("no media player"))`. Commands the player refuses (GSMTC `Try*` returns
    /// false, MPRIS `Can*` false) → `Err(Other(..))`. Default: `Unsupported`, which makes the router fall back to
    /// media hotkeys for the transport commands.
    fn control(&self, _cmd: MediaCommand) -> Result<()> {
        Err(PlatformError::Unsupported("media session control"))
    }
}

/// Round 17: candidate mixer app keys ([`app_key`]) for a player id ([`NowPlaying::app`]), most likely first,
/// lowercased, no duplicates. Rules:
/// - MPRIS bus suffix: drop `.instance…` (`firefox.instance_1_42` → `firefox`, `chromium.instance123` → `chromium`).
/// - Packaged AUMID `Family_hash!App`: the part after `!` (`SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify` →
///   `spotify`), then the last dotted segment of the family name (`spotifymusic`).
/// - Paths / exe names go through [`app_key`] (`Spotify.exe` → `spotify`, `C:\x\vlc.exe` → `vlc`).
/// - Known aliases: Firefox's GSMTC id `308046B0AF4A39CB` (and `6F193CCC56814779` for Firefox Nightly) →
///   `firefox`; `MSEdge` → `msedge`; `Chrome` → `chrome`.
/// - Empty input → empty list.
pub fn player_app_keys(player: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |k: String| {
        if !k.is_empty() && !out.contains(&k) {
            out.push(k);
        }
    };
    let player = player.trim();
    if player.is_empty() {
        return Vec::new();
    }
    match player.to_lowercase().as_str() {
        "308046b0af4a39cb" | "6f193ccc56814779" => push("firefox".into()),
        _ => {}
    }
    if let Some((family, app)) = player.split_once('!') {
        push(app_key(app, false));
        let family = family.split('_').next().unwrap_or(family);
        push(family.rsplit('.').next().unwrap_or(family).to_lowercase());
    } else if let Some(i) = player.find(".instance") {
        push(player[..i].to_lowercase());
    } else {
        push(app_key(player, false));
    }
    out
}

/// One sample of host load. Rates are averaged since the previous call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemSnapshot {
    /// Overall CPU usage `0.0..=100.0`.
    pub cpu_percent: f32,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub net_rx_bytes_per_sec: u64,
    pub net_tx_bytes_per_sec: u64,
}

/// Host load sampling. Implementations keep state between calls (CPU / network deltas).
pub trait SystemStats: Send + Sync {
    fn snapshot(&self) -> Result<SystemSnapshot>;
}

/// Fallback used when no audio backend is available: every call is `Unsupported`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullAudio;

impl AudioControl for NullAudio {
    fn backend_name(&self) -> &'static str {
        "none"
    }
    fn volume(&self, _: AudioTarget) -> Result<f32> {
        Err(PlatformError::Unsupported("audio control"))
    }
    fn set_volume(&self, _: AudioTarget, _: f32) -> Result<()> {
        Err(PlatformError::Unsupported("audio control"))
    }
    fn muted(&self, _: AudioTarget) -> Result<bool> {
        Err(PlatformError::Unsupported("audio control"))
    }
    fn set_muted(&self, _: AudioTarget, _: bool) -> Result<()> {
        Err(PlatformError::Unsupported("audio control"))
    }
}

/// Fallback media source: never reports a player.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullMedia;

impl MediaInfo for NullMedia {
    fn backend_name(&self) -> &'static str {
        "none"
    }
    fn now_playing(&self) -> Result<Option<NowPlaying>> {
        Ok(None)
    }
}

/// Fallback stats source: every call is `Unsupported`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullStats;

impl SystemStats for NullStats {
    fn snapshot(&self) -> Result<SystemSnapshot> {
        Err(PlatformError::Unsupported("system stats"))
    }
}

// ------------------------------------------------------------------ app integration (round 4)

/// Start-at-login registration for the agent (Windows Run key, XDG autostart on Linux).
pub trait Autostart: Send + Sync {
    fn is_enabled(&self) -> Result<bool>;
    fn set_enabled(&self, enabled: bool) -> Result<()>;
}

/// Whether the agent can synthesise keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardAccess {
    /// Real injection works.
    Ok,
    /// Possible but not permitted yet (Linux: `/dev/uinput` not writable). Fixable.
    Missing,
    /// No backend on this OS.
    Unsupported,
}

// ------------------------------------------------------------------ focus tracking (round 8, M5)

/// The window that has keyboard focus (SPEC §6.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusedApp {
    /// Executable image name as the OS reports it, without directory: `obs64.exe` on Windows,
    /// `obs` on Linux (`/proc/<pid>/comm` or the exe basename). Empty when unknown.
    pub process: String,
    /// Window class: Win32 class name, X11 `WM_CLASS` class part, Wayland `app_id`. Empty when unknown.
    pub class: String,
    /// Window title. Empty when unknown.
    pub title: String,
    pub pid: Option<u32>,
}

/// Keeps a focus watch alive; dropping it stops the watch (and joins/cancels its thread).
pub struct FocusHandle(pub Box<dyn Send>);

/// Source of focused-window changes.
pub trait FocusWatcher: Send + Sync {
    /// Short backend id for diagnostics and the UI: `windows`, `x11`, `sway`, `hyprland`, `none`.
    fn backend_name(&self) -> &'static str;
    /// Starts watching. `on_change` runs on a backend thread: once with the current window right away,
    /// then on every change of the focused window or of its title. Consecutive identical values are
    /// not repeated. Errors mean the backend is unusable (the UI then shows auto-switch as unavailable).
    fn start(&self, on_change: Box<dyn Fn(FocusedApp) + Send + Sync>) -> Result<FocusHandle>;
}

/// Fallback when no backend works on this desktop: `start` is `Unsupported`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullFocus;

impl FocusWatcher for NullFocus {
    fn backend_name(&self) -> &'static str {
        "none"
    }
    fn start(&self, _: Box<dyn Fn(FocusedApp) + Send + Sync>) -> Result<FocusHandle> {
        Err(PlatformError::Unsupported("focused-window tracking"))
    }
}

// ------------------------------------------------------------------ installed apps (round 13)

/// An app the user can pick for a launch action instead of typing a path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledApp {
    /// Stable id. Windows: the AppsFolder parsing name (AppUserModelID or file path);
    /// Linux: the desktop file id (`firefox.desktop`, `org.gnome.Nautilus.desktop`).
    pub id: String,
    /// Display name (`Spotify`, `Visual Studio Code`).
    pub name: String,
    /// What `Launcher::launch` gets as `target`. Windows: `shell:AppsFolder\<id>` (works for Win32 and Store apps);
    /// Linux: the program from `Exec` (field codes like `%U` removed).
    pub target: String,
    /// Launch arguments (Windows: always empty; Linux: the rest of `Exec`, field codes removed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// Lists installed apps for the config UI's app picker.
pub trait AppCatalog: Send + Sync {
    /// Short backend id: `appsfolder`, `desktop-files`, `none`.
    fn backend_name(&self) -> &'static str;
    /// All launchable apps, sorted by name (case-insensitive), no duplicate ids. May take ~1 s; callers cache.
    fn list(&self) -> Result<Vec<InstalledApp>>;
    /// App icon as PNG (64×64 preferred) or `Ok(None)`. Callers cache per id.
    fn icon(&self, _app: &InstalledApp) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

/// Fallback: `list` is `Unsupported`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullAppCatalog;

impl AppCatalog for NullAppCatalog {
    fn backend_name(&self) -> &'static str {
        "none"
    }
    fn list(&self) -> Result<Vec<InstalledApp>> {
        Err(PlatformError::Unsupported("installed app list"))
    }
}

#[cfg(test)]
mod player_key_tests {
    use super::player_app_keys;

    #[test]
    fn player_ids_map_to_app_keys() {
        assert_eq!(player_app_keys("Spotify.exe"), ["spotify"]);
        assert_eq!(player_app_keys(r"C:\x\vlc.exe"), ["vlc"]);
        assert_eq!(player_app_keys("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), ["spotify", "spotifymusic"]);
        assert_eq!(player_app_keys("firefox.instance_1_42"), ["firefox"]);
        assert_eq!(player_app_keys("chromium.instance123"), ["chromium"]);
        assert_eq!(player_app_keys("308046B0AF4A39CB"), ["firefox", "308046b0af4a39cb"]);
        assert_eq!(player_app_keys("MSEdge"), ["msedge"]);
        assert_eq!(player_app_keys("Chrome"), ["chrome"]);
        assert_eq!(player_app_keys("spotify"), ["spotify"]);
        assert!(player_app_keys("  ").is_empty());
    }
}
