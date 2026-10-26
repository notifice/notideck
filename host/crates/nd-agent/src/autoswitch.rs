// SPDX-License-Identifier: GPL-3.0-or-later
//! Auto profile switching glue (SPEC §6.3, M5): persistence, focus watching with a 200 ms
//! debounce, and applying the engine's [`SwitchTarget`]s to the deck.
//!
//! Lock order: the engine mutex is never held while calling into [`AgentState`] (which emits
//! events), and `AgentState` locks are never held while taking the engine mutex.

use crate::AgentState;
use nd_core::autoswitch::{AutoSwitch, AutoSwitchConfig, ProfileActions, RecentApps, RuleError, SwitchTarget};
use nd_core::model::ProfileTarget;
use nd_core::store::ProfileSummary;
use nd_core::PauseMode;
use nd_platform::{FocusHandle, FocusWatcher, FocusedApp};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

const FILE: &str = "autoswitch.json";
/// Focus changes closer together than this collapse into the latest one.
pub const DEBOUNCE: Duration = Duration::from_millis(200);
pub const DEBOUNCE_MAX: Duration = Duration::from_millis(600);

/// Load `autoswitch.json`; missing is silent, invalid warns. Both give the default config.
pub fn load(root: &Path) -> AutoSwitchConfig {
    match std::fs::read(root.join(FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            warn!("autoswitch.json is invalid ({e}); using defaults");
            AutoSwitchConfig::default()
        }),
        Err(_) => AutoSwitchConfig::default(),
    }
}

/// Atomic save: write a temp file, then rename over the real one.
pub fn save(root: &Path, config: &AutoSwitchConfig) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let json = serde_json::to_vec_pretty(config).map_err(std::io::Error::other)?;
    let tmp = root.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, root.join(FILE))
}

#[derive(Default)]
struct Live {
    current: Option<FocusedApp>,
    recent: RecentApps,
    /// `rule_id` of the last applied target (`None` for the fallback or before any switch).
    active_rule_id: Option<String>,
}

/// Work deferred to the `nd-autoswitch-cmd` thread: the input path holds the `AgentState` write lock
/// while it runs router actions, so those actions must never call `AgentState` themselves.
enum Cmd {
    /// Show the main profile on its initial page.
    Home,
    /// Apply a target returned by the engine (e.g. on resume).
    Apply(SwitchTarget),
    /// Round 22: switch to a profile chosen by a tile.
    Switch(ProfileTarget),
}

/// Most profile ids kept for the `back` target.
const HISTORY_CAP: usize = 32;

/// Profile list + back history, readable without the `AgentState` lock (the deck input path calls
/// [`ProfileActions`] with that lock held). The list refreshes on `ProfilesChanged`.
#[derive(Default)]
struct ProfileNav {
    profiles: Vec<ProfileSummary>,
    /// Profiles the deck left, most recent last.
    history: Vec<String>,
}

impl ProfileNav {
    fn name_of(&self, id: &str) -> Option<&str> {
        self.profiles.iter().find(|p| p.id == id).map(|p| p.name.as_str())
    }

    fn push(&mut self, id: String) {
        self.history.retain(|h| *h != id);
        self.history.push(id);
        if self.history.len() > HISTORY_CAP {
            self.history.remove(0);
        }
    }

    /// Top of the history that still exists and is not the profile on screen.
    fn back_id(&self, current: &str) -> Option<&str> {
        self.history.iter().rev().find(|h| *h != current && self.name_of(h).is_some()).map(String::as_str)
    }

    /// Profile id a target resolves to; `Err` when it cannot go anywhere.
    fn resolve(&self, to: &ProfileTarget, current: &str) -> Result<String, String> {
        match to {
            ProfileTarget::Profile { id } => {
                self.name_of(id).map(|_| id.clone()).ok_or_else(|| format!("profile '{id}' does not exist"))
            }
            ProfileTarget::Back => {
                self.back_id(current).map(str::to_string).ok_or_else(|| "there is no previous profile".to_string())
            }
            ProfileTarget::Next | ProfileTarget::Previous => {
                let n = self.profiles.len();
                if n == 0 {
                    return Err("there are no profiles".into());
                }
                let Some(i) = self.profiles.iter().position(|p| p.id == current) else {
                    return Ok(self.profiles[0].id.clone());
                };
                let j = if matches!(to, ProfileTarget::Next) { (i + 1) % n } else { (i + n - 1) % n };
                Ok(self.profiles[j].id.clone())
            }
        }
    }
}

type ChangeHook = Box<dyn Fn() + Send + Sync>;
/// Called with `(process, title)` when the foreground app or window title changed.
type FocusHook = Box<dyn Fn(&str, &str) + Send + Sync>;

pub struct AutoSwitchService {
    root: PathBuf,
    state: AgentState,
    engine: Mutex<AutoSwitch>,
    live: Mutex<Live>,
    backend: &'static str,
    available: bool,
    error: Option<String>,
    /// Keeps the focus watch alive.
    handle: Mutex<Option<FocusHandle>>,
    /// Sender of the deferred-command worker; dropped by [`AutoSwitchService::stop`].
    cmds: Mutex<Option<mpsc::Sender<Cmd>>>,
    /// Round 22: profile list and back history (see [`ProfileNav`]).
    nav: Mutex<ProfileNav>,
    /// Called when the paused state may have changed (refreshes live data).
    on_change: Mutex<Option<ChangeHook>>,
    /// Round 19: tells plugins about foreground changes.
    on_focus: Mutex<Option<FocusHook>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl AutoSwitchService {
    /// Load the config, start the focus watcher and the debounce worker. A watcher that cannot
    /// start only marks auto-switch unavailable.
    pub fn start(root: PathBuf, state: AgentState, watcher: Box<dyn FocusWatcher>) -> Arc<Self> {
        let config = load(&root);
        let known = known_ids(&state);
        let (tx, rx) = mpsc::channel::<FocusedApp>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let backend = watcher.backend_name();
        let started = watcher.start(Box::new({
            let tx = Mutex::new(tx);
            move |app| {
                let _ = lock(&tx).send(app);
            }
        }));
        let (handle, available, error) = match started {
            Ok(h) => (Some(h), true, None),
            Err(e) => {
                info!("Auto-switch unavailable ({backend}): {e}");
                (None, false, Some(e.to_string()))
            }
        };
        let svc = Arc::new(Self {
            root,
            state: state.clone(),
            engine: Mutex::new(AutoSwitch::new(config, known)),
            live: Mutex::new(Live::default()),
            backend,
            available,
            error,
            handle: Mutex::new(handle),
            cmds: Mutex::new(Some(cmd_tx)),
            nav: Mutex::new(ProfileNav { profiles: state.list_profiles().unwrap_or_default(), history: Vec::new() }),
            on_change: Mutex::new(None),
            on_focus: Mutex::new(None),
        });
        let worker = svc.clone();
        let spawned = std::thread::Builder::new().name("nd-autoswitch".into()).spawn(move || worker.run(rx));
        if let Err(e) = spawned {
            warn!("cannot start the auto-switch worker: {e}");
        }
        let worker = svc.clone();
        let spawned = std::thread::Builder::new().name("nd-autoswitch-cmd".into()).spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                worker.run_cmd(cmd);
            }
        });
        if let Err(e) = spawned {
            warn!("cannot start the auto-switch command worker: {e}");
        }
        svc
    }

    /// Stop the focus watch (the worker then exits when the channel closes).
    pub fn stop(&self) {
        lock(&self.handle).take();
        lock(&self.cmds).take();
    }

    /// Register a callback fired after the paused state may have changed.
    pub fn set_on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        *lock(&self.on_change) = Some(Box::new(f));
    }

    /// Register a callback fired with `(process, title)` whenever the focused window changed.
    pub fn set_on_focus(&self, f: impl Fn(&str, &str) + Send + Sync + 'static) {
        *lock(&self.on_focus) = Some(Box::new(f));
    }

    fn changed(&self) {
        if let Some(f) = lock(&self.on_change).as_ref() {
            f();
        }
    }

    fn send(&self, cmd: Cmd) -> Result<(), String> {
        match lock(&self.cmds).as_ref() {
            Some(tx) => tx.send(cmd).map_err(|_| "auto-switch is stopped".to_string()),
            None => Err("auto-switch is stopped".to_string()),
        }
    }

    fn run_cmd(&self, cmd: Cmd) {
        match cmd {
            Cmd::Home => match self.main_profile_id() {
                Some(id) => {
                    let before = self.state.active_profile_id();
                    match self.state.show_initial_page(&id) {
                        Ok(page) => info!(profile = %id, page = %page, "main menu"),
                        Err(e) => warn!("cannot show the main profile '{id}': {e}"),
                    }
                    self.record_change(before);
                }
                None => warn!("no profile available for the main menu"),
            },
            Cmd::Switch(to) => self.switch(&to),
            Cmd::Apply(t) => self.apply_opt(Some(t)),
        }
    }

    /// Push `before` onto the back history when the active profile is no longer that one.
    fn record_change(&self, before: String) {
        if self.state.active_profile_id() != before {
            lock(&self.nav).push(before);
        }
    }

    /// Worker side of `switch_profile`: resolve against fresh data, show the initial page, keep the history.
    fn switch(&self, to: &ProfileTarget) {
        self.refresh_known_profiles();
        let current = self.state.active_profile_id();
        let target = lock(&self.nav).resolve(to, &current);
        let id = match target {
            Ok(id) => id,
            Err(e) => {
                warn!("cannot switch profile: {e}");
                return;
            }
        };
        if id == current && !matches!(to, ProfileTarget::Profile { .. }) {
            return; // next / previous with a single profile: stay
        }
        match self.state.show_initial_page(&id) {
            Ok(page) => {
                info!(profile = %id, page = %page, "profile switch");
                let mut nav = lock(&self.nav);
                if matches!(to, ProfileTarget::Back) {
                    // pop the entry we went back to (and anything above it that was skipped)
                    if let Some(i) = nav.history.iter().rposition(|h| *h == id) {
                        nav.history.truncate(i);
                    }
                } else if id != current {
                    nav.push(current);
                }
            }
            Err(e) => warn!("cannot switch to profile '{id}': {e}"),
        }
    }

    /// Main profile: `home_profile_id`, else `fallback_profile_id`, else the first stored profile,
    /// each only if it exists.
    pub fn main_profile_id(&self) -> Option<String> {
        let (home, fallback) = {
            let e = lock(&self.engine);
            (e.config().home_profile_id.clone(), e.config().fallback_profile_id.clone())
        };
        let ids = known_ids(&self.state);
        [home, fallback].into_iter().flatten().find(|id| ids.contains(id)).or_else(|| ids.into_iter().next())
    }

    /// Debounce loop: wait for an event, then keep taking newer ones until 200 ms of quiet, but never
    /// longer than [`DEBOUNCE_MAX`] (a window whose title keeps changing must not hold the switch back).
    fn run(&self, rx: mpsc::Receiver<FocusedApp>) {
        while let Ok(mut app) = rx.recv() {
            let give_up = std::time::Instant::now() + DEBOUNCE_MAX;
            loop {
                let left = give_up.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    break;
                }
                match rx.recv_timeout(DEBOUNCE.min(left)) {
                    Ok(next) => app = next,
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        self.handle_focus(app);
                        return;
                    }
                }
            }
            self.handle_focus(app);
        }
    }

    fn handle_focus(&self, app: FocusedApp) {
        let focus_changed = {
            let mut live = lock(&self.live);
            live.recent.push(&app);
            let changed = live.current.as_ref().is_none_or(|c| c.process != app.process || c.title != app.title);
            live.current = Some(app.clone());
            changed
        };
        if focus_changed {
            if let Some(f) = lock(&self.on_focus).as_ref() {
                f(&app.process, &app.title);
            }
        }
        let target = lock(&self.engine).on_focus(app.clone());
        if let Some(t) = target {
            self.apply(&t, Some(&app.process));
        }
    }

    fn apply(&self, target: &SwitchTarget, process: Option<&str>) {
        lock(&self.live).active_rule_id = target.rule_id.clone();
        let before = self.state.active_profile_id();
        let res = self.state.apply_switch(target);
        self.record_change(before);
        match res {
            Ok(Some(page)) => info!(
                process = process.unwrap_or(""),
                rule_id = target.rule_id.as_deref().unwrap_or("fallback"),
                profile = %target.profile_id,
                page = %page,
                "auto-switch"
            ),
            Ok(None) => {}
            Err(e) => warn!("auto-switch to profile '{}' failed: {e}", target.profile_id),
        }
    }

    fn apply_opt(&self, target: Option<SwitchTarget>) {
        if let Some(t) = target {
            let process = lock(&self.live).current.as_ref().map(|a| a.process.clone());
            self.apply(&t, process.as_deref());
        }
    }

    /// Process name of the focused window, if the focus backend has reported one (mixer "focused app").
    pub fn focused_process(&self) -> Option<String> {
        lock(&self.live).current.as_ref().map(|a| a.process.clone())
    }

    /// Refresh the engine's profile list (call on `ProfilesChanged`).
    pub fn refresh_known_profiles(&self) {
        let profiles = self.state.list_profiles().unwrap_or_default();
        let ids = profiles.iter().map(|p| p.id.clone()).collect();
        lock(&self.engine).set_known_profiles(ids);
        lock(&self.nav).profiles = profiles;
    }

    /// Validate, persist and apply a new config. `Err(errors)` are validation problems.
    pub fn put_config(&self, config: AutoSwitchConfig) -> Result<(), PutError> {
        let errors = config.validate(&known_ids(&self.state));
        if !errors.is_empty() {
            return Err(PutError::Invalid(errors));
        }
        save(&self.root, &config).map_err(|e| PutError::Io(format!("cannot save autoswitch.json: {e}")))?;
        let target = lock(&self.engine).set_config(config);
        self.apply_opt(target);
        Ok(())
    }

    pub fn set_locked(&self, locked: bool) {
        let target = lock(&self.engine).set_locked(locked);
        self.apply_opt(target);
        self.changed();
    }

    /// Body of `GET /api/autoswitch`.
    pub fn status_json(&self) -> serde_json::Value {
        let (config, locked) = {
            let e = lock(&self.engine);
            (e.config().clone(), e.locked())
        };
        let live = lock(&self.live);
        let mut v = serde_json::json!({
            "config": config,
            "locked": locked,
            "backend": self.backend,
            "available": self.available,
            "recent": live.recent.list(),
        });
        if let Some(e) = &self.error {
            v["error"] = e.clone().into();
        }
        if let Some(c) = &live.current {
            v["current"] = serde_json::json!(c);
        }
        if let Some(r) = &live.active_rule_id {
            v["active_rule_id"] = r.clone().into();
        }
        v
    }
}

/// Used by the deck's input path, which holds the `AgentState` lock: profile changes are queued to
/// the worker thread and these methods return immediately.
impl ProfileActions for AutoSwitchService {
    fn go_home(&self, pause: bool) -> Result<(), String> {
        if pause {
            lock(&self.engine).set_locked(true);
        }
        let queued = self.send(Cmd::Home);
        if pause {
            self.changed();
        }
        queued
    }

    fn set_paused(&self, mode: PauseMode) -> Result<bool, String> {
        let (paused, target) = {
            let mut e = lock(&self.engine);
            let want = match mode {
                PauseMode::Toggle => !e.locked(),
                PauseMode::Pause => true,
                PauseMode::Resume => false,
            };
            (want, e.set_locked(want))
        };
        if let Some(t) = target {
            self.send(Cmd::Apply(t))?;
        }
        self.changed();
        Ok(paused)
    }

    fn paused(&self) -> bool {
        lock(&self.engine).locked()
    }

    fn switch_profile(&self, to: &ProfileTarget) -> Result<(), String> {
        let current = self.state.active_profile_id_nolock();
        lock(&self.nav).resolve(to, &current)?;
        self.send(Cmd::Switch(to.clone()))
    }

    fn destination(&self, to: &ProfileTarget) -> Option<(String, bool)> {
        let current = self.state.active_profile_id_nolock();
        let nav = lock(&self.nav);
        let id = nav.resolve(to, &current).ok()?;
        Some((nav.name_of(&id)?.to_string(), id == current))
    }

    fn current_profile_name(&self) -> Option<String> {
        let current = self.state.active_profile_id_nolock();
        lock(&self.nav).name_of(&current).map(str::to_string)
    }
}

#[derive(Debug)]
pub enum PutError {
    Invalid(Vec<RuleError>),
    Io(String),
}

fn known_ids(state: &AgentState) -> Vec<String> {
    state.list_profiles().unwrap_or_default().into_iter().map(|p| p.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_invalid_files_give_defaults() {
        let dir = std::env::temp_dir().join(format!("nd-as-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&dir), AutoSwitchConfig::default());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), b"{nope").unwrap();
        assert_eq!(load(&dir), AutoSwitchConfig::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("nd-as-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = AutoSwitchConfig {
            enabled: false,
            fallback_profile_id: Some("p".into()),
            rules: vec![],
            home_profile_id: None,
        };
        save(&dir, &cfg).unwrap();
        assert_eq!(load(&dir), cfg);
        assert!(!dir.join("autoswitch.json.tmp").exists());
    }

    use nd_core::autoswitch::{MatchKind, Rule};
    use nd_core::ProfileStore;

    type Cb = Box<dyn Fn(FocusedApp) + Send + Sync>;
    struct FakeFocus(Mutex<Option<Cb>>);
    struct Shared(Arc<FakeFocus>);
    impl FocusWatcher for Shared {
        fn backend_name(&self) -> &'static str {
            "fake"
        }
        fn start(&self, cb: Box<dyn Fn(FocusedApp) + Send + Sync>) -> nd_platform::Result<FocusHandle> {
            *self.0 .0.lock().unwrap() = Some(cb);
            Ok(FocusHandle(Box::new(())))
        }
    }

    fn app(p: &str) -> FocusedApp {
        FocusedApp { process: p.into(), class: String::new(), title: String::new(), pid: None }
    }

    #[test]
    fn debounced_focus_switches_profile() {
        let dir = std::env::temp_dir().join(format!("nd-as-svc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = AgentState::open(ProfileStore::new(dir.clone())).unwrap();
        let mut p = state.get_profile("default").unwrap();
        p.id = "other".into();
        state.put_profile(p).unwrap();
        let fake = Arc::new(FakeFocus(Mutex::new(None)));
        let svc = AutoSwitchService::start(dir, state.clone(), Box::new(Shared(fake.clone())));
        assert!(svc.status_json()["available"].as_bool().unwrap());
        let cfg = AutoSwitchConfig {
            home_profile_id: None,
            enabled: true,
            fallback_profile_id: None,
            rules: vec![Rule {
                id: "r1".into(),
                enabled: true,
                kind: MatchKind::Process,
                pattern: "obs64".into(),
                profile_id: "other".into(),
                page_id: None,
            }],
        };
        svc.put_config(cfg).unwrap();
        let emit = fake.0.lock().unwrap().take().unwrap();
        // a burst: only the last event counts
        emit(app("obs64.exe"));
        emit(app("notepad.exe"));
        emit(app("obs64.exe"));
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(state.active_profile_id(), "other");
        let v = svc.status_json();
        assert_eq!(v["active_rule_id"], "r1");
        assert_eq!(v["current"]["process"], "obs64.exe");
        assert_eq!(v["recent"].as_array().unwrap().len(), 1, "burst collapsed into one event");
        // invalid config -> errors, nothing saved
        let bad = AutoSwitchConfig {
            enabled: true,
            fallback_profile_id: Some("nope".into()),
            rules: vec![],
            home_profile_id: None,
        };
        assert!(matches!(svc.put_config(bad), Err(PutError::Invalid(e)) if !e.is_empty()));
    }

    fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
        for _ in 0..100 {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}");
    }

    fn press(state: &AgentState, router: &nd_core::ActionRouter, control: &str) -> nd_core::InputReaction {
        let ev = nd_proto::v1::InputEvent {
            page_id: String::new(),
            control_id: control.into(),
            kind: nd_proto::v1::InputKind::KeyDown as i32,
            delta: 0,
            value: 0.0,
            phone_time_ms: 0,
        };
        state.handle_input(&ev, router).unwrap()
    }

    /// Presses deck keys through the real input path (which holds the AgentState write lock while the
    /// router runs): the profile change must be deferred, not deadlock.
    #[test]
    fn main_menu_and_pause_keys_through_the_input_path() {
        use nd_core::{Action, Control, Page, PauseMode};
        let dir = std::env::temp_dir().join(format!("nd-as-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = AgentState::open(ProfileStore::new(dir.clone())).unwrap();
        let mut main = state.get_profile("default").unwrap();
        main.id = "main".into();
        let mut second = Page::new("second", "Second");
        second.add_control(Control::key("home", 0, 0, "Main").with_action(Action::GoHome { pause_auto_switch: false }));
        second
            .add_control(Control::key("homep", 1, 0, "Main+").with_action(Action::GoHome { pause_auto_switch: true }));
        second.add_control(
            Control::key("pause", 2, 0, "Pause").with_action(Action::AutoSwitchPause { mode: PauseMode::Toggle }),
        );
        let mut other = main.clone();
        other.id = "other".into();
        other.pages.push(second);
        state.put_profile(main).unwrap();
        state.put_profile(other).unwrap();

        let fake = Arc::new(FakeFocus(Mutex::new(None)));
        let svc = AutoSwitchService::start(dir, state.clone(), Box::new(Shared(fake.clone())));
        let changes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = changes.clone();
        svc.set_on_change(move || {
            c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let (injector, launcher) = (nd_core::mock::MockInputInjector::new(), nd_core::mock::MockLauncher::new());
        let router =
            nd_core::ActionRouter::new(Arc::new(injector), Arc::new(launcher)).with_profile_actions(svc.clone());
        let t = |p: &str, page: Option<&str>| SwitchTarget {
            profile_id: p.into(),
            page_id: page.map(Into::into),
            rule_id: None,
        };

        // main profile resolution: fallback missing -> first stored profile ("default"), then explicit ones
        assert_eq!(svc.main_profile_id().as_deref(), Some("default"));
        let mut cfg = AutoSwitchConfig { fallback_profile_id: Some("other".into()), ..Default::default() };
        svc.put_config(cfg.clone()).unwrap();
        assert_eq!(svc.main_profile_id().as_deref(), Some("other"));
        cfg.home_profile_id = Some("main".into());
        svc.put_config(cfg).unwrap();
        assert_eq!(svc.main_profile_id().as_deref(), Some("main"));

        // go home from a non-initial page of another profile, without pausing
        state.apply_switch(&t("other", Some("second"))).unwrap();
        press(&state, &router, "home");
        wait_for("main profile", || state.active_profile_id() == "main");
        assert_eq!(state.active_page_id(), "home");
        assert!(!svc.paused());

        // with pause: locked, and the already-active profile goes back to its initial page
        state.apply_switch(&t("other", Some("second"))).unwrap();
        press(&state, &router, "homep");
        wait_for("main profile again", || state.active_profile_id() == "main");
        assert!(svc.paused());
        assert!(changes.load(std::sync::atomic::Ordering::SeqCst) >= 1);

        // pause toggle key: state_index follows the paused state
        state.apply_switch(&t("other", Some("second"))).unwrap();
        press(&state, &router, "pause"); // resumes (was paused)
        assert!(!svc.paused());
        press(&state, &router, "pause");
        assert!(svc.paused());
        assert_eq!(svc.set_paused(PauseMode::Resume), Ok(false));

        // resume re-applies the focused app's profile
        let emit = fake.0.lock().unwrap().take();
        if let Some(emit) = emit {
            let mut cfg = svc.status_json()["config"].clone();
            cfg["rules"] =
                serde_json::json!([{"id":"r1","enabled":true,"kind":"process","pattern":"obs64","profile_id":"other"}]);
            svc.put_config(serde_json::from_value(cfg).unwrap()).unwrap();
            emit(app("obs64.exe"));
            wait_for("auto switch to other", || state.active_profile_id() == "other");
            press(&state, &router, "homep");
            wait_for("home while paused", || state.active_profile_id() == "main");
            assert_eq!(svc.set_paused(PauseMode::Resume), Ok(false));
            wait_for("resume switches back", || state.active_profile_id() == "other");
        }
        svc.stop();
        assert!(svc.go_home(false).is_err(), "stopped service reports an error instead of hanging");
    }

    fn switch_fixture(tag: &str, ids: &[&str]) -> (AgentState, Arc<AutoSwitchService>) {
        let dir = std::env::temp_dir().join(format!("nd-as-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = AgentState::open(ProfileStore::new(dir.clone())).unwrap();
        for id in ids {
            let mut p = state.get_profile("default").unwrap();
            p.id = (*id).into();
            p.name = format!("Name {id}");
            state.put_profile(p).unwrap();
        }
        let fake = Arc::new(FakeFocus(Mutex::new(None)));
        let svc = AutoSwitchService::start(dir, state.clone(), Box::new(Shared(fake)));
        svc.refresh_known_profiles();
        (state, svc)
    }

    fn go(svc: &AutoSwitchService, state: &AgentState, to: ProfileTarget, expect: &str) {
        svc.switch_profile(&to).unwrap();
        wait_for(expect, || state.active_profile_id() == expect);
    }

    #[test]
    fn switch_next_previous_wrap_and_unknown_id() {
        let (state, svc) = switch_fixture("sw-wrap", &["pa", "pb"]);
        let order: Vec<String> = state.list_profiles().unwrap().into_iter().map(|p| p.id).collect();
        assert!(order.len() >= 3);
        let (first, second, last) = (order[0].clone(), order[1].clone(), order[order.len() - 1].clone());
        go(&svc, &state, ProfileTarget::Profile { id: first.clone() }, &first);
        assert_eq!(svc.current_profile_name().unwrap(), state.list_profiles().unwrap()[0].name);
        go(&svc, &state, ProfileTarget::Next, &second);
        go(&svc, &state, ProfileTarget::Previous, &first);
        go(&svc, &state, ProfileTarget::Previous, &last); // wraps
        go(&svc, &state, ProfileTarget::Next, &first); // wraps back
        let err = svc.switch_profile(&ProfileTarget::Profile { id: "nope".into() });
        assert!(err.is_err());
        assert_eq!(svc.destination(&ProfileTarget::Profile { id: "nope".into() }), None);
        assert_eq!(state.active_profile_id(), first);
        svc.stop();
    }

    #[test]
    fn single_profile_stays_put() {
        let (state, svc) = switch_fixture("sw-one", &[]);
        let only = state.active_profile_id();
        let name = svc.current_profile_name().unwrap();
        assert_eq!(svc.destination(&ProfileTarget::Next), Some((name.clone(), true)));
        assert_eq!(svc.destination(&ProfileTarget::Previous), Some((name, true)));
        svc.switch_profile(&ProfileTarget::Next).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(state.active_profile_id(), only);
        assert!(svc.destination(&ProfileTarget::Back).is_none());
        assert!(svc.switch_profile(&ProfileTarget::Back).is_err());
        svc.stop();
    }

    #[test]
    fn back_walks_the_history_and_skips_deleted_profiles() {
        let (state, svc) = switch_fixture("sw-back", &["pa", "pb", "pc"]);
        let p = |id: &str| ProfileTarget::Profile { id: id.into() };
        go(&svc, &state, p("pa"), "pa");
        go(&svc, &state, p("pb"), "pb");
        go(&svc, &state, p("pc"), "pc");
        // destination has no side effects, however often it is asked
        for _ in 0..3 {
            let (name, here) = svc.destination(&ProfileTarget::Back).unwrap();
            assert_eq!((name.as_str(), here), ("Name pb", false));
        }
        assert_eq!(lock(&svc.nav).history, vec!["default", "pa", "pb"]);
        assert_eq!(state.active_profile_id(), "pc");
        assert_eq!(svc.destination(&p("pc")), Some(("Name pc".into(), true)));
        // delete the profile one step back: it is skipped (history is [default, pa, pb])
        state.delete_profile("pb").unwrap();
        svc.refresh_known_profiles();
        assert_eq!(svc.destination(&ProfileTarget::Back).unwrap().0, "Name pa");
        // a 3-step walk back, then nowhere to go
        go(&svc, &state, ProfileTarget::Back, "pa");
        go(&svc, &state, ProfileTarget::Back, "default");
        assert!(svc.destination(&ProfileTarget::Back).is_none());
        assert!(svc.switch_profile(&ProfileTarget::Back).is_err());
        // going back did not push: a fresh switch starts a new history
        go(&svc, &state, p("pc"), "pc");
        assert_eq!(
            svc.destination(&ProfileTarget::Back).unwrap().0,
            state.list_profiles().unwrap().iter().find(|x| x.id == "default").unwrap().name
        );
        svc.stop();
    }

    #[test]
    fn home_and_auto_switch_are_recorded_in_the_history() {
        let (state, svc) = switch_fixture("sw-rec", &["pa", "pb"]);
        let p = |id: &str| ProfileTarget::Profile { id: id.into() };
        go(&svc, &state, p("pb"), "pb");
        // Home (main profile = first stored) records pb
        svc.go_home(false).unwrap();
        let main = svc.main_profile_id().unwrap();
        // the worker sets the active profile first and records the history right after: wait for both
        wait_for("home", || {
            state.active_profile_id() == main && lock(&svc.nav).history.last().map(String::as_str) == Some("pb")
        });
        // auto-switch Apply records the profile it left
        svc.apply(&SwitchTarget { profile_id: "pa".into(), page_id: None, rule_id: None }, None);
        assert_eq!(state.active_profile_id(), "pa");
        assert_eq!(lock(&svc.nav).history.last(), Some(&main));
        // switching to the already-active profile pushes nothing
        let len = lock(&svc.nav).history.len();
        go(&svc, &state, p("pa"), "pa");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(lock(&svc.nav).history.len(), len);
        svc.stop();
    }
}
