// SPDX-License-Identifier: GPL-3.0-or-later
//! State management for deck sessions (SPEC §5, §6.1, §6.2).
//!
//! Tracks active profile, active page, folder navigation stack, and control
//! runtime states (values, state indices, title/subtitle overrides).
//! Generates [`nd_proto::v1::SetLayout`] and [`nd_proto::v1::UpdateControls`] messages.

use crate::error::{CoreError, Result};
use crate::live::LiveSample;
use crate::model::{Action, Control, DataSource, Page, Profile, TriggerKind};
use crate::rail::RAIL_PAGE_ID;
use crate::router::{ActionOutcome, ActionRouter, InputContext, NavigationTarget};
use nd_proto::v1::{ControlPatch, InputEvent, InputKind, SetLayout, UpdateControls};

/// Dial step per detent; matches the Android optimistic `DIAL_STEP`.
const DIAL_STEP: f32 = 0.02;

/// Maximum repetitions of a `dial_rotate_cw/ccw` action for one event.
const MAX_DIAL_REPEAT: i32 = 20;

/// The reaction to an input event, indicating what protocol message should be sent to the phone.
#[derive(Debug, Clone, PartialEq)]
pub enum InputReaction {
    /// Full layout change (e.g. after page navigation). Send `SetLayout` to phone.
    SetLayout(SetLayout),
    /// Partial control update. Send `UpdateControls` to phone.
    UpdateControls(UpdateControls),
    /// No message needs to be sent to the phone.
    None,
}

/// Active deck session state for a connected phone.
#[derive(Debug, Clone)]
pub struct DeckSession {
    /// Currently loaded profile.
    profile: Profile,

    /// Currently active page ID.
    active_page_id: String,

    /// Navigation history stack for folders (SPEC §6.2).
    folder_stack: Vec<String>,

    /// Pinned side-rail keys (global, round 11). Their runtime state lives here, separate from pages.
    rail: Vec<Control>,
}

impl DeckSession {
    /// Create a new session with the given profile.
    ///
    /// Sets the active page to the profile's initial page, or the first page if available.
    pub fn new(profile: Profile) -> Self {
        let active_page_id = profile.initial_page().map(|p| p.id.clone()).unwrap_or_default();

        Self { profile, active_page_id, folder_stack: Vec::new(), rail: Vec::new() }
    }

    // ------------------------------------------------------------------- Rail

    /// The pinned rail keys with their current runtime state.
    pub fn rail(&self) -> &[Control] {
        &self.rail
    }

    /// Mutable rail keys (in-place edits that must not reset runtime state, e.g. plugin settings).
    pub fn rail_mut(&mut self) -> &mut [Control] {
        &mut self.rail
    }

    /// Replace the rail keys (runtime state resets to what the controls carry).
    pub fn set_rail(&mut self, controls: Vec<Control>) {
        self.rail = controls;
    }

    /// `SetRail` message for the current rail.
    pub fn rail_proto(&self) -> nd_proto::v1::SetRail {
        nd_proto::v1::SetRail { controls: self.rail.iter().map(Control::to_proto).collect() }
    }

    /// Mutable control on `page_id` (`RAIL_PAGE_ID` = the rail).
    fn control_on_mut(&mut self, page_id: &str, control_id: &str) -> Result<&mut Control> {
        let found = if page_id == RAIL_PAGE_ID {
            self.rail.iter_mut().find(|c| c.id == control_id)
        } else {
            self.profile.get_page_mut(page_id).and_then(|p| p.get_control_mut(control_id))
        };
        found.ok_or_else(|| CoreError::ControlNotFound {
            page_id: page_id.to_string(),
            control_id: control_id.to_string(),
        })
    }

    /// Access the active profile.
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Access the active profile mutably.
    pub fn profile_mut(&mut self) -> &mut Profile {
        &mut self.profile
    }

    /// ID of the currently active page.
    pub fn active_page_id(&self) -> &str {
        &self.active_page_id
    }

    /// Reference to the currently active page.
    pub fn active_page(&self) -> Option<&Page> {
        self.profile.get_page(&self.active_page_id)
    }

    /// Mutable reference to the currently active page.
    pub fn active_page_mut(&mut self) -> Option<&mut Page> {
        self.profile.get_page_mut(&self.active_page_id)
    }

    /// Whether the session is currently navigated into a subfolder.
    pub fn is_in_folder(&self) -> bool {
        !self.folder_stack.is_empty()
    }

    /// Current depth of the folder navigation stack.
    pub fn folder_depth(&self) -> usize {
        self.folder_stack.len()
    }

    /// Slice of page IDs in the folder navigation stack.
    pub fn folder_stack(&self) -> &[String] {
        &self.folder_stack
    }

    /// Switch active profile, resetting page navigation.
    pub fn set_profile(&mut self, profile: Profile) -> Result<SetLayout> {
        self.profile = profile;
        self.folder_stack.clear();
        self.active_page_id = self.profile.initial_page().map(|p| p.id.clone()).unwrap_or_default();

        self.current_layout()
    }

    /// Generate a `SetLayout` message for the currently active page.
    pub fn current_layout(&self) -> Result<SetLayout> {
        let page = self.active_page().ok_or_else(|| CoreError::PageNotFound(self.active_page_id.clone()))?;

        Ok(page.to_proto(self.profile.cols, self.profile.rows))
    }

    /// Generate a `SetLayout` message for a specific page.
    pub fn layout_for_page(&self, page_id: &str) -> Result<SetLayout> {
        let page = self.profile.get_page(page_id).ok_or_else(|| CoreError::PageNotFound(page_id.to_string()))?;

        Ok(page.to_proto(self.profile.cols, self.profile.rows))
    }

    // ---------------------------------------------------------------- Navigation

    /// Navigate to a specific page by ID (clears folder back stack).
    pub fn navigate_to(&mut self, page_id: &str) -> Result<SetLayout> {
        if self.profile.get_page(page_id).is_none() {
            return Err(CoreError::PageNotFound(page_id.to_string()));
        }
        self.folder_stack.clear();
        self.active_page_id = page_id.to_string();
        self.current_layout()
    }

    /// Open a folder page, pushing the current page onto the folder back stack.
    pub fn open_folder(&mut self, folder_page_id: &str) -> Result<SetLayout> {
        if self.profile.get_page(folder_page_id).is_none() {
            return Err(CoreError::PageNotFound(folder_page_id.to_string()));
        }
        self.folder_stack.push(self.active_page_id.clone());
        self.active_page_id = folder_page_id.to_string();
        self.current_layout()
    }

    /// Go back to the previous page in the folder stack, or stays on current page if stack is empty.
    pub fn go_back(&mut self) -> Result<SetLayout> {
        if let Some(prev_page_id) = self.folder_stack.pop() {
            if self.profile.get_page(&prev_page_id).is_some() {
                self.active_page_id = prev_page_id;
            }
        }
        self.current_layout()
    }

    /// Index of the visible top-level page and the number of top-level pages, for the phone's page
    /// dots. While a folder is open the count is 0 (dots hidden).
    pub fn page_position(&self) -> (u32, u32) {
        if self.is_in_folder() {
            return (0, 0);
        }
        let idx = self.profile.pages.iter().position(|p| p.id == self.active_page_id).unwrap_or(0);
        (idx as u32, self.profile.pages.len() as u32)
    }

    /// Phone page dot: close any folder and show `pages[index]`. Out-of-range indices return `None`.
    pub fn go_to_page_index(&mut self, index: usize) -> Option<Result<SetLayout>> {
        let id = self.profile.pages.get(index)?.id.clone();
        Some(self.navigate_to(&id))
    }

    /// Navigate to the next page in the profile (cyclical).
    pub fn next_page(&mut self) -> Result<SetLayout> {
        if self.profile.pages.is_empty() {
            return self.current_layout();
        }
        self.folder_stack.clear();
        let current_idx = self.profile.pages.iter().position(|p| p.id == self.active_page_id).unwrap_or(0);
        let next_idx = (current_idx + 1) % self.profile.pages.len();
        self.active_page_id = self.profile.pages[next_idx].id.clone();
        self.current_layout()
    }

    /// Navigate to the previous page in the profile (cyclical).
    pub fn prev_page(&mut self) -> Result<SetLayout> {
        if self.profile.pages.is_empty() {
            return self.current_layout();
        }
        self.folder_stack.clear();
        let current_idx = self.profile.pages.iter().position(|p| p.id == self.active_page_id).unwrap_or(0);
        let prev_idx = (current_idx + self.profile.pages.len() - 1) % self.profile.pages.len();
        self.active_page_id = self.profile.pages[prev_idx].id.clone();
        self.current_layout()
    }

    // ----------------------------------------------------------- Control Updates

    /// Find a control on the currently active page.
    pub fn get_control(&self, control_id: &str) -> Option<&Control> {
        self.active_page().and_then(|p| p.get_control(control_id))
    }

    /// Find a control on the currently active page (mutable).
    pub fn get_control_mut(&mut self, control_id: &str) -> Option<&mut Control> {
        let page_id = self.active_page_id.clone();
        self.profile.get_page_mut(&page_id).and_then(|p| p.get_control_mut(control_id))
    }

    /// Update a control's value (slider/dial 0.0..1.0) on the active page and return `UpdateControls`.
    pub fn set_control_value(&mut self, control_id: &str, value: f32) -> Result<UpdateControls> {
        let page_id = self.active_page_id.clone();
        self.set_control_value_on(&page_id, control_id, value)
    }

    fn set_control_value_on(&mut self, page_id: &str, control_id: &str, value: f32) -> Result<UpdateControls> {
        let control = self.control_on_mut(page_id, control_id)?;

        control.value = value.clamp(0.0, 1.0);
        let patch = ControlPatch {
            id: control.id.clone(),
            fields: vec!["value".to_string()],
            control: Some(control.to_proto()),
        };

        Ok(UpdateControls { page_id: page_id.to_string(), patches: vec![patch] })
    }

    /// Update a control's state index (multi-state key) on the active page and return `UpdateControls`.
    pub fn set_control_state_index(&mut self, control_id: &str, state_index: u32) -> Result<UpdateControls> {
        let page_id = self.active_page_id.clone();
        self.set_control_state_index_on(&page_id, control_id, state_index)
    }

    fn set_control_state_index_on(
        &mut self,
        page_id: &str,
        control_id: &str,
        state_index: u32,
    ) -> Result<UpdateControls> {
        let control = self.control_on_mut(page_id, control_id)?;

        control.state_index = state_index;
        let patch = ControlPatch {
            id: control.id.clone(),
            fields: vec![
                "state_index".to_string(),
                "title".to_string(),
                "subtitle".to_string(),
                "icon_hash".to_string(),
                "fg_argb".to_string(),
                "bg_argb".to_string(),
            ],
            control: Some(control.to_proto()),
        };

        Ok(UpdateControls { page_id: page_id.to_string(), patches: vec![patch] })
    }

    /// Toggle or cycle a control's state index on the active page and return `UpdateControls`.
    pub fn toggle_control_state(&mut self, control_id: &str) -> Result<UpdateControls> {
        let page_id = self.active_page_id.clone();
        self.toggle_control_state_on(&page_id, control_id)
    }

    fn toggle_control_state_on(&mut self, page_id: &str, control_id: &str) -> Result<UpdateControls> {
        let current_idx = {
            let control = self.control_on_mut(page_id, control_id)?;
            let num_states = control.states.len();
            if num_states > 0 {
                (control.state_index + 1) % (num_states as u32)
            } else if control.state_index == 0 {
                1
            } else {
                0
            }
        };

        self.set_control_state_index_on(page_id, control_id, current_idx)
    }

    /// Update a control's title on the active page and return `UpdateControls`.
    pub fn set_control_title(&mut self, control_id: &str, title: &str) -> Result<UpdateControls> {
        let page_id = self.active_page_id.clone();
        let control =
            self.profile.get_page_mut(&page_id).and_then(|p| p.get_control_mut(control_id)).ok_or_else(|| {
                CoreError::ControlNotFound { page_id: page_id.clone(), control_id: control_id.to_string() }
            })?;

        control.title = title.to_string();
        let patch = ControlPatch {
            id: control.id.clone(),
            fields: vec!["title".to_string()],
            control: Some(control.to_proto()),
        };

        Ok(UpdateControls { page_id, patches: vec![patch] })
    }

    /// Update a control's subtitle on the active page and return `UpdateControls`.
    pub fn set_control_subtitle(&mut self, control_id: &str, subtitle: &str) -> Result<UpdateControls> {
        let page_id = self.active_page_id.clone();
        let control =
            self.profile.get_page_mut(&page_id).and_then(|p| p.get_control_mut(control_id)).ok_or_else(|| {
                CoreError::ControlNotFound { page_id: page_id.clone(), control_id: control_id.to_string() }
            })?;

        control.subtitle = subtitle.to_string();
        let patch = ControlPatch {
            id: control.id.clone(),
            fields: vec!["subtitle".to_string()],
            control: Some(control.to_proto()),
        };

        Ok(UpdateControls { page_id, patches: vec![patch] })
    }

    /// Update a control's icon hash on the active page and return `UpdateControls`.
    pub fn set_control_icon(&mut self, control_id: &str, icon_hash: &str) -> Result<UpdateControls> {
        let page_id = self.active_page_id.clone();
        let control =
            self.profile.get_page_mut(&page_id).and_then(|p| p.get_control_mut(control_id)).ok_or_else(|| {
                CoreError::ControlNotFound { page_id: page_id.clone(), control_id: control_id.to_string() }
            })?;

        control.icon_hash = icon_hash.to_string();
        let patch = ControlPatch {
            id: control.id.clone(),
            fields: vec!["icon_hash".to_string()],
            control: Some(control.to_proto()),
        };

        Ok(UpdateControls { page_id, patches: vec![patch] })
    }

    /// Update a control's colors on the active page and return `UpdateControls`.
    pub fn set_control_colors(&mut self, control_id: &str, fg_argb: u32, bg_argb: u32) -> Result<UpdateControls> {
        let page_id = self.active_page_id.clone();
        let control =
            self.profile.get_page_mut(&page_id).and_then(|p| p.get_control_mut(control_id)).ok_or_else(|| {
                CoreError::ControlNotFound { page_id: page_id.clone(), control_id: control_id.to_string() }
            })?;

        control.fg_argb = fg_argb;
        control.bg_argb = bg_argb;
        let patch = ControlPatch {
            id: control.id.clone(),
            fields: vec!["fg_argb".to_string(), "bg_argb".to_string()],
            control: Some(control.to_proto()),
        };

        Ok(UpdateControls { page_id, patches: vec![patch] })
    }

    // --------------------------------------------------------------- Live data

    /// Controls on the active page that display live data.
    pub fn live_sources(&self) -> Vec<(String, DataSource)> {
        self.active_page()
            .map(|p| p.controls.iter().filter_map(|c| c.source.clone().map(|s| (c.id.clone(), s))).collect())
            .unwrap_or_default()
    }

    /// Apply live samples to the active page. Returns patches only for fields that changed
    /// (values compared with a 0.005 tolerance), or `None` if nothing changed.
    pub fn apply_live(&mut self, samples: &[(String, LiveSample)]) -> Option<UpdateControls> {
        let page_id = self.active_page_id.clone();
        self.apply_live_on(&page_id, samples)
    }

    /// Rail keys that display live data.
    pub fn rail_live_sources(&self) -> Vec<(String, DataSource)> {
        self.rail.iter().filter_map(|c| c.source.clone().map(|s| (c.id.clone(), s))).collect()
    }

    /// Round 19: plugin actions on the active page, keyed by control id. A control with several plugin
    /// bindings reports the first one (the context is per control). Plugin steps inside `multi` don't count.
    pub fn plugin_instances(&self) -> Vec<(String, crate::plugin::PluginInstance)> {
        let Some(page) = self.active_page() else { return Vec::new() };
        page.controls.iter().filter_map(|c| plugin_instance(&self.profile.id, c).map(|i| (c.id.clone(), i))).collect()
    }

    /// Round 19: plugin actions on the rail keys, keyed by control id.
    pub fn rail_plugin_instances(&self) -> Vec<(String, crate::plugin::PluginInstance)> {
        self.rail
            .iter()
            .filter_map(|c| plugin_instance(crate::plugin::RAIL_CONTEXT, c).map(|i| (c.id.clone(), i)))
            .collect()
    }

    /// Apply live samples to the rail keys (patches carry `RAIL_PAGE_ID`).
    pub fn apply_live_rail(&mut self, samples: &[(String, LiveSample)]) -> Option<UpdateControls> {
        self.apply_live_on(RAIL_PAGE_ID, samples)
    }

    fn apply_live_on(&mut self, page_id: &str, samples: &[(String, LiveSample)]) -> Option<UpdateControls> {
        let mut patches = Vec::new();
        for (id, sample) in samples {
            let Ok(control) = self.control_on_mut(page_id, id) else {
                continue;
            };
            let mut fields: Vec<&str> = Vec::new();
            if let Some(v) = sample.value {
                let v = v.clamp(0.0, 1.0);
                if (control.value - v).abs() > 0.005 {
                    control.value = v;
                    fields.push("value");
                }
            }
            if let Some(t) = &sample.title {
                if control.title != *t {
                    control.title = t.clone();
                    fields.push("title");
                }
            }
            if let Some(t) = &sample.subtitle {
                if control.subtitle != *t {
                    control.subtitle = t.clone();
                    fields.push("subtitle");
                }
            }
            if let Some(t) = &sample.detail {
                if control.detail != *t {
                    control.detail = t.clone();
                    fields.push("detail");
                }
            }
            if let Some(i) = sample.state_index {
                if control.state_index != i {
                    control.state_index = i;
                    for f in ["state_index", "title", "subtitle", "icon_hash", "fg_argb", "bg_argb"] {
                        if !fields.contains(&f) {
                            fields.push(f);
                        }
                    }
                }
            }
            if let Some(h) = &sample.icon_hash {
                if control.icon_hash != *h {
                    control.icon_hash = h.clone();
                    fields.push("icon_hash");
                }
            }
            if let Some(m) = sample.meter {
                if control.meter != m {
                    control.meter = m;
                    fields.push("meter");
                    if !m && control.level != 0.0 {
                        control.level = 0.0;
                        fields.push("level");
                    }
                }
            }
            if let Some(d) = sample.dimmed {
                if control.dimmed != d {
                    control.dimmed = d;
                    fields.push("dimmed");
                }
            }
            if !fields.is_empty() {
                patches.push(ControlPatch {
                    id: control.id.clone(),
                    fields: fields.into_iter().map(String::from).collect(),
                    control: Some(control.to_proto()),
                });
            }
        }
        if patches.is_empty() {
            None
        } else {
            Some(UpdateControls { page_id: page_id.to_string(), patches })
        }
    }

    /// Apply meter levels to the active page (patches only field `level`, only meter controls).
    pub fn apply_levels(&mut self, samples: &[(String, f32)]) -> Option<UpdateControls> {
        let page_id = self.active_page_id.clone();
        self.apply_levels_on(&page_id, samples)
    }

    /// Apply meter levels to the rail keys (patches carry `RAIL_PAGE_ID`).
    pub fn apply_levels_rail(&mut self, samples: &[(String, f32)]) -> Option<UpdateControls> {
        self.apply_levels_on(RAIL_PAGE_ID, samples)
    }

    fn apply_levels_on(&mut self, page_id: &str, samples: &[(String, f32)]) -> Option<UpdateControls> {
        let mut patches = Vec::new();
        for (id, level) in samples {
            let Ok(control) = self.control_on_mut(page_id, id) else {
                continue;
            };
            if !control.meter {
                continue;
            }
            let new = level.clamp(0.0, 1.0);
            let old = control.level;
            if (new - old).abs() >= 0.02 - 1e-6 || (new == 0.0 && old != 0.0) {
                control.level = new;
                patches.push(ControlPatch {
                    id: control.id.clone(),
                    fields: vec!["level".to_string()],
                    control: Some(control.to_proto()),
                });
            }
        }
        if patches.is_empty() {
            None
        } else {
            Some(UpdateControls { page_id: page_id.to_string(), patches })
        }
    }

    // ------------------------------------------------------------- Input Routing

    /// Handle an incoming `InputEvent` from the phone.
    ///
    /// Finds matching actions for the control and event trigger, executes them
    /// against `router`, and updates internal session state if navigation or
    /// control state toggling was requested.
    pub fn handle_input(&mut self, event: &InputEvent, router: &ActionRouter) -> Result<InputReaction> {
        let page_id = if event.page_id.is_empty() { self.active_page_id.clone() } else { event.page_id.clone() };

        let proto_kind = InputKind::try_from(event.kind).unwrap_or(InputKind::Unspecified);
        let trigger = TriggerKind::from_proto_input_kind(proto_kind);

        // Collect actions to execute from the target control
        let (matched_actions, current_value) = {
            let control = if page_id == RAIL_PAGE_ID {
                self.rail.iter().find(|c| c.id == event.control_id)
            } else {
                let page = self.profile.get_page(&page_id).ok_or_else(|| CoreError::PageNotFound(page_id.clone()))?;
                page.get_control(&event.control_id)
            }
            .ok_or_else(|| CoreError::ControlNotFound {
                page_id: page_id.clone(),
                control_id: event.control_id.clone(),
            })?;

            let matching = |t: TriggerKind| -> Vec<Action> {
                control.actions.iter().filter(|d| d.trigger == t).map(|d| d.action.clone()).collect()
            };

            // Ordered (action, ctx) executions; the context is filled in below.
            let mut runs: Vec<(Action, i32, TriggerKind)> = Vec::new();
            let delta = event.delta;
            match proto_kind {
                InputKind::DialRotate => {
                    runs.extend(
                        matching(TriggerKind::DialRotate).into_iter().map(|a| (a, delta, TriggerKind::DialRotate)),
                    );
                    let (kind, n) = if delta > 0 {
                        (TriggerKind::DialRotateCw, delta)
                    } else {
                        (TriggerKind::DialRotateCcw, delta.saturating_abs())
                    };
                    let n = n.clamp(0, MAX_DIAL_REPEAT);
                    let sign = delta.signum();
                    let acts = matching(kind);
                    for _ in 0..n {
                        runs.extend(acts.iter().cloned().map(|a| (a, sign, kind)));
                    }
                }
                InputKind::SliderValue => {
                    runs.extend(
                        matching(TriggerKind::SliderValue).into_iter().map(|a| (a, 0, TriggerKind::SliderValue)),
                    );
                }
                _ => {
                    if let Some(t) = trigger {
                        runs.extend(matching(t).into_iter().map(|a| (a, delta, t)));
                    }
                }
            }
            (runs, control.value)
        };

        let value = if proto_kind == InputKind::SliderValue { event.value } else { current_value };

        // Slider values are always stored (active page only), even without actions.
        let mut updates: Vec<UpdateControls> = Vec::new();
        // Rail keys keep their state on the rail; everything else acts on the active page.
        let is_rail = page_id == RAIL_PAGE_ID;
        let state_page = if is_rail { RAIL_PAGE_ID.to_string() } else { self.active_page_id.clone() };
        let on_state_page = is_rail || page_id == self.active_page_id;
        if proto_kind == InputKind::SliderValue && on_state_page {
            updates.push(self.set_control_value_on(&state_page, &event.control_id, event.value)?);
        }

        let mut layout: Option<SetLayout> = None;
        let mut got_set_value = false;

        let context = crate::plugin::plugin_context(
            if is_rail { crate::plugin::RAIL_CONTEXT } else { &self.profile.id },
            &event.control_id,
        );
        for (action, delta, trigger) in matched_actions {
            let ctx = InputContext { delta, value, context: context.clone(), trigger: Some(trigger) };
            match router.execute(&action, &ctx)? {
                ActionOutcome::Executed => {
                    // OS action succeeded, no protocol reaction needed
                }
                ActionOutcome::Navigate(nav) => {
                    layout = Some(match nav {
                        NavigationTarget::Page(target_id) => self.navigate_to(&target_id)?,
                        NavigationTarget::Next => self.next_page()?,
                        NavigationTarget::Prev => self.prev_page()?,
                        NavigationTarget::Folder(folder_id) => self.open_folder(&folder_id)?,
                        NavigationTarget::Back => self.go_back()?,
                    });
                }
                ActionOutcome::ToggleState => {
                    updates.push(self.toggle_control_state_on(&state_page, &event.control_id)?);
                }
                ActionOutcome::SetValue(v) => {
                    got_set_value = true;
                    updates.push(self.set_control_value_on(&state_page, &event.control_id, v)?);
                }
                ActionOutcome::SetState(i) => {
                    updates.push(self.set_control_state_index_on(&state_page, &event.control_id, i)?);
                }
            }
        }

        // A dial turn always moves the dial, even without a volume-like action.
        if proto_kind == InputKind::DialRotate && !got_set_value && event.delta != 0 && on_state_page {
            let v = current_value + event.delta as f32 * DIAL_STEP;
            updates.push(self.set_control_value_on(&state_page, &event.control_id, v)?);
        }

        if let Some(layout) = layout {
            return Ok(InputReaction::SetLayout(layout));
        }
        Ok(match merge_updates(updates) {
            Some(u) => InputReaction::UpdateControls(u),
            None => InputReaction::None,
        })
    }
}

/// Merge several `UpdateControls` (same page) so each control gets a single patch:
/// union of fields, latest control snapshot.
fn merge_updates(updates: Vec<UpdateControls>) -> Option<UpdateControls> {
    let mut iter = updates.into_iter();
    let mut merged = iter.next()?;
    for u in iter {
        for patch in u.patches {
            match merged.patches.iter_mut().find(|p| p.id == patch.id) {
                Some(existing) => {
                    for f in patch.fields {
                        if !existing.fields.contains(&f) {
                            existing.fields.push(f);
                        }
                    }
                    existing.control = patch.control;
                }
                None => merged.patches.push(patch),
            }
        }
    }
    Some(merged)
}

/// The control's first top-level plugin binding as an on-screen instance.
fn plugin_instance(scope: &str, c: &Control) -> Option<crate::plugin::PluginInstance> {
    c.actions.iter().find_map(|d| match &d.action {
        Action::Plugin { plugin, action, settings } => Some(crate::plugin::PluginInstance {
            context: crate::plugin::plugin_context(scope, &c.id),
            plugin: plugin.clone(),
            action: action.clone(),
            settings: settings.clone(),
            control: c.kind,
            state: c.state_index,
            value: c.value,
        }),
        _ => None,
    })
}
