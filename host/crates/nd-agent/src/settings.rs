// SPDX-License-Identifier: GPL-3.0-or-later
//! Persisted user settings (`<config root>/settings.json`) and OS autostart sync.

use nd_config::{BackendError, Settings, SettingsPatch};
use nd_platform::Autostart;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::{info, warn};

const FILE: &str = "settings.json";

/// Load settings; missing or unreadable files give defaults.
pub fn load(root: &Path) -> Settings {
    match std::fs::read(root.join(FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            warn!("settings.json is invalid ({e}); using defaults");
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

/// Atomic save: write a temp file, then rename over the real one.
pub fn save(root: &Path, settings: &Settings) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let json = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    let tmp = root.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, root.join(FILE))
}

/// The OS registration is the truth for `autostart` (the user may have removed it elsewhere).
pub fn reconcile(mut settings: Settings, os_enabled: Option<bool>) -> Settings {
    if let Some(enabled) = os_enabled {
        settings.autostart = enabled;
    }
    settings
}

/// Settings plus the platform autostart hook.
pub struct SettingsStore {
    root: PathBuf,
    current: Mutex<Settings>,
    autostart: Box<dyn Autostart>,
}

impl SettingsStore {
    /// Load from `root` and reconcile `autostart` with the OS (persisting if it changed).
    pub fn open(root: PathBuf, autostart: Box<dyn Autostart>) -> Self {
        let loaded = load(&root);
        let os = match autostart.is_enabled() {
            Ok(v) => Some(v),
            Err(e) => {
                warn!("cannot read autostart state: {e}");
                None
            }
        };
        let settings = reconcile(loaded.clone(), os);
        if settings != loaded {
            info!(autostart = settings.autostart, "autostart setting reconciled with the OS");
            if let Err(e) = save(&root, &settings) {
                warn!("cannot save settings.json: {e}");
            }
        }
        Self { root, current: Mutex::new(settings), autostart }
    }

    /// First-run migration (round 23): an install that already has a paired phone predates the setup
    /// wizard, so it counts as set up. Returns whether it changed anything.
    pub fn migrate_onboarding(&self, has_paired_phone: bool) -> bool {
        if !has_paired_phone || self.get().onboarding_done {
            return false;
        }
        match self.update(SettingsPatch { onboarding_done: Some(true), ..Default::default() }) {
            Ok(_) => {
                info!("a phone is already paired; marking setup as done");
                true
            }
            Err(e) => {
                warn!("cannot save onboarding_done: {e}");
                false
            }
        }
    }

    pub fn get(&self) -> Settings {
        self.current.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Apply a patch: toggle OS autostart if needed, then persist.
    pub fn update(&self, patch: SettingsPatch) -> Result<Settings, BackendError> {
        let mut cur = self.current.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = cur.clone();
        if let Some(v) = patch.start_minimized {
            next.start_minimized = v;
        }
        if let Some(v) = patch.theme {
            next.theme = v;
        }
        if let Some(v) = patch.phone_theme {
            next.phone_theme = v;
        }
        if let Some(v) = patch.lan_enabled {
            next.lan_enabled = v;
        }
        if let Some(v) = patch.camera_lens {
            next.camera_lens = v;
        }
        if let Some(v) = patch.camera_mode {
            if !nd_config::valid_camera_mode(&v) {
                return Err(BackendError::Invalid(format!("'{v}' is not 'auto' or WIDTHxHEIGHT@FPS")));
            }
            next.camera_mode = v;
        }
        if let Some(v) = patch.camera_bitrate_kbps {
            if v != 0 && !(300..=20_000).contains(&v) {
                return Err(BackendError::Invalid("camera bitrate must be 0 (auto) or 300-20000 kbit/s".into()));
            }
            next.camera_bitrate_kbps = v;
        }
        if let Some(v) = patch.camera_mirror {
            next.camera_mirror = v;
        }
        if let Some(v) = patch.camera_auto_start {
            next.camera_auto_start = v;
        }
        if let Some(v) = patch.camera_image {
            v.validate().map_err(BackendError::Invalid)?;
            next.camera_image = v;
        }
        if let Some(v) = patch.onboarding_done {
            next.onboarding_done = v;
        }
        if let Some(v) = patch.mic_enabled {
            next.mic_enabled = v;
        }
        if let Some(v) = patch.mic_device {
            next.mic_device = v;
        }
        if let Some(v) = patch.mic_voice_processing {
            next.mic_voice_processing = v;
        }
        if let Some(v) = patch.mic_gain_db {
            if !(nd_config::MIC_GAIN_DB_MIN..=nd_config::MIC_GAIN_DB_MAX).contains(&v) {
                return Err(BackendError::Invalid(format!(
                    "mic_gain_db must be {}..{}",
                    nd_config::MIC_GAIN_DB_MIN,
                    nd_config::MIC_GAIN_DB_MAX
                )));
            }
            next.mic_gain_db = v;
        }
        if let Some(v) = patch.mic_guard_defaults {
            next.mic_guard_defaults = v;
        }
        if let Some(v) = patch.deck_sleep_enabled {
            next.deck_sleep_enabled = v;
        }
        if let Some(v) = patch.deck_sleep_idle_s {
            next.deck_sleep_idle_s = v.clamp(nd_config::DECK_SLEEP_IDLE_S_MIN, nd_config::DECK_SLEEP_IDLE_S_MAX);
        }
        if let Some(v) = patch.deck_sleep_wake_on_camera_mic {
            next.deck_sleep_wake_on_camera_mic = v;
        }
        if let Some(v) = patch.deck_sleep_clock {
            next.deck_sleep_clock = v;
        }
        if let Some(v) = patch.autostart {
            if v != next.autostart {
                self.autostart
                    .set_enabled(v)
                    .map_err(|e| BackendError::Internal(format!("cannot change autostart: {e}")))?;
            }
            next.autostart = v;
        }
        save(&self.root, &next).map_err(|e| BackendError::Internal(format!("cannot save settings: {e}")))?;
        *cur = next.clone();
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nd_config::Theme;
    use nd_platform::{PlatformError, Result as PResult};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct Fake {
        on: Arc<AtomicBool>,
        fail: Arc<AtomicBool>,
    }
    impl Autostart for Fake {
        fn is_enabled(&self) -> PResult<bool> {
            Ok(self.on.load(Ordering::SeqCst))
        }
        fn set_enabled(&self, enabled: bool) -> PResult<()> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(PlatformError::Unsupported("autostart"));
            }
            self.on.store(enabled, Ordering::SeqCst);
            Ok(())
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nd-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn save_load_roundtrip() {
        let d = tmp("rt");
        assert_eq!(load(&d), Settings::default());
        let s = Settings { autostart: true, start_minimized: true, theme: Theme::Dark, ..Default::default() };
        save(&d, &s).unwrap();
        assert_eq!(load(&d), s);
        assert!(!d.join("settings.json.tmp").exists());
    }

    #[test]
    fn reconcile_prefers_os_state() {
        let s = Settings { autostart: true, ..Default::default() };
        assert!(!reconcile(s.clone(), Some(false)).autostart);
        assert!(reconcile(s.clone(), None).autostart);
        assert!(reconcile(Settings::default(), Some(true)).autostart);
    }

    #[test]
    fn open_reconciles_and_persists() {
        let d = tmp("rec");
        save(&d, &Settings { autostart: true, ..Default::default() }).unwrap();
        let store = SettingsStore::open(d.clone(), Box::new(Fake::default()));
        assert!(!store.get().autostart);
        assert!(!load(&d).autostart);
    }

    #[test]
    fn update_applies_autostart_and_persists() {
        let d = tmp("upd");
        let fake = Fake::default();
        let store = SettingsStore::open(d.clone(), Box::new(fake.clone()));
        let s = store
            .update(SettingsPatch { autostart: Some(true), theme: Some(Theme::Light), ..Default::default() })
            .unwrap();
        assert!(s.autostart && fake.on.load(Ordering::SeqCst));
        assert_eq!(load(&d).theme, Theme::Light);
        fake.fail.store(true, Ordering::SeqCst);
        let err = store.update(SettingsPatch { autostart: Some(false), ..Default::default() });
        assert!(err.is_err());
        assert!(store.get().autostart, "failed toggle must not change settings");
    }

    #[test]
    fn update_clamps_deck_sleep_idle() {
        let d = tmp("sleep");
        let store = SettingsStore::open(d.clone(), Box::new(Fake::default()));
        let s = store
            .update(SettingsPatch { deck_sleep_idle_s: Some(5), deck_sleep_clock: Some(false), ..Default::default() })
            .unwrap();
        assert_eq!(s.deck_sleep_idle_s, 15);
        assert!(!s.deck_sleep_clock);
        let s = store.update(SettingsPatch { deck_sleep_idle_s: Some(100_000), ..Default::default() }).unwrap();
        assert_eq!(s.deck_sleep_idle_s, 3600);
        assert_eq!(load(&d).deck_sleep_idle_s, 3600);
    }

    #[test]
    fn update_applies_phone_theme() {
        let d = tmp("phone");
        let store = SettingsStore::open(d.clone(), Box::new(Fake::default()));
        let s = store
            .update(SettingsPatch { phone_theme: Some(nd_config::PhoneTheme::Graphite), ..Default::default() })
            .unwrap();
        assert_eq!(s.phone_theme, nd_config::PhoneTheme::Graphite);
        assert_eq!(load(&d).phone_theme, nd_config::PhoneTheme::Graphite);
    }

    #[test]
    fn update_applies_and_validates_camera_settings() {
        use nd_config::{CameraLens, MirrorMode};
        let d = tmp("cam");
        let store = SettingsStore::open(d.clone(), Box::new(Fake::default()));
        assert!(store.get().camera_auto_start);
        let s = store
            .update(SettingsPatch {
                camera_lens: Some(CameraLens::Front),
                camera_mode: Some("1280x720@30".into()),
                camera_bitrate_kbps: Some(3000),
                camera_mirror: Some(MirrorMode::Off),
                camera_auto_start: Some(false),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            (s.camera_lens, s.camera_mode.as_str(), s.camera_bitrate_kbps),
            (CameraLens::Front, "1280x720@30", 3000)
        );
        assert!(!s.camera_auto_start && s.camera_mirror == MirrorMode::Off);
        assert_eq!(load(&d), s);
        // Invalid values are rejected and change nothing.
        let bad = store.update(SettingsPatch {
            camera_mode: Some("huge".into()),
            camera_lens: Some(CameraLens::Back),
            ..Default::default()
        });
        assert!(matches!(bad, Err(BackendError::Invalid(_))));
        assert!(store.update(SettingsPatch { camera_bitrate_kbps: Some(5), ..Default::default() }).is_err());
        assert_eq!(store.get().camera_lens, CameraLens::Front);
        assert_eq!(
            store.update(SettingsPatch { camera_mode: Some("auto".into()), ..Default::default() }).unwrap().camera_mode,
            "auto"
        );
    }

    #[test]
    fn update_applies_lan_enabled() {
        let d = tmp("lan");
        let store = SettingsStore::open(d.clone(), Box::new(Fake::default()));
        assert!(store.get().lan_enabled, "LAN is on by default");
        let s = store.update(SettingsPatch { lan_enabled: Some(false), ..Default::default() }).unwrap();
        assert!(!s.lan_enabled);
        assert!(!load(&d).lan_enabled);
        // an unrelated patch leaves it alone
        let s = store.update(SettingsPatch { start_minimized: Some(true), ..Default::default() }).unwrap();
        assert!(!s.lan_enabled && s.start_minimized);
        let s = store.update(SettingsPatch { lan_enabled: Some(true), ..Default::default() }).unwrap();
        assert!(s.lan_enabled);
    }

    #[test]
    fn update_applies_and_validates_camera_image() {
        use nd_config::{CameraImageSettings, FocusMode};
        let d = tmp("img");
        let store = SettingsStore::open(d.clone(), Box::new(Fake::default()));
        assert_eq!(store.get().camera_image, CameraImageSettings::default());
        let img = CameraImageSettings { focus: FocusMode::Manual, focus_permille: 500, ..Default::default() };
        let s = store.update(SettingsPatch { camera_image: Some(img.clone()), ..Default::default() }).unwrap();
        assert_eq!(s.camera_image, img);
        assert_eq!(load(&d).camera_image, img);
        let bad = CameraImageSettings { focus_permille: 1001, ..Default::default() };
        let r = store.update(SettingsPatch { camera_image: Some(bad), ..Default::default() });
        assert!(matches!(r, Err(BackendError::Invalid(_))));
        assert_eq!(store.get().camera_image, img);
    }

    #[test]
    fn old_settings_file_without_camera_image_loads() {
        let d = tmp("oldimg");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(FILE), br#"{"start_minimized":true}"#).unwrap();
        let s = load(&d);
        assert!(s.start_minimized);
        assert_eq!(s.camera_image, nd_config::CameraImageSettings::default());
    }

    #[test]
    fn onboarding_migration_marks_paired_installs_done() {
        let d = tmp("onb");
        let store = SettingsStore::open(d.clone(), Box::new(Fake::default()));
        assert!(!store.get().onboarding_done);
        assert!(!store.migrate_onboarding(false), "no phone: wizard stays");
        assert!(!store.get().onboarding_done);
        assert!(store.migrate_onboarding(true));
        assert!(store.get().onboarding_done && load(&d).onboarding_done);
        assert!(!store.migrate_onboarding(true), "already done: nothing to change");
        // The wizard's "Run setup again" flips it back through the normal patch.
        let s = store.update(SettingsPatch { onboarding_done: Some(false), ..Default::default() }).unwrap();
        assert!(!s.onboarding_done);
    }
}
