// SPDX-License-Identifier: GPL-3.0-or-later
//! OBS Studio deck actions and live state (round 14, M8, SPEC §6.4 OBS).
//!
//! The agent implements [`ObsControl`] with `nd_obs::ObsClient` (obs-websocket v5); core only sees the trait,
//! a cached [`ObsSnapshot`] and fire-and-forget [`ObsCommand`]s.
//!
//! # Targets (zero-setup templates)
//! - [`ObsScene::Slot`] = the `index`-th scene in OBS's scene list as the OBS UI shows it, top to bottom
//!   (`ObsSnapshot::scenes` is already in that order). A template page uses slots, so it works with any scene names.
//! - [`ObsInput::Special`] = OBS's built-in global audio devices (`GetSpecialInputs`: desktop1/2, mic1..4), so
//!   "Mic" / "Desktop audio" keys work whatever the inputs are called (they are localized).
//!
//! # Semantics (R14-2 implements; tests with a mock `ObsControl`)
//! - Not connected (`status != Connected`): every action fails with `"OBS is not connected"`; every OBS source
//!   samples `dimmed = true` (state 0, value 0, subtitle `""`; `ObsScene` slot sources also title `""`).
//! - A target that does not resolve (no such scene/slot/input/item): action fails with a short message
//!   (`"OBS scene slot 4 is empty"`, `"OBS input 'Mic/Aux' not found"`), source samples dimmed like above.
//! - Actions return optimistic outcomes (the next live tick corrects them; the agent pokes the live tick on
//!   every OBS event, so keys update within ~100 ms):
//!   - `obs_scene` → `SetState(1)`; in studio mode with `preview: true` it sets the preview scene instead
//!     (`preview: true` outside studio mode = program scene).
//!   - `obs_stream` / `obs_record` / `obs_replay_buffer` / `obs_virtual_cam` / `obs_studio_mode` with
//!     [`ObsToggle`] → `SetState(new_active)`. `On` when already active (or `Off` when inactive) sends nothing
//!     and returns the current state.
//!   - `obs_record_pause` → `SetState(2)` when now paused, `1` when resumed; fails when not recording.
//!   - `obs_save_replay` → `Executed`; fails with `"the replay buffer is off"` when inactive.
//!   - `obs_source_visibility` (scene `None` = current program scene) → `SetState(now_visible)`.
//!   - `obs_input_mute` → `SetState(now_muted)`.
//!   - `obs_set_input_volume` (level or slider value, 0..1 fader units) → `SetValue(v)`;
//!     `obs_adjust_input_volume` (step × dial delta, keys: once) → `SetValue(clamped new value)`.
//!   - `obs_studio_transition` → `Executed`; fails with `"studio mode is off"` when off.
//! - Sources (all set `dimmed: Some(false)` when they resolve):
//!   - `obs_scene`: state 1 = program scene, 2 = preview scene (studio mode, not also program), else 0.
//!     Slot targets also set title = scene name.
//!   - `obs_stream` / `obs_record`: state 0 off / 1 active / 2 paused (record only); subtitle = duration
//!     `M:SS` or `H:MM:SS` while active, `""` when off.
//!   - `obs_replay_buffer`, `obs_virtual_cam`, `obs_studio_mode`: state 1 when active.
//!   - `obs_source_visibility`: state 1 when visible.
//!   - `obs_input`: value = fader value ([`mul_to_fader`]), subtitle = [`format_db`], state 1 when muted.
//!   - `obs_status`: state 1 when connected, subtitle `""` when connected, else a short status
//!     (`"offline"`, `"connecting"`, `"wrong password"`, `"server off"`, `"disabled"`). Never dimmed.

use crate::live::LiveSample;
use crate::model::{Action, DataSource};
use crate::router::{ActionOutcome, InputContext};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which scene an OBS action or source targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum ObsScene {
    Name {
        name: String,
    },
    /// 0-based position in the OBS scene list (top to bottom).
    Slot {
        index: u32,
    },
}

/// OBS's built-in global audio devices (Settings → Audio).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecialInput {
    Desktop1,
    Desktop2,
    Mic1,
    Mic2,
    Mic3,
    Mic4,
}

impl SpecialInput {
    /// Key in obs-websocket's `GetSpecialInputs` response (`desktop1`, `mic1`, …).
    pub fn ws_key(self) -> &'static str {
        match self {
            SpecialInput::Desktop1 => "desktop1",
            SpecialInput::Desktop2 => "desktop2",
            SpecialInput::Mic1 => "mic1",
            SpecialInput::Mic2 => "mic2",
            SpecialInput::Mic3 => "mic3",
            SpecialInput::Mic4 => "mic4",
        }
    }
}

/// Which input (audio source) an OBS action or source targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum ObsInput {
    Name { name: String },
    Special { which: SpecialInput },
}

/// Toggle / force on / force off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObsToggle {
    #[default]
    Toggle,
    On,
    Off,
}

/// Connection state of the OBS client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObsStatus {
    /// OBS integration switched off in Notideck's settings.
    #[default]
    Disabled,
    /// Trying to connect (first attempt or reconnecting after a drop).
    Connecting,
    Connected,
    /// OBS answered but rejected the password.
    AuthFailed,
    /// OBS's websocket settings say the server is switched off.
    ServerDisabled,
    /// Nothing listening (OBS not running).
    Offline,
}

/// An output's state (stream, record, replay buffer, virtual camera).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OutputState {
    pub active: bool,
    /// Record only.
    pub paused: bool,
    /// Time active, excluding pauses, at snapshot time. 0 when inactive.
    pub duration_ms: u64,
}

/// One input as obs-websocket reports it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ObsInputInfo {
    pub name: String,
    /// `inputKind`, e.g. `wasapi_input_capture`, `pulse_output_capture`.
    pub kind: String,
    /// Audio inputs only (`None` for video-only inputs).
    pub muted: Option<bool>,
    /// `inputVolumeMul` (linear, 1.0 = 0 dB). Audio inputs only.
    pub volume_mul: Option<f32>,
}

/// One scene item (a source placed in a scene).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObsSceneItem {
    pub source: String,
    pub item_id: i64,
    pub enabled: bool,
}

/// Cached OBS state. Cheap to take (the client keeps it current from events).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ObsSnapshot {
    pub status: ObsStatus,
    pub studio_mode: bool,
    pub program_scene: Option<String>,
    /// Studio mode only.
    pub preview_scene: Option<String>,
    /// Scene names as the OBS UI lists them, top to bottom.
    pub scenes: Vec<String>,
    pub stream: OutputState,
    pub record: OutputState,
    pub replay_buffer: OutputState,
    pub virtual_cam: OutputState,
    pub inputs: Vec<ObsInputInfo>,
    /// `GetSpecialInputs`: only devices that are set (non-null) are present.
    pub special_inputs: BTreeMap<SpecialInput, String>,
    /// Scene name → its items, top to bottom.
    pub scene_items: BTreeMap<String, Vec<ObsSceneItem>>,
}

/// A resolved request the client sends to OBS. Names are concrete OBS names.
#[derive(Debug, Clone, PartialEq)]
pub enum ObsCommand {
    SetProgramScene(String),
    SetPreviewScene(String),
    SetSceneItemEnabled {
        scene: String,
        item_id: i64,
        enabled: bool,
    },
    /// `true` = StartStream, `false` = StopStream.
    Stream(bool),
    Record(bool),
    /// `true` = PauseRecord, `false` = ResumeRecord.
    RecordPause(bool),
    ReplayBuffer(bool),
    SaveReplayBuffer,
    VirtualCam(bool),
    SetStudioMode(bool),
    /// `TriggerStudioModeTransition`.
    StudioTransition,
    SetInputMute {
        input: String,
        muted: bool,
    },
    SetInputVolumeMul {
        input: String,
        mul: f32,
    },
}

/// OBS backend behind the deck actions and live sources. Methods are synchronous and quick.
pub trait ObsControl: Send + Sync {
    fn snapshot(&self) -> ObsSnapshot;
    /// Queue `cmd` (fire and forget). Err only when not connected.
    fn send(&self, cmd: ObsCommand) -> Result<(), String>;
}

/// Used when OBS integration is not attached (tests, headless).
pub struct NullObs;

impl ObsControl for NullObs {
    fn snapshot(&self) -> ObsSnapshot {
        ObsSnapshot::default()
    }
    fn send(&self, _cmd: ObsCommand) -> Result<(), String> {
        Err("OBS is not connected".into())
    }
}

/// Fader position 0..1 → `inputVolumeMul`: `v³` (≈ OBS's own fader feel; 0.5 → −18 dB).
pub fn fader_to_mul(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powi(3)
}

/// `inputVolumeMul` → fader position 0..1 (cube root, clamped; above 0 dB reads as 1.0).
pub fn mul_to_fader(mul: f32) -> f32 {
    mul.max(0.0).cbrt().clamp(0.0, 1.0)
}

/// `"-6.0 dB"`, `"0.0 dB"`, `"-inf dB"` for 0.
pub fn format_db(mul: f32) -> String {
    if mul <= 0.0 {
        "-inf dB".into()
    } else {
        format!("{:.1} dB", 20.0 * mul.log10())
    }
}

/// `M:SS` under an hour, else `H:MM:SS`.
pub fn format_duration(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Resolve a scene target to its OBS name.
pub fn resolve_scene(snap: &ObsSnapshot, scene: &ObsScene) -> Option<String> {
    match scene {
        ObsScene::Name { name } => snap.scenes.iter().find(|s| *s == name).cloned(),
        ObsScene::Slot { index } => snap.scenes.get(*index as usize).cloned(),
    }
}

/// Resolve an input target to its OBS name (must exist in `snap.inputs`).
pub fn resolve_input<'a>(snap: &'a ObsSnapshot, input: &ObsInput) -> Option<&'a ObsInputInfo> {
    let name = match input {
        ObsInput::Name { name } => name,
        ObsInput::Special { which } => snap.special_inputs.get(which)?,
    };
    snap.inputs.iter().find(|i| i.name == *name)
}

/// True for the OBS action variants (the router hands those to [`execute`]).
pub fn is_obs_action(action: &Action) -> bool {
    matches!(
        action,
        Action::ObsScene { .. }
            | Action::ObsSourceVisibility { .. }
            | Action::ObsStream { .. }
            | Action::ObsRecord { .. }
            | Action::ObsRecordPause { .. }
            | Action::ObsReplayBuffer { .. }
            | Action::ObsSaveReplay
            | Action::ObsVirtualCam { .. }
            | Action::ObsStudioMode { .. }
            | Action::ObsStudioTransition
            | Action::ObsInputMute { .. }
            | Action::ObsSetInputVolume { .. }
            | Action::ObsAdjustInputVolume { .. }
    )
}

/// Run one OBS action (see the module doc). Panics never; non-OBS actions return Err.
pub fn execute(obs: &dyn ObsControl, action: &Action, ctx: &InputContext) -> Result<ActionOutcome, String> {
    if !is_obs_action(action) {
        return Err("not an OBS action".into());
    }
    let snap = obs.snapshot();
    if snap.status != ObsStatus::Connected {
        return Err(NOT_CONNECTED.into());
    }
    match action {
        Action::ObsScene { scene, preview } => {
            let name = scene_or_err(&snap, scene)?;
            let cmd = if *preview && snap.studio_mode {
                ObsCommand::SetPreviewScene(name)
            } else {
                ObsCommand::SetProgramScene(name)
            };
            obs.send(cmd)?;
            Ok(ActionOutcome::SetState(1))
        }
        Action::ObsSourceVisibility { scene, source, mode } => {
            let scene_name = match scene {
                Some(sc) => scene_or_err(&snap, sc)?,
                None => snap.program_scene.clone().ok_or_else(|| "OBS has no program scene".to_string())?,
            };
            let item = visibility_item(&snap, &scene_name, source)
                .ok_or_else(|| format!("OBS source '{source}' not found in scene '{scene_name}'"))?;
            let new = mode.resolve(item.enabled);
            if new != item.enabled {
                obs.send(ObsCommand::SetSceneItemEnabled { scene: scene_name, item_id: item.item_id, enabled: new })?;
            }
            Ok(ActionOutcome::SetState(u32::from(new)))
        }
        Action::ObsStream { mode } => toggle_output(obs, snap.stream.active, *mode, ObsCommand::Stream),
        Action::ObsRecord { mode } => toggle_output(obs, snap.record.active, *mode, ObsCommand::Record),
        Action::ObsReplayBuffer { mode } => {
            toggle_output(obs, snap.replay_buffer.active, *mode, ObsCommand::ReplayBuffer)
        }
        Action::ObsVirtualCam { mode } => toggle_output(obs, snap.virtual_cam.active, *mode, ObsCommand::VirtualCam),
        Action::ObsStudioMode { mode } => toggle_output(obs, snap.studio_mode, *mode, ObsCommand::SetStudioMode),
        Action::ObsRecordPause { mode } => {
            if !snap.record.active {
                return Err("OBS is not recording".into());
            }
            let new = mode.resolve(snap.record.paused);
            if new != snap.record.paused {
                obs.send(ObsCommand::RecordPause(new))?;
            }
            Ok(ActionOutcome::SetState(if new { 2 } else { 1 }))
        }
        Action::ObsSaveReplay => {
            if !snap.replay_buffer.active {
                return Err("the replay buffer is off".into());
            }
            obs.send(ObsCommand::SaveReplayBuffer)?;
            Ok(ActionOutcome::Executed)
        }
        Action::ObsStudioTransition => {
            if !snap.studio_mode {
                return Err("studio mode is off".into());
            }
            obs.send(ObsCommand::StudioTransition)?;
            Ok(ActionOutcome::Executed)
        }
        Action::ObsInputMute { input, mode } => {
            let info = input_or_err(&snap, input)?;
            let muted = info.muted.ok_or_else(|| not_audio(info))?;
            let new = mode.resolve(muted);
            if new != muted {
                obs.send(ObsCommand::SetInputMute { input: info.name.clone(), muted: new })?;
            }
            Ok(ActionOutcome::SetState(u32::from(new)))
        }
        Action::ObsSetInputVolume { input, level } => {
            let info = input_or_err(&snap, input)?;
            info.volume_mul.ok_or_else(|| not_audio(info))?;
            let v = level.unwrap_or(ctx.value).clamp(0.0, 1.0);
            obs.send(ObsCommand::SetInputVolumeMul { input: info.name.clone(), mul: fader_to_mul(v) })?;
            Ok(ActionOutcome::SetValue(v))
        }
        Action::ObsAdjustInputVolume { input, step } => {
            let info = input_or_err(&snap, input)?;
            let mul = info.volume_mul.ok_or_else(|| not_audio(info))?;
            let steps = if ctx.delta == 0 { 1 } else { ctx.delta };
            let v = (mul_to_fader(mul) + step * steps as f32).clamp(0.0, 1.0);
            obs.send(ObsCommand::SetInputVolumeMul { input: info.name.clone(), mul: fader_to_mul(v) })?;
            Ok(ActionOutcome::SetValue(v))
        }
        _ => Err("not an OBS action".into()),
    }
}

const NOT_CONNECTED: &str = "OBS is not connected";

impl ObsToggle {
    /// New boolean state given the current one.
    fn resolve(self, current: bool) -> bool {
        match self {
            ObsToggle::Toggle => !current,
            ObsToggle::On => true,
            ObsToggle::Off => false,
        }
    }
}

fn scene_or_err(snap: &ObsSnapshot, scene: &ObsScene) -> Result<String, String> {
    resolve_scene(snap, scene).ok_or_else(|| match scene {
        ObsScene::Name { name } => format!("OBS scene '{name}' not found"),
        ObsScene::Slot { index } => format!("OBS scene slot {index} is empty"),
    })
}

fn input_or_err<'a>(snap: &'a ObsSnapshot, input: &ObsInput) -> Result<&'a ObsInputInfo, String> {
    resolve_input(snap, input).ok_or_else(|| match input {
        ObsInput::Name { name } => format!("OBS input '{name}' not found"),
        ObsInput::Special { which } => format!("OBS input '{}' is not set", which.ws_key()),
    })
}

fn not_audio(info: &ObsInputInfo) -> String {
    format!("OBS input '{}' has no audio", info.name)
}

fn visibility_item<'a>(snap: &'a ObsSnapshot, scene: &str, source: &str) -> Option<&'a ObsSceneItem> {
    snap.scene_items.get(scene)?.iter().find(|i| i.source == source)
}

/// Shared by stream / record / replay buffer / virtual cam / studio mode: send only on a change.
fn toggle_output(
    obs: &dyn ObsControl,
    active: bool,
    mode: ObsToggle,
    cmd: impl FnOnce(bool) -> ObsCommand,
) -> Result<ActionOutcome, String> {
    let new = mode.resolve(active);
    if new != active {
        obs.send(cmd(new))?;
    }
    Ok(ActionOutcome::SetState(u32::from(new)))
}

/// True for the OBS data sources.
pub fn is_obs_source(src: &DataSource) -> bool {
    matches!(
        src,
        DataSource::ObsScene { .. }
            | DataSource::ObsStream
            | DataSource::ObsRecord
            | DataSource::ObsReplayBuffer
            | DataSource::ObsVirtualCam
            | DataSource::ObsStudioMode
            | DataSource::ObsSourceVisibility { .. }
            | DataSource::ObsInput { .. }
            | DataSource::ObsStatus
    )
}

/// Sample one OBS source from a snapshot (see the module doc). Non-OBS sources → `LiveSample::default()`.
pub fn sample(snap: &ObsSnapshot, src: &DataSource) -> LiveSample {
    if !is_obs_source(src) {
        return LiveSample::default();
    }
    if let DataSource::ObsStatus = src {
        let connected = snap.status == ObsStatus::Connected;
        let subtitle = match snap.status {
            ObsStatus::Disabled => "disabled",
            ObsStatus::Connecting => "connecting",
            ObsStatus::Connected => "",
            ObsStatus::AuthFailed => "wrong password",
            ObsStatus::ServerDisabled => "server off",
            ObsStatus::Offline => "offline",
        };
        return LiveSample {
            state_index: Some(u32::from(connected)),
            subtitle: Some(subtitle.into()),
            dimmed: Some(false),
            ..Default::default()
        };
    }
    let unavailable = |slot_title: bool| LiveSample {
        value: Some(0.0),
        subtitle: Some(String::new()),
        state_index: Some(0),
        dimmed: Some(true),
        title: slot_title.then(String::new),
        ..Default::default()
    };
    let ok = |state: u32| LiveSample { state_index: Some(state), dimmed: Some(false), ..Default::default() };
    let output = |o: &OutputState, paused_state: bool| {
        let state = if !o.active {
            0
        } else if paused_state && o.paused {
            2
        } else {
            1
        };
        let mut s = ok(state);
        s.subtitle = Some(if o.active { format_duration(o.duration_ms) } else { String::new() });
        s
    };
    let slot = matches!(src, DataSource::ObsScene { scene: ObsScene::Slot { .. } });
    if snap.status != ObsStatus::Connected {
        return unavailable(slot);
    }
    match src {
        DataSource::ObsScene { scene } => match resolve_scene(snap, scene) {
            Some(name) => {
                let state = if snap.program_scene.as_deref() == Some(&name) {
                    1
                } else if snap.studio_mode && snap.preview_scene.as_deref() == Some(&name) {
                    2
                } else {
                    0
                };
                let mut s = ok(state);
                if slot {
                    s.title = Some(name);
                }
                s
            }
            None => unavailable(slot),
        },
        DataSource::ObsStream => output(&snap.stream, false),
        DataSource::ObsRecord => output(&snap.record, true),
        DataSource::ObsReplayBuffer => ok(u32::from(snap.replay_buffer.active)),
        DataSource::ObsVirtualCam => ok(u32::from(snap.virtual_cam.active)),
        DataSource::ObsStudioMode => ok(u32::from(snap.studio_mode)),
        DataSource::ObsSourceVisibility { scene, source } => {
            let scene_name = match scene {
                Some(sc) => resolve_scene(snap, sc),
                None => snap.program_scene.clone(),
            };
            match scene_name.and_then(|sc| visibility_item(snap, &sc, source).map(|i| i.enabled)) {
                Some(enabled) => ok(u32::from(enabled)),
                None => unavailable(false),
            }
        }
        DataSource::ObsInput { input } => match resolve_input(snap, input) {
            Some(ObsInputInfo { muted: Some(muted), volume_mul: Some(mul), .. }) => {
                LiveSample { value: Some(mul_to_fader(*mul)), subtitle: Some(format_db(*mul)), ..ok(u32::from(*muted)) }
            }
            _ => unavailable(false),
        },
        _ => LiveSample::default(),
    }
}
