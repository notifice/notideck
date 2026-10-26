// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-app audio mixer (round 12, M4; SPEC §6.4 Audio).
//!
//! Groups the platform's audio sessions ([`nd_platform::AppAudio`]) into apps and resolves
//! [`AudioApp`] targets for actions, live data and level meters.
//!
//! # Semantics (contract)
//! - **Grouping:** sessions with the same [`nd_platform::app_key`] form one [`MixerApp`]. App volume = max of
//!   its sessions, muted = all sessions muted, active = any session active, level = max session peak.
//!   `display_name` comes from the first session with a non-empty one. Setting volume / mute applies to every
//!   session of the app. Toggling mute: if the app is muted (all sessions), unmute all; else mute all.
//! - **Visible apps:** every app except keys in `MixerConfig::hidden`, and except `system` unless
//!   `show_system`. Hidden apps are still listed (for the config UI) but get no slot.
//! - **Slot order:** visible apps whose key is in `MixerConfig::order` come first, in that order; then the rest
//!   in first-seen order (the order in which this `Mixer` first saw the key, kept for the agent's lifetime even
//!   while the app is gone, so an app that comes back keeps its place relative to the others). Slots are
//!   dense: slot `i` is the `i`-th visible app currently present.
//! - **Targets:** `Name` matches `app_key(name, false) == key` (hidden apps too); `Focused` uses the focus
//!   provider's process name through `app_key`; `Slot { index }` is the slot. Unresolved → `None` / an error.
//! - **Caching:** `refresh` lists sessions and replaces the cached snapshot. `apps` / `resolve` / the action
//!   helpers use the cached snapshot if it is younger than [`CACHE_MAX_AGE`], else they refresh first.
//!   `levels` never refreshes (it maps targets through the cached snapshot and calls `peaks` once).
//!   If a set call fails because a session vanished, refresh once and retry.
//! - **Icons:** `icon_hash` asks the backend for an app's icon once per key (positive and negative results
//!   are cached for the agent's lifetime), imports PNG bytes through the icon importer, returns the hash.
//! - **Availability:** `available()` is true after the latest `sessions()` call succeeded; `last_error()`
//!   holds the latest failure text.

use crate::error::{CoreError, Result};
use crate::model::AudioApp;
use nd_platform::{app_key, player_app_keys, AppAudio, AudioControl, AudioSession, AudioTarget};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Snapshot age after which [`Mixer::apps`] / [`Mixer::resolve`] / actions re-list sessions.
pub const CACHE_MAX_AGE: Duration = Duration::from_millis(500);

/// User settings of the mixer, stored by the agent in `mixer.json` in the config dir.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixerConfig {
    /// App keys pinned to the first slots, in this order.
    pub order: Vec<String>,
    /// App keys that never get a slot.
    pub hidden: Vec<String>,
    /// Give the Windows system-sounds session (`system`) a slot.
    pub show_system: bool,
}

/// One app in the mixer (sessions grouped by app key).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MixerApp {
    pub key: String,
    pub display_name: String,
    #[serde(skip)]
    pub session_ids: Vec<String>,
    pub volume: f32,
    pub muted: bool,
    pub active: bool,
    pub is_system: bool,
    pub hidden: bool,
    /// Slot index; `None` when hidden.
    pub slot: Option<u32>,
    /// From the first session that has one; used for icon lookup.
    #[serde(skip)]
    pub icon_hint: Option<String>,
    #[serde(skip)]
    pub pid: Option<u32>,
}

/// Process name (as [`nd_platform::FocusedApp::process`]) of the focused window, if known.
pub type FocusProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;
/// Round 17: player id (`nd_platform::NowPlaying::app`) of the current media session, `None` when no player.
pub type MediaAppProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;
/// Round 17: how long a [`MediaAppProvider`] result is reused.
pub const MEDIA_APP_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(1);
/// Imports icon bytes (PNG) into the icon store and returns the hash.
pub type IconImporter = Arc<dyn Fn(&[u8]) -> Option<String> + Send + Sync>;

/// Clock used for the snapshot age (injectable for tests).
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Default)]
struct Inner {
    config: MixerConfig,
    /// Raw sessions of the latest successful listing.
    sessions: Vec<AudioSession>,
    /// Grouped apps: visible in slot order, then hidden by display name.
    apps: Vec<MixerApp>,
    fetched: Option<Instant>,
    available: bool,
    last_error: Option<String>,
    /// App keys in the order this mixer first saw them.
    seen: Vec<String>,
    icons: HashMap<String, Option<String>>,
}

impl Inner {
    /// Regroup `sessions` into `apps` using `config` and `seen`.
    fn rebuild(&mut self) {
        let mut groups: Vec<(String, Vec<&AudioSession>)> = Vec::new();
        for s in &self.sessions {
            let key = app_key(&s.process, s.is_system);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => v.push(s),
                None => groups.push((key, vec![s])),
            }
        }
        for (k, _) in &groups {
            if !self.seen.contains(k) {
                self.seen.push(k.clone());
            }
        }
        let mut apps: Vec<MixerApp> = groups
            .into_iter()
            .map(|(key, ss)| {
                let is_system = key == "system";
                MixerApp {
                    display_name: ss
                        .iter()
                        .map(|s| s.display_name.as_str())
                        .find(|n| !n.is_empty())
                        .unwrap_or("")
                        .to_string(),
                    session_ids: ss.iter().map(|s| s.id.clone()).collect(),
                    volume: ss.iter().map(|s| s.volume).fold(0.0, f32::max),
                    muted: ss.iter().all(|s| s.muted),
                    active: ss.iter().any(|s| s.active),
                    is_system,
                    hidden: self.config.hidden.contains(&key) || (is_system && !self.config.show_system),
                    slot: None,
                    icon_hint: ss.iter().find_map(|s| s.icon_hint.clone()),
                    pid: ss.iter().find_map(|s| s.pid),
                    key,
                }
            })
            .collect();
        let seen_pos = |k: &str| self.seen.iter().position(|s| s == k).unwrap_or(usize::MAX);
        let order_pos = |k: &str| self.config.order.iter().position(|s| s == k);
        let (mut visible, mut hidden): (Vec<_>, Vec<_>) = apps.drain(..).partition(|a| !a.hidden);
        visible.sort_by_key(|a| match order_pos(&a.key) {
            Some(p) => (0, p),
            None => (1, seen_pos(&a.key)),
        });
        for (i, a) in visible.iter_mut().enumerate() {
            a.slot = Some(i as u32);
        }
        hidden.sort_by_key(|a| a.display_name.to_lowercase());
        visible.extend(hidden);
        self.apps = visible;
    }
}

/// Mixer state shared by the router, the live sampler and the config API.
pub struct Mixer {
    backend: Arc<dyn AppAudio>,
    endpoint: Arc<dyn AudioControl>,
    focus: Option<FocusProvider>,
    /// Round 17: player id of the current media session (`NowPlaying::app`) for `AudioApp::Media`.
    media_app: Option<MediaAppProvider>,
    /// Latest provider result and when it was taken.
    media_cache: Mutex<Option<(Instant, Option<String>)>>,
    import: Option<IconImporter>,
    clock: Clock,
    inner: Mutex<Inner>,
}

fn describe(target: &AudioApp) -> String {
    match target {
        AudioApp::Name { name } => name.clone(),
        AudioApp::Focused => "the focused app".to_string(),
        AudioApp::Slot { index } => format!("mixer slot {}", index + 1),
        AudioApp::Media => "the media player".to_string(),
    }
}

fn no_audio(target: &AudioApp) -> CoreError {
    CoreError::UnsupportedAction(format!("no audio from {}", describe(target)))
}

impl Mixer {
    /// `backend` for app sessions, `endpoint` for master-volume peaks. No focus provider (Focused never
    /// resolves) and no icon importer (no icons) until set.
    pub fn new(backend: Arc<dyn AppAudio>, endpoint: Arc<dyn AudioControl>) -> Self {
        Self {
            backend,
            endpoint,
            focus: None,
            media_app: None,
            media_cache: Mutex::new(None),
            import: None,
            clock: Arc::new(Instant::now),
            inner: Mutex::new(Inner::default()),
        }
    }

    pub fn with_focus(mut self, focus: FocusProvider) -> Self {
        self.focus = Some(focus);
        self
    }

    /// Round 17: source of the media player id for `AudioApp::Media`. The mixer calls it at most once per
    /// [`MEDIA_APP_MAX_AGE`] (cached with its clock), so meter ticks do not poll the player.
    pub fn with_media_app(mut self, media_app: MediaAppProvider) -> Self {
        self.media_app = Some(media_app);
        self
    }

    pub fn with_icons(mut self, import: IconImporter) -> Self {
        self.import = Some(import);
        self
    }

    /// Replace the clock used for the cache age (tests).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.backend_name()
    }

    pub fn available(&self) -> bool {
        self.lock().available
    }

    pub fn last_error(&self) -> Option<String> {
        self.lock().last_error.clone()
    }

    /// Whether level meters work for apps (`AppAudio::has_peaks`).
    pub fn has_app_peaks(&self) -> bool {
        self.backend.has_peaks()
    }

    pub fn config(&self) -> MixerConfig {
        self.lock().config.clone()
    }

    /// Replace the config; slots are recomputed from the cached snapshot right away.
    pub fn set_config(&self, config: MixerConfig) {
        let mut g = self.lock();
        g.config = config;
        g.rebuild();
    }

    /// List sessions now, regroup, update the first-seen order and the cache. Returns every app:
    /// visible ones in slot order, then hidden ones by display name.
    pub fn refresh(&self) -> Result<Vec<MixerApp>> {
        let listed = self.backend.sessions();
        let now = (self.clock)();
        let mut g = self.lock();
        match listed {
            Ok(sessions) => {
                g.sessions = sessions;
                g.available = true;
                g.last_error = None;
                g.fetched = Some(now);
                g.rebuild();
                Ok(g.apps.clone())
            }
            Err(e) => {
                g.available = false;
                g.last_error = Some(e.to_string());
                g.sessions.clear();
                g.apps.clear();
                g.fetched = None;
                Err(e.into())
            }
        }
    }

    fn fresh(&self) -> bool {
        let fetched = self.lock().fetched;
        fetched.is_some_and(|t| (self.clock)().saturating_duration_since(t) < CACHE_MAX_AGE)
    }

    /// Like [`Mixer::refresh`] but served from the cache when it is fresh. Empty when unavailable.
    pub fn apps(&self) -> Vec<MixerApp> {
        if !self.fresh() {
            let _ = self.refresh();
        }
        self.lock().apps.clone()
    }

    fn find(&self, target: &AudioApp, apps: &[MixerApp]) -> Option<MixerApp> {
        match target {
            AudioApp::Name { name } => {
                let key = app_key(name, false);
                apps.iter().find(|a| a.key == key).cloned()
            }
            AudioApp::Focused => {
                let process = self.focus.as_ref()?()?;
                let key = app_key(&process, false);
                apps.iter().find(|a| a.key == key).cloned()
            }
            AudioApp::Slot { index } => apps.iter().find(|a| a.slot == Some(*index)).cloned(),
            AudioApp::Media => {
                let player = self.media_player()?;
                for cand in player_app_keys(&player) {
                    if let Some(a) = apps.iter().find(|a| a.key == cand || a.display_name.to_lowercase() == cand) {
                        return Some(a.clone());
                    }
                }
                let mut active = apps.iter().filter(|a| a.active && !a.is_system);
                match (active.next(), active.next()) {
                    (Some(a), None) => Some(a.clone()),
                    _ => None,
                }
            }
        }
    }

    /// Player id from the media provider, cached for [`MEDIA_APP_MAX_AGE`].
    fn media_player(&self) -> Option<String> {
        let provider = self.media_app.as_ref()?;
        let now = (self.clock)();
        {
            let g = self.media_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((t, v)) = g.as_ref() {
                if now.saturating_duration_since(*t) < MEDIA_APP_MAX_AGE {
                    return v.clone();
                }
            }
        }
        let v = provider().filter(|s| !s.is_empty());
        *self.media_cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((now, v.clone()));
        v
    }

    pub fn resolve(&self, target: &AudioApp) -> Option<MixerApp> {
        let apps = self.apps();
        self.find(target, &apps)
    }

    /// Resolve, run `f` on the app, and on failure refresh once and retry.
    fn apply<R>(&self, target: &AudioApp, f: impl Fn(&MixerApp) -> Result<R>) -> Result<R> {
        let app = self.resolve(target).ok_or_else(|| no_audio(target))?;
        match f(&app) {
            Ok(r) => Ok(r),
            Err(first) => {
                if self.refresh().is_err() {
                    return Err(first);
                }
                let app = self.resolve(target).ok_or_else(|| no_audio(target))?;
                f(&app)
            }
        }
    }

    fn set_all_volume(&self, app: &MixerApp, volume: f32) -> Result<()> {
        for id in &app.session_ids {
            self.backend.set_session_volume(id, volume)?;
        }
        self.write_through(app, Some(volume), None);
        Ok(())
    }

    fn set_all_muted(&self, app: &MixerApp, muted: bool) -> Result<()> {
        for id in &app.session_ids {
            self.backend.set_session_muted(id, muted)?;
        }
        self.write_through(app, None, Some(muted));
        Ok(())
    }

    /// Record a successful set in the cached snapshot, so rapid follow-ups (dial detents, double taps)
    /// within [`CACHE_MAX_AGE`] build on the new value instead of the stale one.
    fn write_through(&self, app: &MixerApp, volume: Option<f32>, muted: Option<bool>) {
        let mut g = self.lock();
        for s in g.sessions.iter_mut().filter(|s| app.session_ids.contains(&s.id)) {
            if let Some(v) = volume {
                s.volume = v;
            }
            if let Some(m) = muted {
                s.muted = m;
            }
        }
        g.rebuild();
    }

    /// Volume += `delta` (clamped). Returns the new app volume. Error if the target doesn't resolve.
    pub fn adjust(&self, target: &AudioApp, delta: f32) -> Result<f32> {
        self.apply(target, |app| {
            let v = (app.volume + delta).clamp(0.0, 1.0);
            self.set_all_volume(app, v)?;
            Ok(v)
        })
    }

    /// Returns the new (clamped) app volume.
    pub fn set_volume(&self, target: &AudioApp, volume: f32) -> Result<f32> {
        let v = volume.clamp(0.0, 1.0);
        self.apply(target, |app| {
            self.set_all_volume(app, v)?;
            Ok(v)
        })
    }

    /// Returns the new muted state.
    pub fn toggle_mute(&self, target: &AudioApp) -> Result<bool> {
        self.apply(target, |app| {
            let m = !app.muted;
            self.set_all_muted(app, m)?;
            Ok(m)
        })
    }

    /// Config UI: set volume and/or mute of the app with this key.
    pub fn set_app(&self, key: &str, volume: Option<f32>, muted: Option<bool>) -> Result<()> {
        let target = AudioApp::Name { name: key.to_string() };
        self.apply(&target, |app| {
            if let Some(v) = volume {
                self.set_all_volume(app, v.clamp(0.0, 1.0))?;
            }
            if let Some(m) = muted {
                self.set_all_muted(app, m)?;
            }
            Ok(())
        })
    }

    /// Icon hash for an app (see module doc), or `None`.
    pub fn icon_hash(&self, app: &MixerApp) -> Option<String> {
        if app.is_system {
            return None;
        }
        let import = self.import.as_ref()?;
        let session = {
            let g = self.lock();
            if let Some(cached) = g.icons.get(&app.key) {
                return cached.clone();
            }
            g.sessions.iter().find(|s| app_key(&s.process, s.is_system) == app.key).cloned().unwrap_or_else(|| {
                AudioSession {
                    id: app.session_ids.first().cloned().unwrap_or_default(),
                    pid: app.pid,
                    process: app.key.clone(),
                    display_name: app.display_name.clone(),
                    icon_hint: app.icon_hint.clone(),
                    ..Default::default()
                }
            })
        };
        let hash = match self.backend.app_icon(&session) {
            Ok(Some(png)) => import(&png),
            _ => None,
        };
        self.lock().icons.insert(app.key.clone(), hash.clone());
        hash
    }

    /// Current level per target (same order); `None` when unresolved or peaks are unsupported.
    pub fn levels(&self, targets: &[AudioApp]) -> Vec<Option<f32>> {
        if !self.backend.has_peaks() {
            return vec![None; targets.len()];
        }
        let apps = self.lock().apps.clone();
        let resolved: Vec<Option<MixerApp>> = targets.iter().map(|t| self.find(t, &apps)).collect();
        let mut ids: Vec<String> = Vec::new();
        for a in resolved.iter().flatten() {
            for id in &a.session_ids {
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
        if ids.is_empty() {
            return vec![None; targets.len()];
        }
        let peaks = self.backend.peaks(&ids).unwrap_or_default();
        let peak_of = |id: &String| ids.iter().position(|i| i == id).and_then(|p| peaks.get(p).copied().flatten());
        resolved
            .iter()
            .map(|a| {
                a.as_ref().and_then(|a| {
                    a.session_ids
                        .iter()
                        .filter_map(peak_of)
                        .fold(None, |m: Option<f32>, p| Some(m.map_or(p, |m| m.max(p))))
                })
            })
            .collect()
    }

    /// Default-endpoint peak, `None` if unsupported.
    pub fn endpoint_level(&self, target: AudioTarget) -> Option<f32> {
        self.endpoint.peak(target).ok()
    }
}
