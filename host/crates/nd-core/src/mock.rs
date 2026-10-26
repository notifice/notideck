// SPDX-License-Identifier: GPL-3.0-or-later
//! Mock implementations of platform traits for unit and integration testing.

use nd_platform::{InputInjector, KeyCombo, Launcher, PlatformError, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A thread-safe mock input injector that records all calls.
#[derive(Debug, Default, Clone)]
pub struct MockInputInjector {
    combos: Arc<Mutex<Vec<KeyCombo>>>,
    typed: Arc<Mutex<Vec<String>>>,
    fail: Arc<AtomicBool>,
}

impl MockInputInjector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether calls should return an error.
    pub fn set_failing(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    /// List of all key combos injected.
    pub fn key_combos(&self) -> Vec<KeyCombo> {
        self.combos.lock().unwrap().clone()
    }

    /// List of all text strings typed.
    pub fn typed_texts(&self) -> Vec<String> {
        self.typed.lock().unwrap().clone()
    }

    /// The most recent key combo injected, if any.
    pub fn last_combo(&self) -> Option<KeyCombo> {
        self.combos.lock().unwrap().last().cloned()
    }

    /// The most recent text string typed, if any.
    pub fn last_text(&self) -> Option<String> {
        self.typed.lock().unwrap().last().cloned()
    }

    /// Clear all recorded inputs.
    pub fn clear(&self) {
        self.combos.lock().unwrap().clear();
        self.typed.lock().unwrap().clear();
    }
}

impl InputInjector for MockInputInjector {
    fn key_combo(&self, combo: &KeyCombo) -> Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(PlatformError::Other("mock input failure".to_string()));
        }
        self.combos.lock().unwrap().push(combo.clone());
        Ok(())
    }

    fn type_text(&self, text: &str) -> Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(PlatformError::Other("mock input failure".to_string()));
        }
        self.typed.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

pub type LaunchCall = (String, Vec<String>);

/// A thread-safe mock launcher that records all calls.
#[derive(Debug, Default, Clone)]
pub struct MockLauncher {
    urls: Arc<Mutex<Vec<String>>>,
    apps: Arc<Mutex<Vec<LaunchCall>>>,
    fail: Arc<AtomicBool>,
}

impl MockLauncher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether calls should return an error.
    pub fn set_failing(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    /// List of all URLs opened.
    pub fn opened_urls(&self) -> Vec<String> {
        self.urls.lock().unwrap().clone()
    }

    /// List of all apps launched `(path, args)`.
    pub fn launched_apps(&self) -> Vec<LaunchCall> {
        self.apps.lock().unwrap().clone()
    }

    /// The most recent URL opened, if any.
    pub fn last_url(&self) -> Option<String> {
        self.urls.lock().unwrap().last().cloned()
    }

    /// The most recent app launched, if any.
    pub fn last_launch(&self) -> Option<LaunchCall> {
        self.apps.lock().unwrap().last().cloned()
    }

    /// Clear all recorded launches.
    pub fn clear(&self) {
        self.urls.lock().unwrap().clear();
        self.apps.lock().unwrap().clear();
    }
}

impl Launcher for MockLauncher {
    fn open_url(&self, url: &str) -> Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(PlatformError::Other("mock launch failure".to_string()));
        }
        self.urls.lock().unwrap().push(url.to_string());
        Ok(())
    }

    fn launch(&self, target: &str, args: &[String]) -> Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(PlatformError::Other("mock launch failure".to_string()));
        }
        self.apps.lock().unwrap().push((target.to_string(), args.to_vec()));
        Ok(())
    }
}

/// A thread-safe mock audio backend storing volume and mute per target.
#[derive(Debug, Clone)]
pub struct MockAudio {
    state: Arc<Mutex<[(f32, bool); 2]>>,
    peak: Arc<Mutex<Option<f32>>>,
}

impl Default for MockAudio {
    fn default() -> Self {
        Self { state: Arc::new(Mutex::new([(0.5, false); 2])), peak: Arc::new(Mutex::new(None)) }
    }
}

impl MockAudio {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make `peak` report this level (`None`: unsupported, the default).
    pub fn set_peak(&self, peak: Option<f32>) {
        *self.peak.lock().unwrap() = peak;
    }

    fn idx(target: nd_platform::AudioTarget) -> usize {
        match target {
            nd_platform::AudioTarget::Output => 0,
            nd_platform::AudioTarget::Input => 1,
        }
    }
}

impl nd_platform::AudioControl for MockAudio {
    fn volume(&self, target: nd_platform::AudioTarget) -> Result<f32> {
        Ok(self.state.lock().unwrap()[Self::idx(target)].0)
    }

    fn set_volume(&self, target: nd_platform::AudioTarget, volume: f32) -> Result<()> {
        self.state.lock().unwrap()[Self::idx(target)].0 = volume.clamp(0.0, 1.0);
        Ok(())
    }

    fn muted(&self, target: nd_platform::AudioTarget) -> Result<bool> {
        Ok(self.state.lock().unwrap()[Self::idx(target)].1)
    }

    fn set_muted(&self, target: nd_platform::AudioTarget, muted: bool) -> Result<()> {
        self.state.lock().unwrap()[Self::idx(target)].1 = muted;
        Ok(())
    }

    fn peak(&self, _target: nd_platform::AudioTarget) -> Result<f32> {
        (*self.peak.lock().unwrap()).ok_or(PlatformError::Unsupported("audio peak meter"))
    }
}

/// One recorded `set_session_*` call of [`MockAppAudio`].
#[derive(Debug, Clone, PartialEq)]
pub enum AppAudioCall {
    Volume(String, f32),
    Muted(String, bool),
}

#[derive(Default)]
struct AppAudioState {
    sessions: Vec<nd_platform::AudioSession>,
    peaks: std::collections::HashMap<String, f32>,
    has_peaks: bool,
    icons: std::collections::HashMap<String, Vec<u8>>,
    calls: Vec<AppAudioCall>,
    list_calls: usize,
    peak_calls: Vec<Vec<String>>,
    icon_calls: usize,
    fail_list: bool,
}

/// A thread-safe mock per-app audio backend. Set calls update the stored sessions and are recorded;
/// unknown ids fail like a vanished session.
#[derive(Clone, Default)]
pub struct MockAppAudio {
    state: Arc<Mutex<AppAudioState>>,
}

impl MockAppAudio {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the session list.
    pub fn set_sessions(&self, sessions: Vec<nd_platform::AudioSession>) {
        self.state.lock().unwrap().sessions = sessions;
    }

    /// Convenience session: id, process, display name, volume.
    pub fn session(id: &str, process: &str, name: &str, volume: f32) -> nd_platform::AudioSession {
        nd_platform::AudioSession {
            id: id.into(),
            process: process.into(),
            display_name: name.into(),
            volume,
            active: true,
            ..Default::default()
        }
    }

    /// Set the peak reported for a session id; also turns `has_peaks` on.
    pub fn set_peak(&self, id: &str, peak: f32) {
        let mut s = self.state.lock().unwrap();
        s.has_peaks = true;
        s.peaks.insert(id.into(), peak);
    }

    pub fn set_has_peaks(&self, on: bool) {
        self.state.lock().unwrap().has_peaks = on;
    }

    /// PNG bytes returned by `app_icon` for sessions whose process is `process`.
    pub fn set_icon(&self, process: &str, png: &[u8]) {
        self.state.lock().unwrap().icons.insert(process.into(), png.to_vec());
    }

    pub fn set_fail_list(&self, fail: bool) {
        self.state.lock().unwrap().fail_list = fail;
    }

    /// Recorded `set_session_*` calls.
    pub fn calls(&self) -> Vec<AppAudioCall> {
        self.state.lock().unwrap().calls.clone()
    }

    pub fn clear_calls(&self) {
        self.state.lock().unwrap().calls.clear();
    }

    /// How many times `sessions()` was called.
    pub fn list_calls(&self) -> usize {
        self.state.lock().unwrap().list_calls
    }

    /// Id lists of every `peaks()` call.
    pub fn peak_calls(&self) -> Vec<Vec<String>> {
        self.state.lock().unwrap().peak_calls.clone()
    }

    /// How many times `app_icon()` was called.
    pub fn icon_calls(&self) -> usize {
        self.state.lock().unwrap().icon_calls
    }

    /// Current stored session by id.
    pub fn get(&self, id: &str) -> Option<nd_platform::AudioSession> {
        self.state.lock().unwrap().sessions.iter().find(|s| s.id == id).cloned()
    }
}

impl nd_platform::AppAudio for MockAppAudio {
    fn backend_name(&self) -> &'static str {
        "mock"
    }

    fn sessions(&self) -> Result<Vec<nd_platform::AudioSession>> {
        let mut s = self.state.lock().unwrap();
        s.list_calls += 1;
        if s.fail_list {
            return Err(PlatformError::Other("mock list failure".to_string()));
        }
        Ok(s.sessions.clone())
    }

    fn set_session_volume(&self, id: &str, volume: f32) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let Some(sess) = s.sessions.iter_mut().find(|x| x.id == id) else {
            return Err(PlatformError::Other(format!("no session {id}")));
        };
        sess.volume = volume;
        s.calls.push(AppAudioCall::Volume(id.into(), volume));
        Ok(())
    }

    fn set_session_muted(&self, id: &str, muted: bool) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let Some(sess) = s.sessions.iter_mut().find(|x| x.id == id) else {
            return Err(PlatformError::Other(format!("no session {id}")));
        };
        sess.muted = muted;
        s.calls.push(AppAudioCall::Muted(id.into(), muted));
        Ok(())
    }

    fn has_peaks(&self) -> bool {
        self.state.lock().unwrap().has_peaks
    }

    fn peaks(&self, ids: &[String]) -> Result<Vec<Option<f32>>> {
        let mut s = self.state.lock().unwrap();
        s.peak_calls.push(ids.to_vec());
        Ok(ids.iter().map(|i| s.peaks.get(i).copied()).collect())
    }

    fn app_icon(&self, session: &nd_platform::AudioSession) -> Result<Option<Vec<u8>>> {
        let mut s = self.state.lock().unwrap();
        s.icon_calls += 1;
        Ok(s.icons.get(&session.process).cloned())
    }
}
