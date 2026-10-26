// SPDX-License-Identifier: GPL-3.0-or-later
//! On-disk profile persistence (SPEC §9): `<config>/profiles/<id>.json`.

use crate::error::{CoreError, Result};
use crate::model::{
    Action, ActionDefinition, Control, ControlStateVisual, DataSource, MultiStep, Page, Profile, TriggerKind,
};
use nd_platform::AudioTarget;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Lightweight listing entry for a stored profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
}

/// Change-detection fingerprint: sorted `(id, mtime_nanos, len)` per profile file.
pub type Fingerprint = Vec<(String, u128, u64)>;

/// Resolve the default config root: `$XDG_CONFIG_HOME/notideck`, `~/.config/notideck`
/// or `%APPDATA%\Notideck` on Windows.
pub fn default_config_root() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        return base.join("Notideck");
    }
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(x).join("notideck");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("notideck")
}

/// Profile ids become file names, so restrict them to `[A-Za-z0-9_-]`, 1..=64 chars.
pub fn validate_profile_id(id: &str) -> Result<()> {
    let ok =
        !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(CoreError::Profile(format!("invalid profile id '{id}'")))
    }
}

/// Directory of profile JSON files.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    root: PathBuf,
}

impl ProfileStore {
    /// Store rooted at `root` (the config directory itself).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Store at the platform default location.
    pub fn open_default() -> Self {
        Self::new(default_config_root())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    pub fn icons_dir(&self) -> PathBuf {
        self.root.join("icons")
    }

    fn path_for(&self, id: &str) -> Result<PathBuf> {
        validate_profile_id(id)?;
        Ok(self.profiles_dir().join(format!("{id}.json")))
    }

    pub fn exists(&self, id: &str) -> bool {
        self.path_for(id).map(|p| p.is_file()).unwrap_or(false)
    }

    /// Load one profile.
    pub fn load(&self, id: &str) -> Result<Profile> {
        let path = self.path_for(id)?;
        let text = fs::read_to_string(&path)?;
        Profile::from_json(&text)
    }

    /// Save a profile atomically (write temp file, then rename).
    pub fn save(&self, profile: &Profile) -> Result<()> {
        let path = self.path_for(&profile.id)?;
        let dir = self.profiles_dir();
        fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!(".{}.json.tmp", profile.id));
        let json = profile.to_json_pretty()?;
        {
            use std::io::Write;
            let mut f = fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })?;
        Ok(())
    }

    /// Delete a profile file.
    pub fn delete(&self, id: &str) -> Result<()> {
        fs::remove_file(self.path_for(id)?)?;
        Ok(())
    }

    /// Create and persist an empty profile with one blank page. Fails if it exists.
    pub fn create(&self, id: &str, name: &str) -> Result<Profile> {
        if self.exists(id) {
            return Err(CoreError::Profile(format!("profile '{id}' already exists")));
        }
        let mut profile = Profile::new(id, name);
        profile.add_page(Page::new("home", "Home"));
        self.save(&profile)?;
        Ok(profile)
    }

    /// List stored profiles sorted by id. Unreadable files are skipped.
    pub fn list(&self) -> Result<Vec<ProfileSummary>> {
        let mut out = Vec::new();
        for id in self.ids()? {
            match self.load(&id) {
                Ok(p) => out.push(ProfileSummary { id, name: p.name }),
                Err(e) => tracing::warn!("skipping unreadable profile '{id}': {e}"),
            }
        }
        Ok(out)
    }

    fn ids(&self) -> Result<Vec<String>> {
        let dir = self.profiles_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if validate_profile_id(stem).is_ok() {
                    ids.push(stem.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Cheap fingerprint of the profile directory, for mtime polling.
    pub fn fingerprint(&self) -> Fingerprint {
        let mut fp = Vec::new();
        for id in self.ids().unwrap_or_default() {
            if let Ok(meta) = self.path_for(&id).and_then(|p| Ok(fs::metadata(p)?)) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                fp.push((id, mtime, meta.len()));
            }
        }
        fp
    }

    /// Write the default profile if no profiles exist. Returns true if seeded.
    pub fn seed_default_if_empty(&self) -> Result<bool> {
        if !self.ids()?.is_empty() {
            return Ok(false);
        }
        self.save(&default_profile())?;
        Ok(true)
    }
}

/// Built-in starter profile (seeded when the store is empty).
pub fn default_profile() -> Profile {
    let mut page = Page::new("home", "Default Page");
    page.cols = Some(5);
    page.rows = Some(3);

    let k_github = Control::key("k_github", 0, 0, "GitHub");
    page.add_control(k_github.with_action(Action::open_url("https://github.com")));

    let k_term = Control::key("k_term", 1, 0, "Terminal");
    page.add_control(k_term.with_action(Action::launch_app_confirmed("x-terminal-emulator", vec![])));

    let mut k_hello = Control::key("k_hello", 2, 0, "Say Hi");
    k_hello.bg_argb = 0xFF2B3A4F;
    k_hello.fg_argb = 0xFFE3E6EA;
    page.add_control(k_hello.with_action(Action::type_text("Hello from Notideck!\n")));

    let mut k_mute = Control::key("k_mute", 4, 2, "Mute");
    k_mute.source = Some(DataSource::Mute { target: AudioTarget::Output });
    k_mute.states = vec![
        ControlStateVisual::default(),
        ControlStateVisual { title: Some("Muted".into()), bg_argb: Some(0xFF5A2E2C), ..Default::default() },
    ];
    page.add_control(k_mute.with_action(Action::ToggleMute { target: AudioTarget::Output }));

    // Multi-action: copy, short pause, paste.
    let mut k_dup = Control::key("k_dup", 3, 0, "Copy+Paste");
    k_dup.bg_argb = 0xFF3A3350;
    k_dup.fg_argb = 0xFFE3E6EA;
    page.add_control(k_dup.with_action(Action::multi(vec![
        MultiStep::new(Action::hotkey_with_mods(vec![nd_platform::Key::Ctrl], nd_platform::Key::C)),
        MultiStep::after(150, Action::hotkey_with_mods(vec![nd_platform::Key::Ctrl], nd_platform::Key::V)),
    ])));

    // Two-state toggle key (state 0 = playing, state 1 = paused); each press toggles the state.
    let mut k_play = Control::key("k_play", 0, 1, "Pause");
    k_play.bg_argb = 0xFF2F4232;
    k_play.fg_argb = 0xFFE3E6EA;
    k_play.states = vec![
        ControlStateVisual { title: Some("Pause".into()), bg_argb: Some(0xFF2F4232), ..Default::default() },
        ControlStateVisual { title: Some("Play".into()), bg_argb: Some(0xFF4F3A26), ..Default::default() },
    ];
    k_play.actions.push(ActionDefinition::new(Action::multi(vec![
        MultiStep::new(Action::hotkey(nd_platform::Key::MediaPlayPause)),
        MultiStep::new(Action::ToggleState),
    ])));
    page.add_control(k_play);

    // Dial: rotate = output volume, press = toggle mute; shows the live volume.
    let mut d_vol = Control::dial("d_vol", 1, 1, "Volume");
    d_vol.value = 0.5;
    d_vol.source = Some(DataSource::Volume { target: AudioTarget::Output });
    d_vol.actions.push(ActionDefinition::with_trigger(
        Action::AdjustVolume { target: AudioTarget::Output, step: 0.02 },
        TriggerKind::DialRotate,
    ));
    d_vol.actions.push(ActionDefinition::with_trigger(
        Action::ToggleMute { target: AudioTarget::Output },
        TriggerKind::DialPress,
    ));
    page.add_control(d_vol);

    // Tile: live CPU load.
    let mut t_info = Control::tile("t_info", 2, 1, "CPU", "");
    t_info.source = Some(DataSource::Cpu);
    page.add_control(t_info);

    // Clock and now-playing tiles.
    let mut t_clock = Control::tile("t_clock", 4, 0, "", "");
    t_clock.source = Some(DataSource::Clock);
    page.add_control(t_clock);
    let mut t_media = Control::tile("t_media", 4, 1, "Now playing", "");
    t_media.source = Some(DataSource::NowPlaying { art: false });
    page.add_control(t_media);

    // Horizontal slider: microphone level (value mirrored from the host).
    let mut s_bright = Control::slider("s_level", 0, 2, 3, 1, true);
    s_bright.title = "Mic".into();
    s_bright.value = 0.3;
    s_bright.source = Some(DataSource::Volume { target: AudioTarget::Input });
    s_bright.actions.push(ActionDefinition::with_trigger(
        Action::SetVolume { target: AudioTarget::Input, level: None },
        TriggerKind::SliderValue,
    ));
    page.add_control(s_bright);

    // Folder: key opens `tools` page, which has a Back key.
    let mut k_folder = Control::key("k_tools", 3, 1, "Tools");
    k_folder.bg_argb = 0xFF2A3036;
    k_folder.fg_argb = 0xFFE3E6EA;
    page.add_control(k_folder.with_action(Action::open_folder("tools")));

    let mut folder = Page::new("tools", "Tools");
    folder.cols = Some(5);
    folder.rows = Some(3);
    folder.add_control(Control::key("k_back", 0, 0, "Back").with_action(Action::GoBack));
    let mut k_vol_up = Control::key("k_vol_up", 1, 0, "Vol +");
    k_vol_up.bg_argb = 0xFF263238;
    k_vol_up.fg_argb = 0xFFE3E6EA;
    folder.add_control(k_vol_up.with_action(Action::hotkey(nd_platform::Key::VolumeUp)));
    let mut k_vol_down = Control::key("k_vol_down", 2, 0, "Vol -");
    k_vol_down.bg_argb = 0xFF263238;
    k_vol_down.fg_argb = 0xFFE3E6EA;
    folder.add_control(k_vol_down.with_action(Action::hotkey(nd_platform::Key::VolumeDown)));

    let mut profile = Profile::new("default", "Default Profile");
    profile.cols = 5;
    profile.rows = 3;
    profile.add_page(page);
    profile.add_page(folder);
    profile
}

/// All distinct non-empty icon hashes referenced by a profile (including per-state icons).
pub fn referenced_icon_hashes(profile: &Profile) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for page in &profile.pages {
        for c in &page.controls {
            if !c.icon_hash.is_empty() {
                set.insert(c.icon_hash.clone());
            }
            for s in &c.states {
                if let Some(h) = s.icon_hash.as_ref().filter(|h| !h.is_empty()) {
                    set.insert(h.clone());
                }
            }
        }
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> ProfileStore {
        let dir = std::env::temp_dir().join(format!("nd-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        ProfileStore::new(dir)
    }

    #[test]
    fn id_validation() {
        assert!(validate_profile_id("obs-studio_2").is_ok());
        assert!(validate_profile_id("").is_err());
        assert!(validate_profile_id("../x").is_err());
        assert!(validate_profile_id("a/b").is_err());
    }

    #[test]
    fn save_load_roundtrip_and_list() {
        let store = temp_store("rt");
        let p = default_profile();
        store.save(&p).unwrap();
        assert_eq!(store.load("default").unwrap(), p);
        store.create("second", "Second").unwrap();
        assert!(store.create("second", "Again").is_err());
        let list = store.list().unwrap();
        assert_eq!(list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["default", "second"]);
        store.delete("second").unwrap();
        assert!(!store.exists("second"));
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn seed_only_when_empty() {
        let store = temp_store("seed");
        assert!(store.seed_default_if_empty().unwrap());
        assert!(!store.seed_default_if_empty().unwrap());
        assert_eq!(store.list().unwrap().len(), 1);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn fingerprint_changes_on_save() {
        let store = temp_store("fp");
        store.seed_default_if_empty().unwrap();
        let a = store.fingerprint();
        let mut p = store.load("default").unwrap();
        p.name = "Renamed profile".to_string();
        store.save(&p).unwrap();
        assert_ne!(a, store.fingerprint());
        let _ = fs::remove_dir_all(store.root());
    }
}
