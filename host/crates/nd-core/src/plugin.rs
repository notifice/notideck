// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin seam (SPEC §8, round 19 / M9). Protocol details: `docs/PLUGINS.md`.
//!
//! nd-core only knows this trait; the out-of-process host lives in `nd-plugin`. A control runs a plugin
//! action through [`crate::Action::Plugin`]; the router forwards the event here. Plugin-driven key looks
//! (title, image, state, value) come back through [`PluginControl::sample`] on the live tick.

use crate::live::LiveSample;
use crate::model::{ControlKind, TriggerKind};
use serde_json::{Map, Value};

/// Per-instance plugin settings (the JSON object stored in the action).
pub type PluginSettings = Map<String, Value>;

/// Context prefix for rail controls; page controls use the profile id (see [`plugin_context`]).
pub const RAIL_CONTEXT: &str = "rail";

/// Context string for one placed plugin action: `"{profile_id}:{control_id}"`, or `"rail:{control_id}"`
/// for rail keys. Unique across profiles even when a duplicated profile reuses control ids.
pub fn plugin_context(profile_or_rail: &str, control_id: &str) -> String {
    format!("{profile_or_rail}:{control_id}")
}

/// Split a context back into `(profile_id_or_"rail", control_id)`. Control ids never contain ':'.
pub fn split_context(context: &str) -> Option<(&str, &str)> {
    let (a, b) = context.rsplit_once(':')?;
    (!a.is_empty() && !b.is_empty()).then_some((a, b))
}

/// What happened on the control (maps 1:1 to the protocol's agent → plugin notifications).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PluginEventKind {
    KeyDown,
    KeyUp,
    LongPress,
    DoubleTap,
    /// Signed detents (+ = clockwise). `dial_rotate_cw/ccw` bindings arrive as ±1 per detent.
    DialRotate {
        ticks: i32,
    },
    DialPress,
    DialRelease,
    /// Slider value 0..1.
    SliderChange {
        value: f32,
    },
    TouchStart,
    TouchEnd,
}

impl PluginEventKind {
    /// Event kind for a binding's trigger. `delta` is the router's signed detents, `value` the slider value.
    pub fn from_trigger(trigger: TriggerKind, delta: i32, value: f32) -> Self {
        match trigger {
            TriggerKind::KeyDown => Self::KeyDown,
            TriggerKind::KeyUp => Self::KeyUp,
            TriggerKind::LongPress => Self::LongPress,
            TriggerKind::DoubleTap => Self::DoubleTap,
            TriggerKind::DialRotate | TriggerKind::DialRotateCw | TriggerKind::DialRotateCcw => {
                Self::DialRotate { ticks: delta }
            }
            TriggerKind::DialPress => Self::DialPress,
            TriggerKind::DialRelease => Self::DialRelease,
            TriggerKind::SliderValue => Self::SliderChange { value },
            TriggerKind::TouchStart => Self::TouchStart,
            TriggerKind::TouchEnd => Self::TouchEnd,
        }
    }

    /// JSON-RPC method name sent to the plugin.
    pub fn method(&self) -> &'static str {
        match self {
            Self::KeyDown => "key.down",
            Self::KeyUp => "key.up",
            Self::LongPress => "key.longPress",
            Self::DoubleTap => "key.doubleTap",
            Self::DialRotate { .. } => "dial.rotate",
            Self::DialPress => "dial.press",
            Self::DialRelease => "dial.release",
            Self::SliderChange { .. } => "slider.change",
            Self::TouchStart => "touch.start",
            Self::TouchEnd => "touch.end",
        }
    }
}

/// One input event for a plugin action.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginEvent {
    pub context: String,
    pub plugin: String,
    pub action: String,
    pub settings: PluginSettings,
    pub kind: PluginEventKind,
}

/// A plugin action that is currently on screen (on some phone's active page, or on the rail).
#[derive(Debug, Clone, PartialEq)]
pub struct PluginInstance {
    pub context: String,
    pub plugin: String,
    pub action: String,
    pub settings: PluginSettings,
    pub control: ControlKind,
    /// The control's current state index and value (sent with `action.willAppear`).
    pub state: u32,
    pub value: f32,
}

/// The agent-side plugin host as nd-core sees it. All calls are cheap and non-blocking
/// (they queue work for the host's own tasks); they are called with the session lock held.
pub trait PluginControl: Send + Sync {
    /// Forward an input event. Unknown / stopped plugins: drop it silently.
    fn event(&self, event: PluginEvent);

    /// The full set of on-screen plugin instances (union over all sessions + rail), called on every
    /// live tick and after layout changes. The host diffs it against the previous call and sends
    /// `action.willAppear` / `action.willDisappear` / `settings.didChange`.
    fn set_visible(&self, instances: Vec<PluginInstance>);

    /// Plugin-provided look for `context` (only fields the plugin set are `Some`). Sets `dimmed: Some(true)`
    /// while the plugin is not running, `Some(false)` once it is. `None` = nothing known for this context.
    fn sample(&self, context: &str) -> Option<LiveSample>;
}

/// No plugin support: events are dropped, nothing is sampled.
pub struct NullPlugins;

impl PluginControl for NullPlugins {
    fn event(&self, _event: PluginEvent) {}
    fn set_visible(&self, _instances: Vec<PluginInstance>) {}
    fn sample(&self, _context: &str) -> Option<LiveSample> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_round_trip() {
        let c = plugin_context("p_abc", "c_123");
        assert_eq!(c, "p_abc:c_123");
        assert_eq!(split_context(&c), Some(("p_abc", "c_123")));
        assert_eq!(split_context(&plugin_context(RAIL_CONTEXT, "k1")), Some(("rail", "k1")));
        assert_eq!(split_context("nocolon"), None);
        assert_eq!(split_context(":x"), None);
    }

    #[test]
    fn trigger_mapping() {
        assert_eq!(
            PluginEventKind::from_trigger(TriggerKind::DialRotateCcw, -1, 0.0),
            PluginEventKind::DialRotate { ticks: -1 }
        );
        assert_eq!(PluginEventKind::from_trigger(TriggerKind::SliderValue, 0, 0.25).method(), "slider.change");
        assert_eq!(PluginEventKind::from_trigger(TriggerKind::LongPress, 0, 0.0).method(), "key.longPress");
    }
}
