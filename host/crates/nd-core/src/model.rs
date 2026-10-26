// SPDX-License-Identifier: GPL-3.0-or-later
//! Core data models: Profile, Page, Control, Action, and ActionDefinition.
//!
//! All models are serializable to/from JSON for storage in `%APPDATA%\Notideck\profiles\`
//! or `$XDG_CONFIG_HOME/notideck/profiles/` (SPEC §9).

use crate::error::{CoreError, Result};
use crate::obs::{ObsInput, ObsScene, ObsToggle};
use nd_platform::{AudioTarget, Key, KeyCombo};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Current profile schema version (SPEC §9).
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

fn default_cols() -> u32 {
    nd_proto::DEFAULT_COLS
}

fn default_rows() -> u32 {
    nd_proto::DEFAULT_ROWS
}

fn default_span() -> u32 {
    1
}

/// A Notideck profile (SPEC §6.2).
///
/// A profile contains one or more pages of controls, an optional list of target
/// application names for auto-profile switching (SPEC §6.3), and layout dimensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Schema version for future migrations.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Unique identifier for the profile.
    pub id: String,

    /// User-visible profile name (e.g. "Default", "OBS Studio").
    pub name: String,

    /// Default grid column count (default: 5).
    #[serde(default = "default_cols")]
    pub cols: u32,

    /// Default grid row count (default: 3).
    #[serde(default = "default_rows")]
    pub rows: u32,

    /// Optional target process/window names for auto-profile switching (SPEC §6.3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub app_bindings: Vec<String>,

    /// Initial page to display. If not specified, the first page is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_page_id: Option<String>,

    /// Pages in this profile.
    #[serde(default)]
    pub pages: Vec<Page>,
}

impl Profile {
    /// Create a new profile with the default grid size (5x3).
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: id.into(),
            name: name.into(),
            cols: default_cols(),
            rows: default_rows(),
            app_bindings: Vec::new(),
            initial_page_id: None,
            pages: Vec::new(),
        }
    }

    /// Load a profile from a JSON string.
    pub fn from_json(json_str: &str) -> Result<Self> {
        let mut value: serde_json::Value = serde_json::from_str(json_str)?;
        migrate_profile_json(&mut value)?;
        let profile: Self = serde_json::from_value(value)?;
        Ok(profile)
    }

    /// Serialize the profile to a compact JSON string.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Serialize the profile to pretty-printed JSON.
    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Find a page by ID.
    pub fn get_page(&self, id: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    /// Find a page by ID (mutable).
    pub fn get_page_mut(&mut self, id: &str) -> Option<&mut Page> {
        self.pages.iter_mut().find(|p| p.id == id)
    }

    /// Get the initial page (specified by `initial_page_id` or first page).
    pub fn initial_page(&self) -> Option<&Page> {
        if let Some(ref id) = self.initial_page_id {
            self.get_page(id).or_else(|| self.pages.first())
        } else {
            self.pages.first()
        }
    }

    /// Add a page to the profile.
    pub fn add_page(&mut self, page: Page) {
        if self.pages.is_empty() && self.initial_page_id.is_none() {
            self.initial_page_id = Some(page.id.clone());
        }
        self.pages.push(page);
    }

    /// Remove a page by ID.
    pub fn remove_page(&mut self, id: &str) -> Option<Page> {
        if let Some(pos) = self.pages.iter().position(|p| p.id == id) {
            let removed = self.pages.remove(pos);
            if self.initial_page_id.as_deref() == Some(id) {
                self.initial_page_id = self.pages.first().map(|p| p.id.clone());
            }
            Some(removed)
        } else {
            None
        }
    }
}

/// Migration hook for profile JSON (SPEC §9).
fn migrate_profile_json(value: &mut serde_json::Value) -> Result<()> {
    let schema_version = value.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(1) as u32;

    if schema_version > CURRENT_SCHEMA_VERSION {
        return Err(CoreError::MigrationError {
            from: schema_version,
            to: CURRENT_SCHEMA_VERSION,
            message: format!(
                "profile schema version {schema_version} is newer than host version {CURRENT_SCHEMA_VERSION}"
            ),
        });
    }

    // Future migrations (v1 -> v2, etc.) go here.
    Ok(())
}

/// A single deck page/layout (SPEC §6.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// Unique page identifier.
    pub id: String,

    /// Human-readable page name.
    #[serde(default)]
    pub name: String,

    /// Optional grid columns override (if None, profile.cols is used).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u32>,

    /// Optional grid rows override (if None, profile.rows is used).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,

    /// Controls on this page.
    #[serde(default)]
    pub controls: Vec<Control>,
}

impl Page {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { id: id.into(), name: name.into(), cols: None, rows: None, controls: Vec::new() }
    }

    /// Find a control by ID.
    pub fn get_control(&self, id: &str) -> Option<&Control> {
        self.controls.iter().find(|c| c.id == id)
    }

    /// Find a control by ID (mutable).
    pub fn get_control_mut(&mut self, id: &str) -> Option<&mut Control> {
        self.controls.iter_mut().find(|c| c.id == id)
    }

    /// Add a control to the page.
    pub fn add_control(&mut self, control: Control) {
        self.controls.push(control);
    }

    /// Remove a control by ID.
    pub fn remove_control(&mut self, id: &str) -> Option<Control> {
        if let Some(pos) = self.controls.iter().position(|c| c.id == id) {
            Some(self.controls.remove(pos))
        } else {
            None
        }
    }

    /// Translate this page into an `nd_proto::v1::SetLayout` message.
    pub fn to_proto(&self, fallback_cols: u32, fallback_rows: u32) -> nd_proto::v1::SetLayout {
        nd_proto::v1::SetLayout {
            page_id: self.id.clone(),
            cols: self.cols.unwrap_or(fallback_cols),
            rows: self.rows.unwrap_or(fallback_rows),
            controls: self.controls.iter().map(|c| c.to_proto()).collect(),
        }
    }
}

/// Control kind corresponding to protobuf `ControlKind` (SPEC §5, §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Key,
    Dial,
    Slider,
    Tile,
}

impl From<ControlKind> for nd_proto::v1::ControlKind {
    fn from(kind: ControlKind) -> Self {
        match kind {
            ControlKind::Key => nd_proto::v1::ControlKind::Key,
            ControlKind::Dial => nd_proto::v1::ControlKind::Dial,
            ControlKind::Slider => nd_proto::v1::ControlKind::Slider,
            ControlKind::Tile => nd_proto::v1::ControlKind::Tile,
        }
    }
}

impl From<nd_proto::v1::ControlKind> for ControlKind {
    fn from(k: nd_proto::v1::ControlKind) -> Self {
        match k {
            nd_proto::v1::ControlKind::Key => ControlKind::Key,
            nd_proto::v1::ControlKind::Dial => ControlKind::Dial,
            nd_proto::v1::ControlKind::Slider => ControlKind::Slider,
            nd_proto::v1::ControlKind::Tile => ControlKind::Tile,
            nd_proto::v1::ControlKind::Unspecified => ControlKind::Key,
        }
    }
}

/// Visual overrides for a specific state index (for multi-state keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ControlStateVisual {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_hash: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg_argb: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg_argb: Option<u32>,
}

/// A control on a deck page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Control {
    /// Unique control identifier within the page.
    pub id: String,

    /// Type of control: Key, Dial, Slider, Tile.
    pub kind: ControlKind,

    /// 0-based column index.
    pub col: u32,

    /// 0-based row index.
    pub row: u32,

    /// Column span (default: 1).
    pub span_w: u32,

    /// Row span (default: 1).
    pub span_h: u32,

    /// Sha256 content hash of the icon image.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon_hash: String,

    /// Primary title text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,

    /// Secondary subtitle text (e.g. for tiles).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,

    /// Foreground ARGB color (0 = theme default).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fg_argb: u32,

    /// Background ARGB color (0 = theme default).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bg_argb: u32,

    /// State index for multi-state keys.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub state_index: u32,

    /// Dial/slider value (0.0..1.0) or tile gauge.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub value: f32,

    /// Slider orientation (true = horizontal, false = vertical).
    #[serde(default, skip_serializing_if = "is_false")]
    pub horizontal: bool,

    /// Visual appearance per state index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<ControlStateVisual>,

    /// Live data shown on this control, refreshed by the host (SPEC §6.1 Tile, §6.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DataSource>,

    /// Bound actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionDefinition>,

    /// Runtime only (round 12): show a level meter. Set by live data, never saved.
    #[serde(skip)]
    pub meter: bool,

    /// Runtime only (round 12): meter level 0..1.
    #[serde(skip)]
    pub level: f32,

    /// Runtime only (round 12): live data target unavailable, draw inactive.
    #[serde(skip)]
    pub dimmed: bool,

    /// Round 16: how a tile draws its fields (`"media"` = now-playing face). Saved.
    #[serde(default, skip_serializing_if = "ControlFace::is_default")]
    pub face: ControlFace,

    /// Runtime only (round 16): third text line set by live data (media face: `1:23 / 3:45`).
    #[serde(skip)]
    pub detail: String,
}

/// How a tile draws its fields (round 16, proto `ControlFace`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlFace {
    #[default]
    Default,
    /// Now-playing face: art left, track / artist / times, progress bar (see the proto comment).
    Media,
    /// Round 20: live-stream tile: full-bleed still, channel / stream title / viewers, LIVE pill on state 1.
    Stream,
    /// Round 28: camera preview tile, handled on the phone (tap = preview in the tile, long press = full screen).
    Camera,
}

impl ControlFace {
    pub fn is_default(&self) -> bool {
        *self == ControlFace::Default
    }
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl<'de> Deserialize<'de> for Control {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ControlHelper {
            id: String,
            kind: ControlKind,
            col: u32,
            row: u32,
            #[serde(default = "default_span")]
            span_w: u32,
            #[serde(default = "default_span")]
            span_h: u32,
            #[serde(default)]
            icon_hash: String,
            #[serde(default)]
            title: String,
            #[serde(default)]
            subtitle: String,
            #[serde(default)]
            fg_argb: u32,
            #[serde(default)]
            bg_argb: u32,
            #[serde(default)]
            state_index: u32,
            #[serde(default)]
            value: f32,
            #[serde(default)]
            horizontal: bool,
            #[serde(default)]
            states: Vec<ControlStateVisual>,
            #[serde(default)]
            source: Option<DataSource>,
            #[serde(default)]
            action: Option<ActionDefinition>,
            #[serde(default)]
            actions: Vec<ActionDefinition>,
            #[serde(default)]
            face: ControlFace,
        }

        let helper = ControlHelper::deserialize(deserializer)?;
        let mut actions = helper.actions;
        if let Some(single_action) = helper.action {
            actions.push(single_action);
        }

        Ok(Control {
            id: helper.id,
            kind: helper.kind,
            col: helper.col,
            row: helper.row,
            span_w: helper.span_w.max(1),
            span_h: helper.span_h.max(1),
            icon_hash: helper.icon_hash,
            title: helper.title,
            subtitle: helper.subtitle,
            fg_argb: helper.fg_argb,
            bg_argb: helper.bg_argb,
            state_index: helper.state_index,
            value: helper.value,
            horizontal: helper.horizontal,
            states: helper.states,
            source: helper.source,
            meter: false,
            level: 0.0,
            dimmed: false,
            face: helper.face,
            detail: String::new(),
            actions,
        })
    }
}

impl Control {
    /// Create a standard key button.
    pub fn key(id: impl Into<String>, col: u32, row: u32, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: ControlKind::Key,
            col,
            row,
            span_w: 1,
            span_h: 1,
            icon_hash: String::new(),
            title: title.into(),
            subtitle: String::new(),
            fg_argb: 0,
            bg_argb: 0,
            state_index: 0,
            value: 0.0,
            horizontal: false,
            states: Vec::new(),
            source: None,
            meter: false,
            level: 0.0,
            dimmed: false,
            face: ControlFace::Default,
            detail: String::new(),
            actions: Vec::new(),
        }
    }

    /// Create a slider control.
    pub fn slider(id: impl Into<String>, col: u32, row: u32, span_w: u32, span_h: u32, horizontal: bool) -> Self {
        Self {
            id: id.into(),
            kind: ControlKind::Slider,
            col,
            row,
            span_w: span_w.max(1),
            span_h: span_h.max(1),
            icon_hash: String::new(),
            title: String::new(),
            subtitle: String::new(),
            fg_argb: 0,
            bg_argb: 0,
            state_index: 0,
            value: 0.0,
            horizontal,
            states: Vec::new(),
            source: None,
            meter: false,
            level: 0.0,
            dimmed: false,
            face: ControlFace::Default,
            detail: String::new(),
            actions: Vec::new(),
        }
    }

    /// Create a dial (knob) control.
    pub fn dial(id: impl Into<String>, col: u32, row: u32, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: ControlKind::Dial,
            col,
            row,
            span_w: 1,
            span_h: 1,
            icon_hash: String::new(),
            title: title.into(),
            subtitle: String::new(),
            fg_argb: 0,
            bg_argb: 0,
            state_index: 0,
            value: 0.0,
            horizontal: false,
            states: Vec::new(),
            source: None,
            meter: false,
            level: 0.0,
            dimmed: false,
            face: ControlFace::Default,
            detail: String::new(),
            actions: Vec::new(),
        }
    }

    /// Create a live tile control.
    pub fn tile(
        id: impl Into<String>,
        col: u32,
        row: u32,
        title: impl Into<String>,
        subtitle: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: ControlKind::Tile,
            col,
            row,
            span_w: 1,
            span_h: 1,
            icon_hash: String::new(),
            title: title.into(),
            subtitle: subtitle.into(),
            fg_argb: 0,
            bg_argb: 0,
            state_index: 0,
            value: 0.0,
            horizontal: false,
            states: Vec::new(),
            source: None,
            meter: false,
            level: 0.0,
            dimmed: false,
            face: ControlFace::Default,
            detail: String::new(),
            actions: Vec::new(),
        }
    }

    /// Attach a live data source.
    pub fn with_source(mut self, source: DataSource) -> Self {
        self.source = Some(source);
        self
    }

    /// Attach an action that triggers on KeyDown / press.
    pub fn with_action(mut self, action: Action) -> Self {
        self.actions.push(ActionDefinition::new(action));
        self
    }

    /// Attach an action with a specific trigger.
    pub fn with_trigger_action(mut self, trigger: TriggerKind, action: Action) -> Self {
        self.actions.push(ActionDefinition::with_trigger(action, trigger));
        self
    }

    /// Effective title considering current `state_index`.
    pub fn effective_title(&self) -> &str {
        if let Some(state) = self.states.get(self.state_index as usize) {
            if let Some(ref t) = state.title {
                return t.as_str();
            }
        }
        &self.title
    }

    /// Effective subtitle considering current `state_index`.
    pub fn effective_subtitle(&self) -> &str {
        if let Some(state) = self.states.get(self.state_index as usize) {
            if let Some(ref s) = state.subtitle {
                return s.as_str();
            }
        }
        &self.subtitle
    }

    /// Effective icon hash considering current `state_index`.
    pub fn effective_icon_hash(&self) -> &str {
        if let Some(state) = self.states.get(self.state_index as usize) {
            if let Some(ref h) = state.icon_hash {
                return h.as_str();
            }
        }
        &self.icon_hash
    }

    /// Effective foreground ARGB considering current `state_index`.
    pub fn effective_fg_argb(&self) -> u32 {
        if let Some(state) = self.states.get(self.state_index as usize) {
            if let Some(fg) = state.fg_argb {
                return fg;
            }
        }
        self.fg_argb
    }

    /// Effective background ARGB considering current `state_index`.
    pub fn effective_bg_argb(&self) -> u32 {
        if let Some(state) = self.states.get(self.state_index as usize) {
            if let Some(bg) = state.bg_argb {
                return bg;
            }
        }
        self.bg_argb
    }

    /// Convert this control to its protobuf wire format.
    pub fn to_proto(&self) -> nd_proto::v1::Control {
        nd_proto::v1::Control {
            id: self.id.clone(),
            kind: nd_proto::v1::ControlKind::from(self.kind) as i32,
            col: self.col,
            row: self.row,
            span_w: self.span_w.max(1),
            span_h: self.span_h.max(1),
            icon_hash: self.effective_icon_hash().to_string(),
            title: self.effective_title().to_string(),
            subtitle: self.effective_subtitle().to_string(),
            fg_argb: self.effective_fg_argb(),
            bg_argb: self.effective_bg_argb(),
            state_index: self.state_index,
            value: self.value,
            horizontal: self.horizontal,
            meter: self.meter,
            level: self.level,
            dimmed: self.dimmed,
            face: match self.face {
                ControlFace::Default => nd_proto::v1::ControlFace::Default,
                ControlFace::Media => nd_proto::v1::ControlFace::Media,
                ControlFace::Stream => nd_proto::v1::ControlFace::Stream,
                ControlFace::Camera => nd_proto::v1::ControlFace::Camera,
            } as i32,
            detail: self.detail.clone(),
        }
    }
}

/// The event that triggers an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    #[default]
    KeyDown,
    KeyUp,
    LongPress,
    DoubleTap,
    /// Any dial rotation; the action receives the signed detent delta.
    DialRotate,
    /// Clockwise rotation; the action runs once per detent (capped).
    DialRotateCw,
    /// Counter-clockwise rotation; the action runs once per detent (capped).
    DialRotateCcw,
    DialPress,
    DialRelease,
    SliderValue,
    TouchStart,
    TouchEnd,
}

impl TriggerKind {
    /// Try converting from protobuf `InputKind`.
    pub fn from_proto_input_kind(kind: nd_proto::v1::InputKind) -> Option<Self> {
        match kind {
            nd_proto::v1::InputKind::KeyDown => Some(TriggerKind::KeyDown),
            nd_proto::v1::InputKind::KeyUp => Some(TriggerKind::KeyUp),
            nd_proto::v1::InputKind::LongPress => Some(TriggerKind::LongPress),
            nd_proto::v1::InputKind::DoubleTap => Some(TriggerKind::DoubleTap),
            nd_proto::v1::InputKind::DialRotate => Some(TriggerKind::DialRotate),
            nd_proto::v1::InputKind::DialPress => Some(TriggerKind::DialPress),
            nd_proto::v1::InputKind::DialRelease => Some(TriggerKind::DialRelease),
            nd_proto::v1::InputKind::SliderValue => Some(TriggerKind::SliderValue),
            nd_proto::v1::InputKind::TouchStart => Some(TriggerKind::TouchStart),
            nd_proto::v1::InputKind::TouchEnd => Some(TriggerKind::TouchEnd),
            nd_proto::v1::InputKind::Unspecified => None,
        }
    }
}

/// Executable action definition (SPEC §6.4).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Hotkey / key combination (modifiers down, key tapped, modifiers up).
    Hotkey { combo: KeyCombo },
    /// Type arbitrary Unicode text on the host.
    TypeText { text: String },
    /// Open a URL in the host's default web browser.
    OpenUrl { url: String },
    /// Launch an application or file with optional arguments.
    LaunchApp {
        path: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// User reviewed this command line (ADR 0002). Unconfirmed launches are refused.
        #[serde(default)]
        confirmed: bool,
    },
    /// Navigate to a specific page by ID.
    GoToPage { page_id: String },
    /// Navigate to the next page in the profile.
    NextPage,
    /// Navigate to the previous page in the profile.
    PrevPage,
    /// Open a sub-page folder (pushes current page to back stack).
    OpenFolder { folder_page_id: String },
    /// Go back to the previous page in folder navigation stack.
    GoBack,
    /// Toggle or cycle control state index.
    ToggleState,
    /// Run several actions in order, each optionally preceded by a delay (SPEC §6.4).
    Multi { steps: Vec<MultiStep> },
    /// Change volume by `step` (linear 0..1 units). On a dial the step is multiplied
    /// by the signed detent delta; on a key it is applied once.
    AdjustVolume {
        #[serde(default)]
        target: AudioTarget,
        step: f32,
    },
    /// Set volume to `level`, or to the slider/dial value of the event when `level` is absent.
    SetVolume {
        #[serde(default)]
        target: AudioTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<f32>,
    },
    /// Toggle mute. The control's `state_index` becomes 1 when muted, 0 when not.
    ToggleMute {
        #[serde(default)]
        target: AudioTarget,
    },
    /// Change one app's volume by `step` (linear 0..1 units); dial: multiplied by the signed detent delta (round 12).
    AdjustAppVolume { app: AudioApp, step: f32 },
    /// Set one app's volume to `level`, or to the slider/dial value of the event when `level` is absent.
    SetAppVolume {
        app: AudioApp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<f32>,
    },
    /// Toggle one app's mute. The control's `state_index` becomes 1 when muted.
    ToggleAppMute { app: AudioApp },
    /// Webcam on/off. With no app using the camera it starts/stops the manual (test) stream;
    /// while an app uses it, it pauses/resumes (the app then sees the "not connected" picture).
    /// The control's `state_index` becomes 1 when the camera is on.
    CameraToggle,
    /// Switch between the back and front lens.
    CameraSwitchLens,
    /// Toggle the phone torch. `state_index` becomes 1 when on.
    CameraTorch,
    /// Zoom the phone camera. On a dial the step is multiplied by the signed detent delta
    /// (ratio units, clamped to 1.0..8.0); on a key it is applied once.
    CameraZoom {
        #[serde(default = "default_zoom_step")]
        step: f32,
    },
    /// Round 25: phone microphone on/off. `state_index` 1 when on.
    MicToggle,
    /// Round 25: mute/unmute the phone microphone on the PC side. `state_index` 1 when muted.
    MicMute,
    /// Show the main profile (auto-switch `home_profile_id`, else its fallback, else the first profile).
    /// With `pause_auto_switch` auto-switch is paused until resumed; without it, auto-switch stays on and
    /// the main profile stays until focus moves to an app with a different target (SPEC §6.3).
    GoHome {
        #[serde(default)]
        pause_auto_switch: bool,
    },
    /// Round 22: switch the deck to another profile (its initial page). Like `go_home`, auto-switch stays on and
    /// the choice sticks until focus moves to an app with a different target. Every profile switch the agent
    /// makes (this, Home, auto-switch) is recorded so `back` returns to the profile shown before.
    SwitchProfile { to: ProfileTarget },
    /// Pause / resume / toggle auto-switch. `state_index` becomes 1 while paused.
    AutoSwitchPause {
        #[serde(default)]
        mode: PauseMode,
    },
    // ---- OBS Studio (round 14, M8; semantics in `crate::obs`) ----
    /// Switch to a scene (program; preview in studio mode when `preview`). `state_index` 1.
    ObsScene {
        scene: ObsScene,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        preview: bool,
    },
    /// Show / hide / toggle a source in a scene (`scene` absent = current program scene).
    ObsSourceVisibility {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene: Option<ObsScene>,
        source: String,
        #[serde(default)]
        mode: ObsToggle,
    },
    /// Start / stop / toggle streaming. `state_index` 1 while live.
    ObsStream {
        #[serde(default)]
        mode: ObsToggle,
    },
    /// Start / stop / toggle recording. `state_index` 1 while recording.
    ObsRecord {
        #[serde(default)]
        mode: ObsToggle,
    },
    /// Pause / resume / toggle the running recording. `state_index` 2 paused, 1 recording.
    ObsRecordPause {
        #[serde(default)]
        mode: ObsToggle,
    },
    /// Start / stop / toggle the replay buffer. `state_index` 1 while active.
    ObsReplayBuffer {
        #[serde(default)]
        mode: ObsToggle,
    },
    /// Save the replay buffer.
    ObsSaveReplay,
    /// Start / stop / toggle OBS's virtual camera. `state_index` 1 while active.
    ObsVirtualCam {
        #[serde(default)]
        mode: ObsToggle,
    },
    /// Studio mode on / off / toggle. `state_index` 1 when on.
    ObsStudioMode {
        #[serde(default)]
        mode: ObsToggle,
    },
    /// Studio mode: transition preview → program.
    ObsStudioTransition,
    /// Mute / unmute / toggle an input. `state_index` 1 when muted.
    ObsInputMute {
        input: ObsInput,
        #[serde(default)]
        mode: ObsToggle,
    },
    /// Set an input's fader (0..1) to `level`, or to the slider/dial value when absent.
    ObsSetInputVolume {
        input: ObsInput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<f32>,
    },
    /// Move an input's fader by `step` (dial: × signed detent delta; key: once).
    ObsAdjustInputVolume { input: ObsInput, step: f32 },
    // ---- media session control (round 17) ----
    /// Transport command to the current media session (`MediaInfo::control`). When the backend returns
    /// `Unsupported` (or there is no player), falls back to the matching media hotkey (`play` / `pause` fall back
    /// to `media_play_pause` only on `Unsupported`, never when there is just no player).
    MediaTransport { command: MediaTransportCmd },
    /// Seek to `position` (fraction 0..1 of the track length); absent = the event value (slider value, or the
    /// control's stored value for other triggers, so `touch_end` on a slider seeks once to the released
    /// position). Needs a known length. Outcome `SetValue(fraction)`.
    MediaSeek {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<f32>,
    },
    /// Seek by `seconds` (negative = back); on a dial multiplied by the signed detent delta, on a key once.
    MediaSeekBy { seconds: f32 },
    /// Shuffle on / off / toggle (toggle reads `NowPlaying::shuffle`, unknown = off). `state_index` 1 when on.
    MediaShuffle {
        #[serde(default)]
        mode: ObsToggle,
    },
    /// Repeat mode: cycle off → list → track → off, or set one. `state_index` 0 off, 1 list, 2 track.
    MediaRepeat {
        #[serde(default)]
        mode: RepeatAction,
    },
    // ---- plugins (round 19, M9; seam in `crate::plugin`, protocol in docs/PLUGINS.md) ----
    /// Forward the event to plugin `plugin`'s action `action`. `settings` is this instance's settings
    /// object (edited in the config UI from the manifest's fields, or by the plugin via `setSettings`).
    Plugin {
        plugin: String,
        action: String,
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        settings: crate::plugin::PluginSettings,
    },
}

/// Round 17: transport command of [`Action::MediaTransport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaTransportCmd {
    PlayPause,
    Play,
    Pause,
    Next,
    Prev,
    Stop,
}

/// Round 17: what [`Action::MediaRepeat`] does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatAction {
    /// off → list → track → off (unknown current mode counts as off).
    #[default]
    Cycle,
    Off,
    List,
    Track,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseMode {
    #[default]
    Toggle,
    Pause,
    Resume,
}

fn default_zoom_step() -> f32 {
    0.25
}

/// Live data a control displays (SPEC §6.4 Tiles / Audio / Media). The host samples
/// sources for the visible page and patches `value`, `title`, `subtitle`, `state_index`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataSource {
    /// Local time `HH:MM` as title, weekday + date as subtitle.
    Clock,
    /// CPU usage: value 0..1, title like `37%`.
    Cpu,
    /// Memory usage: value 0..1, title like `6.1 GB`.
    Memory,
    /// Network throughput: title down rate, subtitle up rate.
    Network,
    /// Endpoint volume: value 0..1, subtitle like `45%`.
    Volume {
        #[serde(default)]
        target: AudioTarget,
    },
    /// Endpoint mute: `state_index` 1 when muted.
    Mute {
        #[serde(default)]
        target: AudioTarget,
    },
    /// Now playing: title = track, subtitle = artist, value = progress, state 1 when playing.
    /// Round 16: also detail = `elapsed / length` (`1:23 / 3:45`, or empty when unknown) and dimmed when no
    /// player is active; with `art`, icon_hash = album art (imported into the icon store, `""` when none).
    NowPlaying {
        #[serde(default, skip_serializing_if = "is_false")]
        art: bool,
    },
    /// Round 16: play state only, for a play/pause key: `state_index` 1 while playing, dimmed when no
    /// player is active. Title, subtitle and icon are left to the control's states.
    MediaPlaying,
    /// Webcam on (`state_index` 1) or off (0).
    CameraState,
    /// Phone torch on (`state_index` 1) or off (0).
    CameraTorch,
    /// Phone camera zoom: value 0..1 (1x..8x), subtitle like `2.5x`.
    CameraZoom,
    /// Round 25: phone microphone on (`state_index` 1) or off (0).
    MicState,
    /// Round 25: phone microphone muted (`state_index` 1) or not (0).
    MicMuted,
    /// Auto-switch paused (`state_index` 1) or running (0).
    AutoSwitchPaused,
    /// Round 22: where a `switch_profile` with the same target would go: subtitle = that profile's name,
    /// `state_index` 1 when it is the profile on screen (for `profile` targets), dimmed when there is nowhere
    /// to go (`back` with no history, or the profile was deleted). Answered by `ProfileActions::destination`.
    ProfileDestination { to: ProfileTarget },
    /// Round 22: title = name of the profile on screen.
    CurrentProfile,
    /// One app's audio (round 12, M4): value = volume, subtitle like `45%`, `state_index` 1 when muted,
    /// live level meter. For `focused` / `slot` targets also title = app name and icon = app icon.
    /// No matching app: dimmed, value 0, subtitle empty (and empty title/icon for `focused` / `slot`).
    AppAudio { app: AudioApp },
    // ---- OBS Studio (round 14, M8; semantics in `crate::obs`) ----
    /// A scene: state 1 program, 2 preview; slot targets also set title = scene name.
    ObsScene { scene: ObsScene },
    /// Streaming: state 1 live, subtitle = duration.
    ObsStream,
    /// Recording: state 1 recording, 2 paused, subtitle = duration.
    ObsRecord,
    /// Replay buffer active (state 1).
    ObsReplayBuffer,
    /// OBS virtual camera active (state 1).
    ObsVirtualCam,
    /// Studio mode on (state 1).
    ObsStudioMode,
    /// A source's visibility in a scene (`scene` absent = current program scene): state 1 visible.
    ObsSourceVisibility {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scene: Option<ObsScene>,
        source: String,
    },
    /// An input: value = fader, subtitle = dB, state 1 muted.
    ObsInput { input: ObsInput },
    /// Connection: state 1 connected, subtitle = short status otherwise. Never dimmed.
    ObsStatus,
    // ---- media session (round 17) ----
    /// Shuffle: `state_index` 1 when on; dimmed when no player is active or it does not report shuffle.
    MediaShuffle,
    /// Repeat: `state_index` 0 off, 1 list, 2 track; dimmed when no player or not reported.
    MediaRepeat,
}

/// Round 22: destination of [`Action::SwitchProfile`]. JSON: `{"by":"profile","id":"p_…"}`, `{"by":"next"}`,
/// `{"by":"previous"}`, `{"by":"back"}`. Next / previous follow the profile list order (`list_profiles`) and wrap.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum ProfileTarget {
    Profile {
        id: String,
    },
    Next,
    Previous,
    /// The profile shown before the current one (a stack: pressing back repeatedly walks further back).
    Back,
}

/// Which app a per-app audio action or source targets (round 12, M4). See `crate::mixer`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum AudioApp {
    /// The app whose key (`nd_platform::app_key`) equals `app_key(name)`: `spotify`, `Discord.exe`, `firefox`.
    Name { name: String },
    /// The app owning the focused window (matched by its process' app key). Nothing if it has no audio session.
    Focused,
    /// The `index`-th visible app in mixer order (0-based), see `crate::mixer` for the order.
    Slot { index: u32 },
    /// Round 17: the app of the current media player (`NowPlaying::app` matched through
    /// `nd_platform::player_app_keys`, hidden apps too); see `crate::mixer` for the fallback rule.
    Media,
}

/// One step of an [`Action::Multi`] sequence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiStep {
    /// Action to run (nested `multi` actions are rejected by the router).
    pub action: Action,
    /// Delay before this step runs, in milliseconds.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub delay_ms: u64,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

impl MultiStep {
    pub fn new(action: Action) -> Self {
        Self { action, delay_ms: 0 }
    }

    pub fn after(delay_ms: u64, action: Action) -> Self {
        Self { action, delay_ms }
    }
}

impl<'de> Deserialize<'de> for Action {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum RawAction {
            Hotkey {
                combo: Option<KeyCombo>,
                key: Option<Key>,
                #[serde(default)]
                modifiers: Vec<Key>,
            },
            TypeText {
                text: String,
            },
            OpenUrl {
                url: String,
            },
            LaunchApp {
                path: String,
                #[serde(default)]
                args: Vec<String>,
                #[serde(default)]
                confirmed: bool,
            },
            GoToPage {
                page_id: String,
            },
            NextPage,
            PrevPage,
            OpenFolder {
                folder_page_id: String,
            },
            GoBack,
            ToggleState,
            Multi {
                steps: Vec<MultiStep>,
            },
            AdjustVolume {
                #[serde(default)]
                target: AudioTarget,
                step: f32,
            },
            SetVolume {
                #[serde(default)]
                target: AudioTarget,
                #[serde(default)]
                level: Option<f32>,
            },
            ToggleMute {
                #[serde(default)]
                target: AudioTarget,
            },
            AdjustAppVolume {
                app: AudioApp,
                step: f32,
            },
            SetAppVolume {
                app: AudioApp,
                #[serde(default)]
                level: Option<f32>,
            },
            ToggleAppMute {
                app: AudioApp,
            },
            CameraToggle,
            CameraSwitchLens,
            CameraTorch,
            CameraZoom {
                #[serde(default = "default_zoom_step")]
                step: f32,
            },
            MicToggle,
            MicMute,
            GoHome {
                #[serde(default)]
                pause_auto_switch: bool,
            },
            SwitchProfile {
                to: ProfileTarget,
            },
            AutoSwitchPause {
                #[serde(default)]
                mode: PauseMode,
            },
            ObsScene {
                scene: ObsScene,
                #[serde(default)]
                preview: bool,
            },
            ObsSourceVisibility {
                #[serde(default)]
                scene: Option<ObsScene>,
                source: String,
                #[serde(default)]
                mode: ObsToggle,
            },
            ObsStream {
                #[serde(default)]
                mode: ObsToggle,
            },
            ObsRecord {
                #[serde(default)]
                mode: ObsToggle,
            },
            ObsRecordPause {
                #[serde(default)]
                mode: ObsToggle,
            },
            ObsReplayBuffer {
                #[serde(default)]
                mode: ObsToggle,
            },
            ObsSaveReplay,
            ObsVirtualCam {
                #[serde(default)]
                mode: ObsToggle,
            },
            ObsStudioMode {
                #[serde(default)]
                mode: ObsToggle,
            },
            ObsStudioTransition,
            ObsInputMute {
                input: ObsInput,
                #[serde(default)]
                mode: ObsToggle,
            },
            ObsSetInputVolume {
                input: ObsInput,
                #[serde(default)]
                level: Option<f32>,
            },
            ObsAdjustInputVolume {
                input: ObsInput,
                step: f32,
            },
            MediaTransport {
                command: MediaTransportCmd,
            },
            MediaSeek {
                #[serde(default)]
                position: Option<f32>,
            },
            MediaSeekBy {
                seconds: f32,
            },
            MediaShuffle {
                #[serde(default)]
                mode: ObsToggle,
            },
            MediaRepeat {
                #[serde(default)]
                mode: RepeatAction,
            },
            Plugin {
                plugin: String,
                action: String,
                #[serde(default)]
                settings: crate::plugin::PluginSettings,
            },
        }

        let raw = RawAction::deserialize(deserializer)?;
        match raw {
            RawAction::Hotkey { combo, key, modifiers } => {
                let combo = if let Some(c) = combo {
                    c
                } else if let Some(k) = key {
                    KeyCombo { modifiers, key: k }
                } else {
                    return Err(serde::de::Error::custom("hotkey requires either 'combo' or 'key'"));
                };
                Ok(Action::Hotkey { combo })
            }
            RawAction::TypeText { text } => Ok(Action::TypeText { text }),
            RawAction::OpenUrl { url } => Ok(Action::OpenUrl { url }),
            RawAction::LaunchApp { path, args, confirmed } => Ok(Action::LaunchApp { path, args, confirmed }),
            RawAction::GoToPage { page_id } => Ok(Action::GoToPage { page_id }),
            RawAction::NextPage => Ok(Action::NextPage),
            RawAction::PrevPage => Ok(Action::PrevPage),
            RawAction::OpenFolder { folder_page_id } => Ok(Action::OpenFolder { folder_page_id }),
            RawAction::GoBack => Ok(Action::GoBack),
            RawAction::ToggleState => Ok(Action::ToggleState),
            RawAction::Multi { steps } => Ok(Action::Multi { steps }),
            RawAction::AdjustVolume { target, step } => Ok(Action::AdjustVolume { target, step }),
            RawAction::SetVolume { target, level } => Ok(Action::SetVolume { target, level }),
            RawAction::ToggleMute { target } => Ok(Action::ToggleMute { target }),
            RawAction::AdjustAppVolume { app, step } => Ok(Action::AdjustAppVolume { app, step }),
            RawAction::SetAppVolume { app, level } => Ok(Action::SetAppVolume { app, level }),
            RawAction::ToggleAppMute { app } => Ok(Action::ToggleAppMute { app }),
            RawAction::CameraToggle => Ok(Action::CameraToggle),
            RawAction::CameraSwitchLens => Ok(Action::CameraSwitchLens),
            RawAction::CameraTorch => Ok(Action::CameraTorch),
            RawAction::CameraZoom { step } => Ok(Action::CameraZoom { step }),
            RawAction::MicToggle => Ok(Action::MicToggle),
            RawAction::MicMute => Ok(Action::MicMute),
            RawAction::GoHome { pause_auto_switch } => Ok(Action::GoHome { pause_auto_switch }),
            RawAction::SwitchProfile { to } => Ok(Action::SwitchProfile { to }),
            RawAction::AutoSwitchPause { mode } => Ok(Action::AutoSwitchPause { mode }),
            RawAction::ObsScene { scene, preview } => Ok(Action::ObsScene { scene, preview }),
            RawAction::ObsSourceVisibility { scene, source, mode } => {
                Ok(Action::ObsSourceVisibility { scene, source, mode })
            }
            RawAction::ObsStream { mode } => Ok(Action::ObsStream { mode }),
            RawAction::ObsRecord { mode } => Ok(Action::ObsRecord { mode }),
            RawAction::ObsRecordPause { mode } => Ok(Action::ObsRecordPause { mode }),
            RawAction::ObsReplayBuffer { mode } => Ok(Action::ObsReplayBuffer { mode }),
            RawAction::ObsSaveReplay => Ok(Action::ObsSaveReplay),
            RawAction::ObsVirtualCam { mode } => Ok(Action::ObsVirtualCam { mode }),
            RawAction::ObsStudioMode { mode } => Ok(Action::ObsStudioMode { mode }),
            RawAction::ObsStudioTransition => Ok(Action::ObsStudioTransition),
            RawAction::ObsInputMute { input, mode } => Ok(Action::ObsInputMute { input, mode }),
            RawAction::ObsSetInputVolume { input, level } => Ok(Action::ObsSetInputVolume { input, level }),
            RawAction::ObsAdjustInputVolume { input, step } => Ok(Action::ObsAdjustInputVolume { input, step }),
            RawAction::MediaTransport { command } => Ok(Action::MediaTransport { command }),
            RawAction::MediaSeek { position } => Ok(Action::MediaSeek { position }),
            RawAction::MediaSeekBy { seconds } => Ok(Action::MediaSeekBy { seconds }),
            RawAction::MediaShuffle { mode } => Ok(Action::MediaShuffle { mode }),
            RawAction::MediaRepeat { mode } => Ok(Action::MediaRepeat { mode }),
            RawAction::Plugin { plugin, action, settings } => Ok(Action::Plugin { plugin, action, settings }),
        }
    }
}

impl Action {
    /// Create a hotkey action with no modifiers.
    pub fn hotkey(key: Key) -> Self {
        Self::Hotkey { combo: KeyCombo { modifiers: Vec::new(), key } }
    }

    /// Create a hotkey action with modifiers.
    pub fn hotkey_with_mods(modifiers: Vec<Key>, key: Key) -> Self {
        Self::Hotkey { combo: KeyCombo { modifiers, key } }
    }

    /// Create a type text action.
    pub fn type_text(text: impl Into<String>) -> Self {
        Self::TypeText { text: text.into() }
    }

    /// Create an open URL action.
    pub fn open_url(url: impl Into<String>) -> Self {
        Self::OpenUrl { url: url.into() }
    }

    /// Create a launch app action.
    ///
    /// Not confirmed: the router refuses it until `confirmed` is set (ADR 0002).
    pub fn launch_app(path: impl Into<String>, args: Vec<String>) -> Self {
        Self::LaunchApp { path: path.into(), args, confirmed: false }
    }

    /// Create a launch app action the user has already confirmed.
    pub fn launch_app_confirmed(path: impl Into<String>, args: Vec<String>) -> Self {
        Self::LaunchApp { path: path.into(), args, confirmed: true }
    }

    /// Create a multi-action from steps.
    pub fn multi(steps: Vec<MultiStep>) -> Self {
        Self::Multi { steps }
    }

    /// Create a go to page action.
    pub fn go_to_page(page_id: impl Into<String>) -> Self {
        Self::GoToPage { page_id: page_id.into() }
    }

    /// Create an open folder action.
    pub fn open_folder(folder_page_id: impl Into<String>) -> Self {
        Self::OpenFolder { folder_page_id: folder_page_id.into() }
    }
}

/// An action paired with its trigger condition.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionDefinition {
    /// The action to execute.
    pub action: Action,

    /// Event trigger (default: KeyDown).
    pub trigger: TriggerKind,
}

impl ActionDefinition {
    /// Create an action definition that fires on `TriggerKind::KeyDown`.
    pub fn new(action: Action) -> Self {
        Self { action, trigger: TriggerKind::KeyDown }
    }

    /// Create an action definition with an explicit trigger.
    pub fn with_trigger(action: Action, trigger: TriggerKind) -> Self {
        Self { action, trigger }
    }
}

impl Serialize for ActionDefinition {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct Helper<'a> {
            action: &'a Action,
            trigger: TriggerKind,
        }

        Helper { action: &self.action, trigger: self.trigger }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ActionDefinition {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("action").is_some() {
            #[derive(Deserialize)]
            struct Helper {
                action: Action,
                #[serde(default)]
                trigger: TriggerKind,
            }
            let h = Helper::deserialize(value).map_err(serde::de::Error::custom)?;
            Ok(ActionDefinition { action: h.action, trigger: h.trigger })
        } else {
            #[derive(Deserialize)]
            struct HelperWithTrigger {
                #[serde(default)]
                trigger: TriggerKind,
            }
            let trigger = HelperWithTrigger::deserialize(&value).map(|h| h.trigger).unwrap_or_default();
            let action = Action::deserialize(value).map_err(serde::de::Error::custom)?;
            Ok(ActionDefinition { action, trigger })
        }
    }
}
