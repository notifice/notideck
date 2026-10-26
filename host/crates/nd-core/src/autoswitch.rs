// SPDX-License-Identifier: GPL-3.0-or-later
//! Auto profile switching (SPEC §6.3, M5): rules that map the focused app to a profile.
//!
//! Pure logic. The agent feeds [`FocusedApp`] values (already debounced) into [`AutoSwitch::on_focus`]
//! and applies the returned [`SwitchTarget`]. Persistence (`autoswitch.json`) lives in the agent.
//!
//! CONTRACT (round 8, written by the lead; R8-1 implements the bodies):
//! - Rules are ordered; the first enabled rule that matches wins.
//! - Matching ([`Rule::matches`]):
//!   - `Process`: case-insensitive equality on the image name; a trailing `.exe` is ignored on both
//!     sides, so `obs64` matches `OBS64.EXE`.
//!   - `Class`: case-insensitive equality.
//!   - `Title`: regex search (not full match) with the `regex` crate; the user adds `(?i)` if wanted.
//!     Invalid patterns are rejected by [`AutoSwitchConfig::validate`] and never match.
//!   - An empty pattern never matches.
//! - [`AutoSwitch::on_focus`]:
//!   - Ignored (returns `None`, state unchanged) while `!config.enabled`, while locked, and for
//!     Notideck's own windows (process `notideck`/`notideck.exe`/`nd-agent`/`nd-agent.exe`).
//!   - Target = first matching rule's profile/page, else `fallback_profile_id` (page `None`), else none.
//!   - Returns `Some(target)` only when the target differs from the last one it returned (so a manual
//!     profile change sticks until focus moves to an app with a different target). A `None` target
//!     returns `None` and forgets the last target, so returning to a matched app switches again.
//!   - Rules pointing at a profile id not in `known_profiles` are skipped.
//! - Lock: [`AutoSwitch::set_locked`]`(false)` on a locked engine forgets the last target and re-evaluates
//!   the last focused app (resume always re-applies its profile); otherwise it returns `None`, as does
//!   `set_locked(true)`.
//! - [`AutoSwitch::set_config`] replaces the config, forgets the last target and re-evaluates the last
//!   focused app (returns its target, if any).
//! - Page restore: when a target has `page_id: None`, the agent shows the page last used in that
//!   profile during this run (see [`PageMemory`]), else the profile's initial page.

use nd_platform::FocusedApp;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Processes belonging to Notideck itself; focus on them never triggers a switch.
const OWN_PROCESSES: [&str; 2] = ["notideck", "nd-agent"];

fn strip_exe(s: &str) -> String {
    let l = s.to_lowercase();
    match l.strip_suffix(".exe") {
        Some(x) => x.to_string(),
        None => l,
    }
}

fn is_own(app: &FocusedApp) -> bool {
    let p = strip_exe(&app.process);
    OWN_PROCESSES.contains(&p.as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Process,
    Class,
    Title,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Stable id (UUID or slug) chosen by the UI.
    pub id: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub kind: MatchKind,
    pub pattern: String,
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_id: Option<String>,
}

fn yes() -> bool {
    true
}

impl Rule {
    pub fn matches(&self, app: &FocusedApp) -> bool {
        let re = if self.kind == MatchKind::Title { Regex::new(&self.pattern).ok() } else { None };
        self.matches_with(app, re.as_ref())
    }

    /// Matching with an optional precompiled regex (used for `Title`).
    fn matches_with(&self, app: &FocusedApp, re: Option<&Regex>) -> bool {
        if self.pattern.is_empty() {
            return false;
        }
        match self.kind {
            MatchKind::Process => strip_exe(&app.process) == strip_exe(&self.pattern),
            MatchKind::Class => app.class.to_lowercase() == self.pattern.to_lowercase(),
            MatchKind::Title => re.is_some_and(|r| r.is_match(&app.title)),
        }
    }
}

/// Persisted as `autoswitch.json` in the config directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoSwitchConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Profile shown when no rule matches. `None` = stay on the current profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_profile_id: Option<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Profile for the deck's "Main menu" key ([`crate::Action::GoHome`]). `None` = the fallback
    /// profile, else the first profile in the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_profile_id: Option<String>,
}

impl Default for AutoSwitchConfig {
    fn default() -> Self {
        Self { enabled: true, fallback_profile_id: None, rules: Vec::new(), home_profile_id: None }
    }
}

/// One problem found by [`AutoSwitchConfig::validate`]; `rule_id` is `None` for the fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleError {
    pub rule_id: Option<String>,
    pub message: String,
}

impl AutoSwitchConfig {
    /// Empty patterns, invalid regexes, unknown profile ids (rules and fallback), duplicate rule ids.
    pub fn validate(&self, known_profiles: &[String]) -> Vec<RuleError> {
        let mut errs = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for r in &self.rules {
            let mut err = |m: String| errs.push(RuleError { rule_id: Some(r.id.clone()), message: m });
            if seen.contains(&r.id.as_str()) {
                err(format!("duplicate rule id '{}'", r.id));
            }
            seen.push(&r.id);
            if r.pattern.is_empty() {
                err("pattern is empty".to_string());
            } else if r.kind == MatchKind::Title {
                if let Err(e) = Regex::new(&r.pattern) {
                    err(format!("invalid regex: {e}"));
                }
            }
            if !known_profiles.iter().any(|p| p == &r.profile_id) {
                err(format!("unknown profile '{}'", r.profile_id));
            }
        }
        if let Some(f) = &self.fallback_profile_id {
            if !known_profiles.iter().any(|p| p == f) {
                errs.push(RuleError { rule_id: None, message: format!("unknown fallback profile '{f}'") });
            }
        }
        if let Some(h) = &self.home_profile_id {
            if !known_profiles.iter().any(|p| p == h) {
                errs.push(RuleError { rule_id: None, message: format!("unknown main profile '{h}'") });
            }
        }
        errs
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchTarget {
    pub profile_id: String,
    pub page_id: Option<String>,
    /// The matching rule, `None` for the fallback.
    pub rule_id: Option<String>,
}

/// The switching engine. Not thread-safe by itself; the agent keeps it behind a mutex.
pub struct AutoSwitch {
    config: AutoSwitchConfig,
    known_profiles: Vec<String>,
    locked: bool,
    last_app: Option<FocusedApp>,
    last_target: Option<SwitchTarget>,
    /// Compiled `Title` regexes, parallel to `config.rules` (`None` = not a title rule or invalid).
    regexes: Vec<Option<Regex>>,
}

fn compile(config: &AutoSwitchConfig) -> Vec<Option<Regex>> {
    config
        .rules
        .iter()
        .map(|r| if r.kind == MatchKind::Title && !r.pattern.is_empty() { Regex::new(&r.pattern).ok() } else { None })
        .collect()
}

impl AutoSwitch {
    pub fn new(config: AutoSwitchConfig, known_profiles: Vec<String>) -> Self {
        let regexes = compile(&config);
        Self { config, known_profiles, locked: false, last_app: None, last_target: None, regexes }
    }

    fn known(&self, id: &str) -> bool {
        self.known_profiles.iter().any(|p| p == id)
    }

    /// Target for `app` ignoring enabled/locked/dedupe state.
    fn target_for(&self, app: &FocusedApp) -> Option<SwitchTarget> {
        for (i, r) in self.config.rules.iter().enumerate() {
            if r.enabled && self.known(&r.profile_id) && r.matches_with(app, self.regexes[i].as_ref()) {
                return Some(SwitchTarget {
                    profile_id: r.profile_id.clone(),
                    page_id: r.page_id.clone(),
                    rule_id: Some(r.id.clone()),
                });
            }
        }
        let f = self.config.fallback_profile_id.as_ref()?;
        self.known(f).then(|| SwitchTarget { profile_id: f.clone(), page_id: None, rule_id: None })
    }

    /// Evaluate the last focused app, returning a target only if it differs from the last returned one.
    fn reevaluate(&mut self) -> Option<SwitchTarget> {
        if !self.config.enabled || self.locked {
            return None;
        }
        let Some(t) = self.target_for(self.last_app.as_ref()?) else {
            // No target here: forget the last one, so coming back to a matched app switches again.
            self.last_target = None;
            return None;
        };
        if self.last_target.as_ref() == Some(&t) {
            return None;
        }
        self.last_target = Some(t.clone());
        Some(t)
    }

    pub fn config(&self) -> &AutoSwitchConfig {
        &self.config
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    pub fn last_app(&self) -> Option<&FocusedApp> {
        self.last_app.as_ref()
    }

    pub fn set_known_profiles(&mut self, ids: Vec<String>) {
        self.known_profiles = ids;
    }

    pub fn set_config(&mut self, config: AutoSwitchConfig) -> Option<SwitchTarget> {
        self.regexes = compile(&config);
        self.config = config;
        self.last_target = None;
        self.reevaluate()
    }

    pub fn set_locked(&mut self, locked: bool) -> Option<SwitchTarget> {
        let resuming = self.locked && !locked;
        self.locked = locked;
        if !resuming {
            return None;
        }
        // Resume always re-applies the focused app's profile, even the one shown before the pause.
        self.last_target = None;
        self.reevaluate()
    }

    pub fn on_focus(&mut self, app: FocusedApp) -> Option<SwitchTarget> {
        if is_own(&app) {
            return None;
        }
        // Remember it even while disabled/locked so a later unlock/enable can re-evaluate.
        self.last_app = Some(app);
        self.reevaluate()
    }
}

/// Last page shown per profile during this run, for page restore.
#[derive(Debug, Default)]
pub struct PageMemory(HashMap<String, String>);

impl PageMemory {
    pub fn remember(&mut self, profile_id: &str, page_id: &str) {
        self.0.insert(profile_id.to_string(), page_id.to_string());
    }
    pub fn page_for(&self, profile_id: &str) -> Option<&str> {
        self.0.get(profile_id).map(String::as_str)
    }
}

/// Bounded list of recently focused distinct apps (newest first), for the UI's "pick an app" helper.
#[derive(Debug, Default)]
pub struct RecentApps {
    items: Vec<FocusedApp>,
}

impl RecentApps {
    pub const MAX: usize = 12;

    /// Dedupes by (process, class), keeping the newest title; skips apps with an empty process and class.
    pub fn push(&mut self, app: &FocusedApp) {
        if app.process.is_empty() && app.class.is_empty() {
            return;
        }
        self.items.retain(|a| !(a.process == app.process && a.class == app.class));
        self.items.insert(0, app.clone());
        self.items.truncate(Self::MAX);
    }

    pub fn list(&self) -> &[FocusedApp] {
        &self.items
    }
}

/// Deck actions that reach the agent's auto-switch service (round 9). Implementations must not
/// block on the agent state lock held by the input path (defer the profile change if needed).
pub trait ProfileActions: Send + Sync {
    /// Show the main profile; `pause` also pauses auto-switch.
    fn go_home(&self, pause: bool) -> Result<(), String>;
    /// Applies `mode`, returns whether auto-switch is now paused.
    fn set_paused(&self, mode: crate::model::PauseMode) -> Result<bool, String>;
    fn paused(&self) -> bool;
    /// Round 22: switch profiles (queued, returns at once). `Err` when the target doesn't exist / no history.
    fn switch_profile(&self, _to: &crate::model::ProfileTarget) -> Result<(), String> {
        Err("profile switching is not available".into())
    }
    /// Round 22: `(profile name, is the profile on screen)` the target resolves to now; `None` = nowhere.
    fn destination(&self, _to: &crate::model::ProfileTarget) -> Option<(String, bool)> {
        None
    }
    /// Round 22: name of the profile on screen.
    fn current_profile_name(&self) -> Option<String> {
        None
    }
}

/// Default when the agent has no auto-switch service: actions fail.
pub struct NullProfileActions;

impl ProfileActions for NullProfileActions {
    fn go_home(&self, _: bool) -> Result<(), String> {
        Err("auto-switch is not available".into())
    }
    fn set_paused(&self, _: crate::model::PauseMode) -> Result<bool, String> {
        Err("auto-switch is not available".into())
    }
    fn paused(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(process: &str, class: &str, title: &str) -> FocusedApp {
        FocusedApp { process: process.into(), class: class.into(), title: title.into(), pid: None }
    }
    fn rule(id: &str, kind: MatchKind, pattern: &str, profile: &str) -> Rule {
        Rule { id: id.into(), enabled: true, kind, pattern: pattern.into(), profile_id: profile.into(), page_id: None }
    }
    fn profiles() -> Vec<String> {
        ["a", "b", "c", "fb"].iter().map(|s| s.to_string()).collect()
    }
    fn engine(rules: Vec<Rule>, fallback: Option<&str>) -> AutoSwitch {
        AutoSwitch::new(
            AutoSwitchConfig {
                enabled: true,
                fallback_profile_id: fallback.map(String::from),
                rules,
                home_profile_id: None,
            },
            profiles(),
        )
    }

    #[test]
    fn process_matching() {
        let r = rule("r", MatchKind::Process, "obs64", "a");
        assert!(r.matches(&app("OBS64.EXE", "", "")));
        assert!(r.matches(&app("obs64", "", "")));
        let r = rule("r", MatchKind::Process, "OBS64.exe", "a");
        assert!(r.matches(&app("obs64", "", "")));
        assert!(r.matches(&app("obs64.exe", "", "")));
        assert!(!r.matches(&app("obs", "", "")));
        assert!(!r.matches(&app("xobs64", "", "")));
    }

    #[test]
    fn class_matching() {
        let r = rule("r", MatchKind::Class, "Firefox", "a");
        assert!(r.matches(&app("", "firefox", "")));
        assert!(!r.matches(&app("firefox", "other", "")));
        // Class does not strip .exe.
        let r = rule("r", MatchKind::Class, "x", "a");
        assert!(!r.matches(&app("", "x.exe", "")));
    }

    #[test]
    fn title_regex_search() {
        let r = rule("r", MatchKind::Title, "Project \\d+", "a");
        assert!(r.matches(&app("", "", "Edit - Project 42 - IDE")));
        assert!(!r.matches(&app("", "", "project 42")));
        let r = rule("r", MatchKind::Title, "(?i)project", "a");
        assert!(r.matches(&app("", "", "My PROJECT")));
        let r = rule("r", MatchKind::Title, "^abc$", "a");
        assert!(!r.matches(&app("", "", "xabc")));
    }

    #[test]
    fn empty_pattern_never_matches() {
        for k in [MatchKind::Process, MatchKind::Class, MatchKind::Title] {
            let r = rule("r", k, "", "a");
            assert!(!r.matches(&app("", "", "")));
            assert!(!r.matches(&app("p", "c", "t")));
        }
    }

    #[test]
    fn invalid_regex_never_matches_and_validates() {
        let r = rule("bad", MatchKind::Title, "(unclosed", "a");
        assert!(!r.matches(&app("", "", "(unclosed")));
        let cfg = AutoSwitchConfig { enabled: true, fallback_profile_id: None, rules: vec![r], home_profile_id: None };
        let errs = cfg.validate(&profiles());
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].rule_id.as_deref(), Some("bad"));
        assert!(errs[0].message.contains("regex"));
        let mut e = AutoSwitch::new(cfg, profiles());
        assert_eq!(e.on_focus(app("", "", "(unclosed")), None);
    }

    #[test]
    fn validate_other_problems() {
        let cfg = AutoSwitchConfig {
            home_profile_id: None,
            enabled: true,
            fallback_profile_id: Some("nope".into()),
            rules: vec![
                rule("1", MatchKind::Process, "", "a"),
                rule("1", MatchKind::Process, "x", "a"),
                rule("2", MatchKind::Class, "x", "ghost"),
                rule("3", MatchKind::Class, "x", "a"),
            ],
        };
        let errs = cfg.validate(&profiles());
        assert_eq!(errs.iter().filter(|e| e.message.contains("empty")).count(), 1);
        assert_eq!(errs.iter().filter(|e| e.message.contains("duplicate")).count(), 1);
        assert_eq!(errs.iter().filter(|e| e.rule_id.as_deref() == Some("2")).count(), 1);
        assert_eq!(errs.iter().filter(|e| e.rule_id.is_none()).count(), 1);
        assert_eq!(errs.len(), 4);
        assert!(AutoSwitchConfig::default().validate(&profiles()).is_empty());
    }

    #[test]
    fn validate_flags_unknown_main_profile() {
        let mut cfg = AutoSwitchConfig { home_profile_id: Some("a".into()), ..Default::default() };
        assert!(cfg.validate(&profiles()).is_empty());
        cfg.home_profile_id = Some("ghost".into());
        let errs = cfg.validate(&profiles());
        assert_eq!(errs.len(), 1);
        assert!(errs[0].rule_id.is_none() && errs[0].message.contains("main profile"));
    }

    #[test]
    fn disabled_rule_skipped() {
        let mut r1 = rule("1", MatchKind::Process, "x", "a");
        r1.enabled = false;
        let mut e = engine(vec![r1, rule("2", MatchKind::Process, "x", "b")], None);
        assert_eq!(e.on_focus(app("x", "", "")).unwrap().profile_id, "b");
    }

    #[test]
    fn first_match_wins_and_page() {
        let mut r1 = rule("1", MatchKind::Process, "x", "a");
        r1.page_id = Some("pg".into());
        let mut e = engine(vec![r1, rule("2", MatchKind::Process, "x", "b")], None);
        let t = e.on_focus(app("x", "", "")).unwrap();
        assert_eq!(t, SwitchTarget { profile_id: "a".into(), page_id: Some("pg".into()), rule_id: Some("1".into()) });
    }

    #[test]
    fn unknown_profile_skipped() {
        let mut e =
            engine(vec![rule("1", MatchKind::Process, "x", "ghost"), rule("2", MatchKind::Process, "x", "b")], None);
        assert_eq!(e.on_focus(app("x", "", "")).unwrap().rule_id.as_deref(), Some("2"));
        e.set_known_profiles(vec![]);
        let mut e2 = engine(vec![rule("1", MatchKind::Process, "x", "ghost")], Some("ghost"));
        assert_eq!(e2.on_focus(app("x", "", "")), None);
    }

    #[test]
    fn fallback() {
        let mut e = engine(vec![rule("1", MatchKind::Process, "x", "a")], Some("fb"));
        assert_eq!(e.on_focus(app("x", "", "")).unwrap().profile_id, "a");
        let t = e.on_focus(app("other", "", "")).unwrap();
        assert_eq!(t, SwitchTarget { profile_id: "fb".into(), page_id: None, rule_id: None });
        // No fallback and no match: None, and the last target is forgotten.
        let mut e = engine(vec![rule("1", MatchKind::Process, "x", "a")], None);
        assert!(e.on_focus(app("x", "", "")).is_some());
        assert_eq!(e.on_focus(app("other", "", "")), None);
        assert_eq!(e.on_focus(app("x", "", "")).unwrap().profile_id, "a");
    }

    #[test]
    fn own_processes_ignored() {
        let mut e = engine(vec![rule("1", MatchKind::Process, "x", "a")], Some("fb"));
        for p in ["notideck", "Notideck.exe", "nd-agent", "ND-AGENT.EXE"] {
            assert_eq!(e.on_focus(app(p, "", "")), None, "{p}");
        }
        assert!(e.last_app().is_none());
        e.on_focus(app("x", "", ""));
        assert_eq!(e.on_focus(app("notideck.exe", "", "")), None);
        assert_eq!(e.last_app().unwrap().process, "x");
    }

    #[test]
    fn dedupe_and_manual_stickiness() {
        let mut e =
            engine(vec![rule("1", MatchKind::Process, "x", "a"), rule("2", MatchKind::Process, "y", "b")], None);
        assert!(e.on_focus(app("x", "", "t1")).is_some());
        // Same target again (even with different title) -> None, so a manual change sticks.
        assert_eq!(e.on_focus(app("x", "", "t2")), None);
        assert_eq!(e.on_focus(app("x", "", "t2")), None);
        assert_eq!(e.on_focus(app("y", "", "")).unwrap().profile_id, "b");
        assert_eq!(e.on_focus(app("x", "", "")).unwrap().profile_id, "a");
    }

    #[test]
    fn lock_unlock() {
        let mut e =
            engine(vec![rule("1", MatchKind::Process, "x", "a"), rule("2", MatchKind::Process, "y", "b")], None);
        assert!(e.on_focus(app("x", "", "")).is_some());
        assert_eq!(e.set_locked(true), None);
        assert!(e.locked());
        assert_eq!(e.on_focus(app("y", "", "")), None);
        assert_eq!(e.last_app().unwrap().process, "y");
        assert_eq!(e.set_locked(false).unwrap().profile_id, "b");
        // Unlock while already unlocked -> None.
        assert_eq!(e.set_locked(false), None);
        // Lock/unlock without focus change: resume re-applies the focused app's profile.
        e.set_locked(true);
        assert_eq!(e.set_locked(false).unwrap().profile_id, "b");
    }

    #[test]
    fn unlock_without_any_app() {
        let mut e = engine(vec![], Some("fb"));
        assert_eq!(e.set_locked(false), None);
    }

    #[test]
    fn unmatched_app_in_between_allows_switch_back() {
        let mut e = engine(vec![rule("1", MatchKind::Process, "obs64", "a")], None);
        assert_eq!(e.on_focus(app("obs64.exe", "", "")).unwrap().profile_id, "a");
        assert_eq!(e.on_focus(app("chrome.exe", "", "")), None);
        assert_eq!(e.on_focus(app("obs64.exe", "", "")).unwrap().profile_id, "a");
    }

    #[test]
    fn set_config_reevaluates() {
        let mut e = engine(vec![rule("1", MatchKind::Process, "x", "a")], None);
        e.on_focus(app("x", "", ""));
        // Same config again: last target forgotten, so it is returned again.
        let same = e.config().clone();
        assert_eq!(e.set_config(same).unwrap().profile_id, "a");
        let cfg = AutoSwitchConfig {
            home_profile_id: None,
            enabled: true,
            fallback_profile_id: None,
            rules: vec![rule("9", MatchKind::Process, "x", "c")],
        };
        let t = e.set_config(cfg).unwrap();
        assert_eq!((t.profile_id.as_str(), t.rule_id.as_deref()), ("c", Some("9")));
        // Config that matches nothing.
        assert_eq!(e.set_config(AutoSwitchConfig::default()), None);
        // Title regex cache is rebuilt.
        let cfg = AutoSwitchConfig {
            home_profile_id: None,
            enabled: true,
            fallback_profile_id: None,
            rules: vec![rule("t", MatchKind::Title, "zz", "b")],
        };
        e.on_focus(app("q", "", "azzb"));
        assert_eq!(e.last_app().unwrap().title, "azzb");
        assert_eq!(e.set_config(cfg).unwrap().profile_id, "b");
    }

    #[test]
    fn set_config_without_app_or_when_locked() {
        let mut e = engine(vec![], None);
        assert_eq!(e.set_config(AutoSwitchConfig::default()), None);
        let mut e = engine(vec![rule("1", MatchKind::Process, "x", "a")], None);
        e.on_focus(app("x", "", ""));
        e.set_locked(true);
        let c = e.config().clone();
        assert_eq!(e.set_config(c), None);
    }

    #[test]
    fn disabled_config() {
        let mut cfg = AutoSwitchConfig {
            home_profile_id: None,
            enabled: false,
            fallback_profile_id: Some("fb".into()),
            rules: vec![rule("1", MatchKind::Process, "x", "a")],
        };
        let mut e = AutoSwitch::new(cfg.clone(), profiles());
        assert_eq!(e.on_focus(app("x", "", "")), None);
        assert_eq!(e.set_locked(false), None);
        // Enabling re-evaluates the remembered app.
        cfg.enabled = true;
        assert_eq!(e.set_config(cfg).unwrap().profile_id, "a");
    }

    #[test]
    fn engine_is_send() {
        fn is_send<T: Send>() {}
        is_send::<AutoSwitch>();
    }

    #[test]
    fn serde_defaults() {
        let cfg: AutoSwitchConfig =
            serde_json::from_str(r#"{"rules":[{"id":"1","kind":"process","pattern":"x","profile_id":"a"}]}"#).unwrap();
        assert!(cfg.enabled);
        assert!(cfg.rules[0].enabled);
        assert_eq!(cfg.rules[0].page_id, None);
    }

    #[test]
    fn recent_apps() {
        let mut r = RecentApps::default();
        r.push(&app("", "", "ghost"));
        assert!(r.list().is_empty());
        r.push(&app("a", "c1", "t1"));
        r.push(&app("b", "c1", "t1"));
        r.push(&app("a", "c1", "t2"));
        assert_eq!(r.list().len(), 2);
        assert_eq!(r.list()[0].process, "a");
        assert_eq!(r.list()[0].title, "t2");
        assert_eq!(r.list()[1].process, "b");
        // Same process, different class is distinct; class-only is kept.
        r.push(&app("a", "c2", ""));
        r.push(&app("", "wayland-app", ""));
        assert_eq!(r.list().len(), 4);
        for i in 0..30 {
            r.push(&app(&format!("p{i}"), "", ""));
        }
        assert_eq!(r.list().len(), RecentApps::MAX);
        assert_eq!(r.list()[0].process, "p29");
    }

    #[test]
    fn page_memory() {
        let mut m = PageMemory::default();
        assert_eq!(m.page_for("a"), None);
        m.remember("a", "p1");
        m.remember("b", "p9");
        m.remember("a", "p2");
        assert_eq!(m.page_for("a"), Some("p2"));
        assert_eq!(m.page_for("b"), Some("p9"));
    }
}
