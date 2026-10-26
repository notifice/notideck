// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared agent state: profile store, icon store and the live deck session.
//!
//! `AgentState` is cheap to clone (`Arc` inside) and exposes a synchronous API so it
//! can be called from the config server (WP-D) as well as from the transport loop.
//! Locks are never held across `.await`.

use nd_core::autoswitch::{PageMemory, SwitchTarget};
use nd_core::live::LiveSample;
use nd_core::plugin::{split_context, PluginInstance, PluginSettings, RAIL_CONTEXT};
use nd_core::{
    referenced_icon_hashes, ActionRouter, CoreError, DeckSession, IconStore, InputReaction, Profile, ProfileStore,
    ProfileSummary,
};
use nd_proto::v1::{InputEvent, PutIcon, SetLayout, SetRail, UpdateControls};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Id prefix marking rail keys in combined live sampling.
const RAIL_PREFIX: &str = "\u{1}rail/";

pub type Result<T> = std::result::Result<T, CoreError>;

/// Notifications emitted when state changes outside the input path.
#[derive(Debug, Clone)]
pub enum StateEvent {
    /// The active layout changed (profile edited, switched or reloaded). Re-push to phones.
    LayoutChanged(SetLayout),
    /// The set of stored profiles changed (informational, for UIs).
    ProfilesChanged,
    /// The side-rail keys changed. Push the `SetRail` (and its icons) to phones.
    RailChanged(SetRail),
}

/// What the phone's navigation rail needs to know about the deck (see [`AgentState::nav_snapshot`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavSnapshot {
    pub can_go_back: bool,
    pub on_home: bool,
    pub profile_name: String,
    /// Index of the visible top-level page (0 while in a folder).
    pub page_index: u32,
    /// Top-level pages in the profile; 0 while a folder is open.
    pub page_count: u32,
}

struct Inner {
    store: ProfileStore,
    icons: IconStore,
    session: DeckSession,
    fingerprint: nd_core::store::Fingerprint,
    /// Last page shown per profile in this run (auto-switch page restore).
    pages: PageMemory,
    /// Configured rail keys (static values; the session holds their runtime state).
    rail: Vec<nd_core::Control>,
    /// Stamp of `rail.json` as last seen or written by us (external-edit detection).
    rail_stamp: Option<FileStamp>,
}

/// Set `settings` on every plugin binding of `plugin` in the controls with id `control_id`; true if any differed.
fn patch_plugin_settings<'a>(
    controls: impl Iterator<Item = &'a mut nd_core::Control>,
    control_id: &str,
    plugin: &str,
    settings: &PluginSettings,
) -> bool {
    let mut changed = false;
    for c in controls.filter(|c| c.id == control_id) {
        for def in &mut c.actions {
            if let nd_core::model::Action::Plugin { plugin: p, settings: s, .. } = &mut def.action {
                if p == plugin && s != settings {
                    *s = settings.clone();
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Modification time and size of a file.
type FileStamp = (std::time::SystemTime, u64);

fn file_stamp(path: &std::path::Path) -> Option<FileStamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

#[derive(Clone)]
pub struct AgentState {
    inner: Arc<RwLock<Inner>>,
    events: broadcast::Sender<StateEvent>,
    /// Id of the active profile, readable without the state lock (updated wherever the session's profile
    /// changes). Used by `ProfileActions` methods that run with the state lock held.
    active_id: Arc<std::sync::Mutex<String>>,
}

impl AgentState {
    /// Open the store, seed the default profile if empty and activate the first profile.
    pub fn open(store: ProfileStore) -> Result<Self> {
        if store.seed_default_if_empty()? {
            info!("Seeded default profile in {}", store.root().display());
        }
        let list = store.list()?;
        let first = list
            .iter()
            .find(|p| p.id == "default")
            .or_else(|| list.first())
            .ok_or_else(|| CoreError::Profile("no readable profiles".into()))?;
        let profile = store.load(&first.id)?;
        let icons = IconStore::new(store.icons_dir());
        let fingerprint = store.fingerprint();
        let (events, _) = broadcast::channel(32);
        let rail = nd_core::load_rail(store.root());
        let rail_stamp = file_stamp(&store.root().join("rail.json"));
        let mut session = DeckSession::new(profile);
        session.set_rail(rail.clone());
        let active_id = Arc::new(std::sync::Mutex::new(session.profile().id.clone()));
        Ok(Self {
            active_id,
            inner: Arc::new(RwLock::new(Inner {
                store,
                icons,
                session,
                fingerprint,
                pages: PageMemory::default(),
                rail,
                rail_stamp,
            })),
            events,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, ev: StateEvent) {
        let _ = self.events.send(ev);
    }

    // ------------------------------------------------------------- profiles

    pub fn list_profiles(&self) -> Result<Vec<ProfileSummary>> {
        self.read().store.list()
    }

    pub fn get_profile(&self, id: &str) -> Result<Profile> {
        self.read().store.load(id)
    }

    pub fn active_profile_id(&self) -> String {
        self.read().session.profile().id.clone()
    }

    /// Like [`Self::active_profile_id`] but never touches the state lock (safe while it is held).
    pub fn active_profile_id_nolock(&self) -> String {
        self.active_id.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn note_active(&self, id: &str) {
        *self.active_id.lock().unwrap_or_else(|e| e.into_inner()) = id.to_string();
    }

    /// Create or replace a profile. If it is the active one, the live session is updated.
    pub fn put_profile(&self, profile: Profile) -> Result<()> {
        let layout = {
            let mut g = self.write();
            g.store.save(&profile)?;
            g.fingerprint = g.store.fingerprint();
            if g.session.profile().id == profile.id {
                Some(Self::swap_profile(&mut g.session, profile))
            } else {
                None
            }
        };
        self.emit(StateEvent::ProfilesChanged);
        if let Some(l) = layout.transpose()? {
            self.emit(StateEvent::LayoutChanged(l));
        }
        Ok(())
    }

    /// Delete a profile. The active profile cannot be deleted.
    pub fn delete_profile(&self, id: &str) -> Result<()> {
        {
            let mut g = self.write();
            if g.session.profile().id == id {
                return Err(CoreError::Profile("cannot delete the active profile".into()));
            }
            g.store.delete(id)?;
            g.fingerprint = g.store.fingerprint();
        }
        self.emit(StateEvent::ProfilesChanged);
        Ok(())
    }

    /// Switch the active profile and push its layout.
    pub fn activate_profile(&self, id: &str) -> Result<()> {
        let layout = {
            let mut g = self.write();
            let profile = g.store.load(id)?;
            let l = g.session.set_profile(profile)?;
            self.note_active(id);
            l
        };
        self.emit(StateEvent::LayoutChanged(layout));
        Ok(())
    }

    /// Replace the session's profile, keeping the current page when it still exists.
    fn swap_profile(session: &mut DeckSession, profile: Profile) -> Result<SetLayout> {
        let page = session.active_page_id().to_string();
        let layout = session.set_profile(profile)?;
        if session.profile().get_page(&page).is_some() {
            session.navigate_to(&page)
        } else {
            Ok(layout)
        }
    }

    // ---------------------------------------------------------------- icons

    /// Import PNG/WebP bytes, returning the content hash.
    pub fn put_icon_bytes(&self, data: &[u8]) -> Result<String> {
        self.read().icons.import(data)
    }

    /// Fetch icon `(mime, bytes)` by hash.
    pub fn get_icon(&self, hash: &str) -> Option<(String, Vec<u8>)> {
        self.read().icons.get(hash)
    }

    /// `PutIcon` messages for the given hashes (unknown hashes are skipped).
    pub fn put_icons_for(&self, hashes: &[String]) -> Vec<PutIcon> {
        let g = self.read();
        hashes.iter().filter_map(|h| g.icons.put_icon(h)).collect()
    }

    /// All icon hashes referenced by the active profile and the rail that exist in the store.
    pub fn active_icon_messages(&self) -> Vec<PutIcon> {
        let g = self.read();
        let mut hashes = referenced_icon_hashes(g.session.profile());
        for h in nd_core::rail_icon_hashes(&g.rail) {
            if !hashes.contains(&h) {
                hashes.push(h);
            }
        }
        hashes.iter().filter_map(|h| g.icons.put_icon(h)).collect()
    }

    // -------------------------------------------------------------- session

    /// Layout of the currently active page.
    pub fn current_layout(&self) -> Result<SetLayout> {
        self.read().session.current_layout()
    }

    /// Route a phone input event through the active session.
    pub fn handle_input(&self, event: &InputEvent, router: &ActionRouter) -> Result<InputReaction> {
        let mut g = self.write();
        let reaction = g.session.handle_input(event, router);
        if matches!(reaction, Ok(InputReaction::SetLayout(_))) {
            Self::remember_page(&mut g);
        }
        reaction
    }

    /// Record the visible page of the active profile (folders are not remembered).
    fn remember_page(g: &mut Inner) {
        if !g.session.is_in_folder() {
            let (profile, page) = (g.session.profile().id.clone(), g.session.active_page_id().to_string());
            g.pages.remember(&profile, &page);
        }
    }

    /// Id of the page the deck currently shows.
    pub fn active_page_id(&self) -> String {
        self.read().session.active_page_id().to_string()
    }

    /// Apply an auto-switch target: activate the profile, then show `page_id` if it exists, else the
    /// page remembered for that profile, else the profile's initial page. Does nothing (returns
    /// `None`) when the profile is already active and no page is requested. Emits one layout event
    /// and returns the page now shown.
    pub fn apply_switch(&self, target: &SwitchTarget) -> Result<Option<String>> {
        let (layout, shown) = {
            let mut g = self.write();
            let same = g.session.profile().id == target.profile_id;
            if same && target.page_id.is_none() {
                return Ok(None);
            }
            let mut layout = None;
            if !same {
                let profile = g.store.load(&target.profile_id)?;
                layout = Some(g.session.set_profile(profile)?);
                self.note_active(&target.profile_id);
            }
            let want = target
                .page_id
                .as_deref()
                .filter(|p| g.session.profile().get_page(p).is_some())
                .or_else(|| {
                    if same {
                        None
                    } else {
                        g.pages.page_for(&target.profile_id).filter(|p| g.session.profile().get_page(p).is_some())
                    }
                })
                .map(str::to_string);
            if let Some(page) = want {
                if page != g.session.active_page_id() || g.session.is_in_folder() {
                    layout = Some(g.session.navigate_to(&page)?);
                }
            }
            Self::remember_page(&mut g);
            let shown = g.session.active_page_id().to_string();
            (layout, shown)
        };
        if let Some(l) = layout {
            self.emit(StateEvent::LayoutChanged(l));
        }
        Ok(Some(shown))
    }

    /// Show `profile_id` on its initial page (not the remembered one), even when it is already the
    /// active profile. Emits one layout event and returns the page now shown.
    pub fn show_initial_page(&self, profile_id: &str) -> Result<String> {
        let (layout, shown) = {
            let mut g = self.write();
            let profile = g.store.load(profile_id)?;
            let layout = g.session.set_profile(profile)?;
            self.note_active(profile_id);
            Self::remember_page(&mut g);
            (layout, g.session.active_page_id().to_string())
        };
        self.emit(StateEvent::LayoutChanged(layout));
        Ok(shown)
    }

    // ------------------------------------------------------------ side rail keys

    /// Configured rail keys (static values, as saved).
    pub fn rail_keys(&self) -> Vec<nd_core::Control> {
        self.read().rail.clone()
    }

    /// `SetRail` for the current rail (runtime state included).
    pub fn rail_proto(&self) -> SetRail {
        self.read().session.rail_proto()
    }

    /// Validate, persist and apply new rail keys, then emit `RailChanged`. `Err` carries the
    /// validation messages (nothing is saved) or an I/O failure.
    pub fn set_rail(&self, keys: Vec<nd_core::Control>) -> std::result::Result<(), Vec<String>> {
        let errors = nd_core::validate_rail(&keys);
        if !errors.is_empty() {
            return Err(errors);
        }
        let msg = {
            let mut g = self.write();
            nd_core::save_rail(g.store.root(), &keys).map_err(|e| vec![format!("cannot save rail.json: {e}")])?;
            g.rail_stamp = file_stamp(&g.store.root().join("rail.json"));
            g.rail = keys.clone();
            g.session.set_rail(keys);
            g.session.rail_proto()
        };
        self.emit(StateEvent::RailChanged(msg));
        Ok(())
    }

    // ------------------------------------------------------------ navigation rail

    /// Navigation facts for `NavState`. `main_profile` is the resolved main profile id.
    pub fn nav_snapshot(&self, main_profile: Option<&str>) -> NavSnapshot {
        let g = self.read();
        let profile = g.session.profile();
        let initial = profile.initial_page().map(|p| p.id.as_str()).unwrap_or_default();
        let in_folder = g.session.is_in_folder();
        let on_initial = g.session.active_page_id() == initial;
        NavSnapshot {
            can_go_back: in_folder || !on_initial,
            on_home: Some(profile.id.as_str()) == main_profile && on_initial && !in_folder,
            profile_name: profile.name.clone(),
            page_index: g.session.page_position().0,
            page_count: g.session.page_position().1,
        }
    }

    /// Phone page dot: close any open folder and show the top-level page `index`. Out-of-range
    /// indices do nothing (false, no event). Emits one layout event otherwise.
    pub fn nav_go_to_page(&self, index: usize) -> Result<bool> {
        let layout = {
            let mut g = self.write();
            let Some(layout) = g.session.go_to_page_index(index) else { return Ok(false) };
            let layout = layout?;
            Self::remember_page(&mut g);
            layout
        };
        self.emit(StateEvent::LayoutChanged(layout));
        Ok(true)
    }

    /// Phone Back button: close the open folder, else go to the profile's initial page. Returns
    /// false (and emits nothing) when there is nowhere to go. Emits one layout event otherwise.
    pub fn nav_back(&self) -> Result<bool> {
        let layout = {
            let mut g = self.write();
            let initial = g.session.profile().initial_page().map(|p| p.id.clone());
            let layout = if g.session.is_in_folder() {
                g.session.go_back()?
            } else {
                match initial {
                    Some(i) if i != g.session.active_page_id() => g.session.navigate_to(&i)?,
                    _ => return Ok(false),
                }
            };
            Self::remember_page(&mut g);
            layout
        };
        self.emit(StateEvent::LayoutChanged(layout));
        Ok(true)
    }

    // ------------------------------------------------------------ live data

    /// Controls on the active page that display live data. Cheap; takes the read lock briefly.
    pub fn live_sources(&self) -> Vec<(String, nd_core::DataSource)> {
        self.read().session.live_sources()
    }

    /// Live sources of the active page plus the rail keys. Rail ids are prefixed with
    /// [`RAIL_PREFIX`] so both sets can be sampled in one pass (ids may repeat across them).
    pub fn live_sources_all(&self) -> Vec<(String, nd_core::DataSource)> {
        let g = self.read();
        let mut out = g.session.live_sources();
        out.extend(g.session.rail_live_sources().into_iter().map(|(id, s)| (format!("{RAIL_PREFIX}{id}"), s)));
        out
    }

    /// Apply samples from [`AgentState::live_sources_all`]; returns the page patch and the rail patch.
    pub fn apply_live_all(&self, samples: &[(String, LiveSample)]) -> Vec<UpdateControls> {
        let (rail, page): (Vec<_>, Vec<_>) = samples.iter().cloned().partition(|(id, _)| id.starts_with(RAIL_PREFIX));
        let rail: Vec<_> = rail.into_iter().map(|(id, s)| (id[RAIL_PREFIX.len()..].to_string(), s)).collect();
        let mut g = self.write();
        let mut out = Vec::new();
        out.extend(g.session.apply_live(&page));
        out.extend(g.session.apply_live_rail(&rail));
        out
    }

    /// Round 19: plugin actions on the active page plus the rail keys (rail ids carry [`RAIL_PREFIX`], like
    /// [`AgentState::live_sources_all`]). Cheap; read lock only.
    pub fn plugin_instances_all(&self) -> Vec<(String, PluginInstance)> {
        let g = self.read();
        let mut out = g.session.plugin_instances();
        out.extend(g.session.rail_plugin_instances().into_iter().map(|(id, i)| (format!("{RAIL_PREFIX}{id}"), i)));
        out
    }

    /// The control id to flash for plugin `context`, when that action is on screen (active page or rail).
    pub fn control_for_context(&self, context: &str) -> Option<String> {
        self.plugin_instances_all()
            .into_iter()
            .find(|(_, i)| i.context == context)
            .map(|(id, _)| id.strip_prefix(RAIL_PREFIX).map(str::to_string).unwrap_or(id))
    }

    /// Round 19 `setSettings`: write `settings` into every `Action::Plugin` binding of `plugin` on the context's
    /// control (stored profile or `rail.json`), and mirror it into the running session in place. Nothing is
    /// pushed to the phone (the settings do not change how the control looks by themselves), and own writes
    /// update the change-detection stamps so the watcher does not reload them. Emits `ProfilesChanged` so the
    /// config UI refreshes its list. Returns whether anything changed.
    pub fn plugin_set_settings(&self, context: &str, plugin: &str, settings: &PluginSettings) -> Result<bool> {
        let Some((scope, control_id)) = split_context(context) else { return Ok(false) };
        let changed = {
            let mut g = self.write();
            if scope == RAIL_CONTEXT {
                let mut keys = g.rail.clone();
                if !patch_plugin_settings(keys.iter_mut(), control_id, plugin, settings) {
                    return Ok(false);
                }
                nd_core::save_rail(g.store.root(), &keys)?;
                g.rail_stamp = file_stamp(&g.store.root().join("rail.json"));
                g.rail = keys;
                patch_plugin_settings(g.session.rail_mut().iter_mut(), control_id, plugin, settings);
                false
            } else {
                // Persist from the stored copy: the session's profile carries runtime state (live titles).
                let mut stored = g.store.load(scope)?;
                let mut any = false;
                for page in &mut stored.pages {
                    any |= patch_plugin_settings(page.controls.iter_mut(), control_id, plugin, settings);
                }
                if !any {
                    return Ok(false);
                }
                g.store.save(&stored)?;
                g.fingerprint = g.store.fingerprint();
                if g.session.profile().id == scope {
                    let profile = g.session.profile_mut();
                    for page in &mut profile.pages {
                        patch_plugin_settings(page.controls.iter_mut(), control_id, plugin, settings);
                    }
                }
                true
            }
        };
        if changed {
            self.emit(StateEvent::ProfilesChanged);
        }
        Ok(true)
    }

    /// Whether any visible source (page or rail) could have a level meter. Cheap; read lock only.
    pub fn has_meter_candidates(&self) -> bool {
        let g = self.read();
        let wants = |(_, s): &(String, nd_core::DataSource)| {
            matches!(s, nd_core::DataSource::AppAudio { .. } | nd_core::DataSource::Volume { .. })
        };
        g.session.live_sources().iter().any(wants) || g.session.rail_live_sources().iter().any(wants)
    }

    /// Meter targets of the visible page plus the rail (rail ids carry [`RAIL_PREFIX`]). May query
    /// the audio backend, so call it off the async reactor; the state lock is not held meanwhile.
    pub fn meter_targets_all(&self, p: &nd_core::LiveProviders) -> Vec<(String, nd_core::live::MeterTarget)> {
        nd_core::live::meter_targets(&self.live_sources_all(), p)
    }

    /// Apply levels from [`AgentState::meter_targets_all`]; returns the page patch and the rail patch.
    pub fn apply_levels_all(&self, levels: &[(String, f32)]) -> Vec<UpdateControls> {
        let (rail, page): (Vec<_>, Vec<_>) = levels.iter().cloned().partition(|(id, _)| id.starts_with(RAIL_PREFIX));
        let rail: Vec<_> = rail.into_iter().map(|(id, l)| (id[RAIL_PREFIX.len()..].to_string(), l)).collect();
        let mut g = self.write();
        let mut out = Vec::new();
        out.extend(g.session.apply_levels(&page));
        out.extend(g.session.apply_levels_rail(&rail));
        out
    }

    /// Write samples into the active page and return the change-only patch, if any.
    ///
    /// The diff is against the session's own controls, which a layout push resets to the
    /// profile's static values (and which every pushed layout reflects), so the phone is
    /// always consistent with what the next sample is compared to.
    pub fn apply_live(&self, samples: &[(String, LiveSample)]) -> Option<UpdateControls> {
        self.write().session.apply_live(samples)
    }

    // ------------------------------------------------------------ live mirror

    /// Check the profile directory for external edits; reload the active profile if it
    /// changed on disk. Returns true if anything changed. Called by [`spawn_watcher`].
    pub fn poll_changes(&self) -> bool {
        let rail = self.poll_rail();
        self.poll_profiles() || rail
    }

    /// Reload `rail.json` when it was edited outside the agent (our own saves update the stamp).
    /// An unreadable or invalid file keeps the running rail. Emits `RailChanged` on a real change.
    fn poll_rail(&self) -> bool {
        let msg = {
            let mut g = self.write();
            let path = g.store.root().join("rail.json");
            let stamp = file_stamp(&path);
            if stamp == g.rail_stamp {
                return false;
            }
            g.rail_stamp = stamp;
            let keys = match stamp {
                None => Vec::new(),
                Some(_) => {
                    let parsed = std::fs::read(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|b| serde_json::from_slice::<nd_core::RailConfig>(&b).map_err(|e| e.to_string()));
                    match parsed {
                        Ok(cfg) if nd_core::validate_rail(&cfg.keys).is_empty() => cfg.keys,
                        Ok(cfg) => {
                            warn!(
                                "rail.json is invalid ({}); keeping the current rail",
                                nd_core::validate_rail(&cfg.keys).join(" ")
                            );
                            return false;
                        }
                        Err(e) => {
                            warn!("rail.json is invalid ({e}); keeping the current rail");
                            return false;
                        }
                    }
                }
            };
            if keys == g.rail {
                return false;
            }
            g.rail = keys.clone();
            g.session.set_rail(keys);
            g.session.rail_proto()
        };
        info!("rail.json changed on disk; live-reloading the rail");
        self.emit(StateEvent::RailChanged(msg));
        true
    }

    fn poll_profiles(&self) -> bool {
        let layout = {
            let mut g = self.write();
            let fp = g.store.fingerprint();
            if fp == g.fingerprint {
                return false;
            }
            let old = std::mem::replace(&mut g.fingerprint, fp);
            let active = g.session.profile().id.clone();
            let changed = |fp: &nd_core::store::Fingerprint| fp.iter().find(|e| e.0 == active).cloned();
            if changed(&old) == changed(&g.fingerprint) {
                None
            } else {
                match g.store.load(&active) {
                    Ok(p) => match Self::swap_profile(&mut g.session, p) {
                        Ok(l) => Some(l),
                        Err(e) => {
                            warn!("reloaded profile '{active}' has no usable layout: {e}");
                            None
                        }
                    },
                    // Mid-write or invalid JSON: keep the running layout.
                    Err(e) => {
                        warn!("failed to reload profile '{active}': {e}");
                        None
                    }
                }
            }
        };
        self.emit(StateEvent::ProfilesChanged);
        if let Some(l) = layout {
            info!("Profile changed on disk; live-reloading layout");
            self.emit(StateEvent::LayoutChanged(l));
        }
        true
    }

    /// Spawn a 2-second mtime poll of the profile directory.
    pub fn spawn_watcher(&self) -> tokio::task::JoinHandle<()> {
        let state = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tick.tick().await;
                let s = state.clone();
                let _ = tokio::task::spawn_blocking(move || s.poll_changes()).await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(tag: &str) -> AgentState {
        let dir = std::env::temp_dir().join(format!("nd-agent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        AgentState::open(ProfileStore::new(dir)).unwrap()
    }

    #[test]
    fn seeds_and_activates_default() {
        let s = state("seed");
        assert_eq!(s.active_profile_id(), "default");
        assert_eq!(s.current_layout().unwrap().page_id, "home");
    }

    #[test]
    fn put_active_profile_emits_layout() {
        let s = state("put");
        let mut rx = s.subscribe();
        let mut p = s.get_profile("default").unwrap();
        p.name = "Edited".into();
        s.put_profile(p).unwrap();
        let mut saw_layout = false;
        while let Ok(ev) = rx.try_recv() {
            saw_layout |= matches!(ev, StateEvent::LayoutChanged(_));
        }
        assert!(saw_layout);
        assert!(!s.poll_changes(), "own writes must not trigger a reload");
    }

    fn second_profile(s: &AgentState) {
        let mut p = s.get_profile("default").unwrap();
        p.id = "other".into();
        p.name = "Other".into();
        s.put_profile(p).unwrap();
    }

    #[test]
    fn apply_switch_activates_and_restores_pages() {
        let s = state("switch");
        second_profile(&s);
        let t = |p: &str, page: Option<&str>| SwitchTarget {
            profile_id: p.into(),
            page_id: page.map(Into::into),
            rule_id: None,
        };
        // already active, no page: nothing happens
        assert_eq!(s.apply_switch(&t("default", None)).unwrap(), None);
        let mut rx = s.subscribe();
        assert_eq!(s.apply_switch(&t("other", None)).unwrap().as_deref(), Some("home"));
        assert_eq!(s.active_profile_id(), "other");
        assert!(matches!(rx.try_recv(), Ok(StateEvent::LayoutChanged(_))));
        // unknown page falls back to the initial page
        assert_eq!(s.apply_switch(&t("default", Some("nope"))).unwrap().as_deref(), Some("home"));
        assert_eq!(s.active_profile_id(), "default");
        assert!(s.apply_switch(&t("missing", None)).is_err());
    }

    #[test]
    fn show_initial_page_resets_the_page_even_on_the_same_profile() {
        let s = state("initial");
        let mut p = s.get_profile("default").unwrap();
        let mut second = p.pages[0].clone();
        second.id = "second".into();
        p.pages.push(second);
        s.put_profile(p).unwrap();
        let t = SwitchTarget { profile_id: "default".into(), page_id: Some("second".into()), rule_id: None };
        assert_eq!(s.apply_switch(&t).unwrap().as_deref(), Some("second"));
        assert_eq!(s.show_initial_page("default").unwrap(), "home");
        assert_eq!(s.active_page_id(), "home");
        assert!(s.show_initial_page("missing").is_err());
    }

    #[test]
    fn external_rail_edit_is_reloaded_but_own_saves_are_not() {
        let s = state("railwatch");
        let mut rx = s.subscribe();
        let keys = vec![nd_core::Control::key("a", 0, 0, "A")];
        s.set_rail(keys.clone()).unwrap();
        assert!(matches!(rx.try_recv(), Ok(StateEvent::RailChanged(_))));
        assert!(!s.poll_changes(), "own save must not reload");
        assert!(rx.try_recv().is_err());
        // External edit (a different size guarantees a new stamp even on coarse mtimes).
        let root = s.read().store.root().to_path_buf();
        let two = vec![nd_core::Control::key("a", 0, 0, "A"), nd_core::Control::key("b", 0, 0, "B")];
        nd_core::save_rail(&root, &two).unwrap();
        assert!(s.poll_changes());
        match rx.try_recv() {
            Ok(StateEvent::RailChanged(r)) => assert_eq!(r.controls.len(), 2),
            other => panic!("expected RailChanged, got {other:?}"),
        }
        assert_eq!(s.rail_keys(), two);
        // Garbage keeps the rail and does not loop.
        std::fs::write(root.join("rail.json"), "not json at all").unwrap();
        assert!(!s.poll_changes());
        assert_eq!(s.rail_keys(), two);
        assert!(!s.poll_changes());
    }

    fn plugin_key(id: &str, fmt: &str) -> nd_core::Control {
        let mut settings = PluginSettings::new();
        settings.insert("format".into(), fmt.into());
        nd_core::Control::key(id, 0, 0, "Plug").with_action(nd_core::Action::Plugin {
            plugin: "p.q".into(),
            action: "a".into(),
            settings,
        })
    }

    #[test]
    fn plugin_set_settings_updates_rail_and_ignores_noops_and_other_plugins() {
        let s = state("pluginrail");
        s.set_rail(vec![plugin_key("r1", "24h")]).unwrap();
        let mut rx = s.subscribe();
        let mut new = PluginSettings::new();
        new.insert("format".into(), "12h".into());
        assert!(s.plugin_set_settings("rail:r1", "p.q", &new).unwrap());
        assert!(!s.plugin_set_settings("rail:r1", "p.q", &new).unwrap(), "same settings: nothing to do");
        assert!(!s.plugin_set_settings("rail:r1", "other.plugin", &PluginSettings::new()).unwrap());
        assert!(!s.plugin_set_settings("rail:nope", "p.q", &new).unwrap());
        assert!(!s.plugin_set_settings("garbage", "p.q", &new).unwrap());
        let nd_core::Action::Plugin { settings, .. } = &s.rail_keys()[0].actions[0].action else { panic!() };
        assert_eq!(*settings, new);
        assert_eq!(s.plugin_instances_all()[0].1.settings, new);
        assert!(rx.try_recv().is_err(), "no layout / rail push for a settings save");
        assert!(!s.poll_changes(), "own rail write is not an external edit");
        assert_eq!(s.control_for_context("rail:r1").as_deref(), Some("r1"));
        assert_eq!(s.control_for_context("rail:zz"), None);
    }

    #[test]
    fn plugin_set_settings_persists_page_controls_without_a_layout_event() {
        let s = state("pluginpage");
        let mut p = s.get_profile("default").unwrap();
        let first = p.pages[0].id.clone();
        p.get_page_mut(&first).unwrap().controls.push(plugin_key("pk", "24h"));
        s.put_profile(p).unwrap();
        let mut rx = s.subscribe();
        let mut new = PluginSettings::new();
        new.insert("format".into(), "12h".into());
        assert!(s.plugin_set_settings("default:pk", "p.q", &new).unwrap());
        let stored = s.get_profile("default").unwrap();
        let nd_core::Action::Plugin { settings, .. } =
            &stored.get_page(&first).unwrap().get_control("pk").unwrap().actions[0].action
        else {
            panic!()
        };
        assert_eq!(*settings, new);
        assert!(matches!(rx.try_recv(), Ok(StateEvent::ProfilesChanged)));
        assert!(rx.try_recv().is_err());
        assert!(!s.poll_changes());
        assert_eq!(s.control_for_context("default:pk").as_deref(), Some("pk"));
    }

    #[test]
    fn cannot_delete_active() {
        let s = state("del");
        assert!(s.delete_profile("default").is_err());
    }

    #[test]
    fn apply_live_reemits_after_layout_change() {
        let s = state("live");
        let sample = [("clock".to_string(), LiveSample { title: Some("12:34".into()), ..Default::default() })];
        // Find any control with a source on the active page to target.
        let sources = s.live_sources();
        assert!(!sources.is_empty(), "default profile has live tiles");
        let id = sources[0].0.clone();
        let sample = vec![(id, sample[0].1.clone())];
        assert!(s.apply_live(&sample).is_some());
        assert!(s.apply_live(&sample).is_none(), "unchanged sample is a no-op");
        // Profile edit resets the controls to their static values.
        let mut p = s.get_profile("default").unwrap();
        p.name = "Edited".into();
        s.put_profile(p).unwrap();
        assert!(s.apply_live(&sample).is_some(), "patch must be re-sent after a layout push");
    }
}
