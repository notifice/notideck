// SPDX-License-Identifier: GPL-3.0-or-later
//! Action routing and execution (SPEC §6.4).
//!
//! Routes deck actions to `nd_platform::InputInjector` and `nd_platform::Launcher`.
//! Actions that require navigation or internal state mutation return an [`ActionOutcome`].

use crate::autoswitch::{NullProfileActions, ProfileActions};
use crate::camera::{zoom_to_value, CameraActions, NullCamera};
use crate::error::{CoreError, Result};
use crate::mic::{MicActions, NullMic};
use crate::mixer::Mixer;
use crate::model::{Action, MediaTransportCmd, RepeatAction, TriggerKind};
use crate::obs::{NullObs, ObsControl, ObsToggle};
use crate::plugin::{NullPlugins, PluginControl, PluginEvent, PluginEventKind};
use nd_platform::{
    AudioControl, InputInjector, Key, KeyCombo, Launcher, MediaCommand, MediaInfo, NullAppAudio, NullAudio, NullMedia,
    PlatformError, RepeatMode,
};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// The target of a navigation action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationTarget {
    /// Navigate to a specific page ID.
    Page(String),
    /// Navigate to the next page.
    Next,
    /// Navigate to the previous page.
    Prev,
    /// Open a folder page (pushes current page to back stack).
    Folder(String),
    /// Go back to the parent folder / previous page.
    Back,
}

/// Event data an action may consume (dial delta, slider value).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InputContext {
    /// Signed dial detents (+ = clockwise). 0 for non-dial events; treat as 1 for keys.
    pub delta: i32,
    /// Slider value 0..1 (or the control's current value for non-slider events).
    pub value: f32,
    /// Round 19: plugin context of the control (`crate::plugin::plugin_context`); empty outside a session.
    pub context: String,
    /// Round 19: the binding's trigger (plugin actions forward it); `None` outside a session.
    pub trigger: Option<TriggerKind>,
}

/// The outcome of an action execution.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionOutcome {
    /// Action executed against platform injector or launcher.
    Executed,
    /// Action requested page navigation.
    Navigate(NavigationTarget),
    /// Action requested toggling control state.
    ToggleState,
    /// Action produced a new control value 0..1 (e.g. volume after a change).
    SetValue(f32),
    /// Action produced a new control state index (e.g. 1 = muted).
    SetState(u32),
}

/// Upper bound for the summed delays of one multi-action. The router runs
/// synchronously (the agent holds the session lock meanwhile), so keep it short.
pub const MAX_MULTI_DELAY_MS: u64 = 5_000;

/// Routes actions to platform implementations or internal state handlers.
#[derive(Clone)]
pub struct ActionRouter {
    /// Shared slot so clones see a swapped injector (see [`ActionRouter::set_injector`]).
    injector: Arc<RwLock<Arc<dyn InputInjector>>>,
    launcher: Arc<dyn Launcher>,
    audio: Arc<dyn AudioControl>,
    camera: Arc<dyn CameraActions>,
    mic: Arc<dyn MicActions>,
    obs: Arc<dyn ObsControl>,
    profile_actions: Arc<dyn ProfileActions>,
    mixer: Arc<Mixer>,
    /// Round 17: media session control for the `media_*` actions.
    media: Arc<dyn MediaInfo>,
    /// Round 19: plugin host for [`Action::Plugin`].
    plugins: Arc<dyn PluginControl>,
}

impl ActionRouter {
    /// Create a new action router with the given platform injector and launcher.
    pub fn new(injector: Arc<dyn InputInjector>, launcher: Arc<dyn Launcher>) -> Self {
        Self {
            injector: Arc::new(RwLock::new(injector)),
            launcher,
            audio: Arc::new(NullAudio),
            camera: Arc::new(NullCamera),
            mic: Arc::new(NullMic),
            obs: Arc::new(NullObs),
            profile_actions: Arc::new(NullProfileActions),
            mixer: Arc::new(Mixer::new(Arc::new(NullAppAudio), Arc::new(NullAudio))),
            media: Arc::new(NullMedia),
            plugins: Arc::new(NullPlugins),
        }
    }

    /// Use `plugins` for [`Action::Plugin`] (default: [`NullPlugins`], which drops the events). Round 19.
    pub fn with_plugins(mut self, plugins: Arc<dyn PluginControl>) -> Self {
        self.plugins = plugins;
        self
    }

    /// Use `audio` for volume / mute actions (default: [`NullAudio`]).
    pub fn with_audio(mut self, audio: Arc<dyn AudioControl>) -> Self {
        self.audio = audio;
        self
    }

    /// Use `obs` for the OBS actions (default: [`NullObs`], which fails them). Round 14.
    pub fn with_obs(mut self, obs: Arc<dyn ObsControl>) -> Self {
        self.obs = obs;
        self
    }

    /// Use `camera` for the webcam actions (default: [`NullCamera`], which fails them).
    pub fn with_camera(mut self, camera: Arc<dyn CameraActions>) -> Self {
        self.camera = camera;
        self
    }

    /// Use `actions` for the main-menu / auto-switch pause actions (default: [`NullProfileActions`], which fails them).
    pub fn with_profile_actions(mut self, actions: Arc<dyn ProfileActions>) -> Self {
        self.profile_actions = actions;
        self
    }

    /// Use `mixer` for the per-app audio actions (default: a mixer without backend, which fails them).
    pub fn with_mixer(mut self, mixer: Arc<Mixer>) -> Self {
        self.mixer = mixer;
        self
    }

    /// Use `media` for the `media_*` actions (default: [`NullMedia`]: transport falls back to hotkeys,
    /// the rest fail). Round 17.
    pub fn with_media(mut self, media: Arc<dyn MediaInfo>) -> Self {
        self.media = media;
        self
    }

    /// Use `mic` for the phone-mic actions (default: [`NullMic`], which fails them).
    pub fn with_mic(mut self, mic: Arc<dyn MicActions>) -> Self {
        self.mic = mic;
        self
    }

    /// Access the camera backend.
    pub fn camera(&self) -> &dyn CameraActions {
        &*self.camera
    }

    /// Access the underlying input injector.
    pub fn injector(&self) -> Arc<dyn InputInjector> {
        self.injector.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replace the input injector for this router and all its clones.
    pub fn set_injector(&self, injector: Arc<dyn InputInjector>) {
        *self.injector.write().unwrap_or_else(|e| e.into_inner()) = injector;
    }

    /// Access the underlying launcher.
    pub fn launcher(&self) -> &dyn Launcher {
        &*self.launcher
    }

    /// Access the audio backend.
    pub fn audio(&self) -> &dyn AudioControl {
        &*self.audio
    }

    /// Execute a single action. `ctx` carries the dial delta / slider value of the event.
    pub fn execute(&self, action: &Action, ctx: &InputContext) -> Result<ActionOutcome> {
        match action {
            Action::Hotkey { combo } => {
                tracing::debug!(?combo, "executing hotkey");
                self.injector().key_combo(combo)?;
                Ok(ActionOutcome::Executed)
            }
            Action::TypeText { text } => {
                tracing::debug!(text, "typing text");
                self.injector().type_text(text)?;
                Ok(ActionOutcome::Executed)
            }
            Action::OpenUrl { url } => {
                tracing::debug!(url, "opening url");
                self.launcher.open_url(url)?;
                Ok(ActionOutcome::Executed)
            }
            Action::LaunchApp { confirmed: false, path, .. } => {
                tracing::warn!(path, "refusing unconfirmed launch action (ADR 0002)");
                Err(CoreError::UnsupportedAction(format!("launch '{path}' has not been confirmed in the config UI")))
            }
            Action::LaunchApp { path, args, .. } => {
                tracing::debug!(path, ?args, "launching app");
                self.launcher.launch(path, args)?;
                Ok(ActionOutcome::Executed)
            }
            Action::GoToPage { page_id } => Ok(ActionOutcome::Navigate(NavigationTarget::Page(page_id.clone()))),
            Action::NextPage => Ok(ActionOutcome::Navigate(NavigationTarget::Next)),
            Action::PrevPage => Ok(ActionOutcome::Navigate(NavigationTarget::Prev)),
            Action::OpenFolder { folder_page_id } => {
                Ok(ActionOutcome::Navigate(NavigationTarget::Folder(folder_page_id.clone())))
            }
            Action::GoBack => Ok(ActionOutcome::Navigate(NavigationTarget::Back)),
            Action::ToggleState => Ok(ActionOutcome::ToggleState),
            Action::AdjustVolume { target, step } => {
                let steps = if ctx.delta == 0 { 1 } else { ctx.delta };
                let current = self.audio.volume(*target)?;
                let new = (current + step * steps as f32).clamp(0.0, 1.0);
                self.audio.set_volume(*target, new)?;
                Ok(ActionOutcome::SetValue(new))
            }
            Action::SetVolume { target, level } => {
                let v = level.unwrap_or(ctx.value).clamp(0.0, 1.0);
                self.audio.set_volume(*target, v)?;
                Ok(ActionOutcome::SetValue(v))
            }
            Action::ToggleMute { target } => {
                let now_muted = !self.audio.muted(*target)?;
                self.audio.set_muted(*target, now_muted)?;
                Ok(ActionOutcome::SetState(u32::from(now_muted)))
            }
            Action::AdjustAppVolume { app, step } => {
                let steps = if ctx.delta == 0 { 1 } else { ctx.delta };
                Ok(ActionOutcome::SetValue(self.mixer.adjust(app, step * steps as f32)?))
            }
            Action::SetAppVolume { app, level } => {
                Ok(ActionOutcome::SetValue(self.mixer.set_volume(app, level.unwrap_or(ctx.value).clamp(0.0, 1.0))?))
            }
            Action::ToggleAppMute { app } => Ok(ActionOutcome::SetState(u32::from(self.mixer.toggle_mute(app)?))),
            Action::CameraToggle => {
                let on = self.camera.toggle().map_err(CoreError::Camera)?;
                Ok(ActionOutcome::SetState(u32::from(on)))
            }
            Action::CameraSwitchLens => {
                self.camera.switch_lens().map_err(CoreError::Camera)?;
                Ok(ActionOutcome::Executed)
            }
            Action::CameraTorch => {
                let on = self.camera.toggle_torch().map_err(CoreError::Camera)?;
                Ok(ActionOutcome::SetState(u32::from(on)))
            }
            Action::CameraZoom { step } => {
                let steps = if ctx.delta == 0 { 1 } else { ctx.delta };
                let zoom = self.camera.zoom_by(step * steps as f32).map_err(CoreError::Camera)?;
                Ok(ActionOutcome::SetValue(zoom_to_value(zoom)))
            }
            Action::MicToggle => {
                let on = self.mic.toggle().map_err(CoreError::Mic)?;
                Ok(ActionOutcome::SetState(u32::from(on)))
            }
            Action::MicMute => {
                let muted = self.mic.toggle_mute().map_err(CoreError::Mic)?;
                Ok(ActionOutcome::SetState(u32::from(muted)))
            }
            Action::GoHome { pause_auto_switch } => {
                self.profile_actions.go_home(*pause_auto_switch).map_err(CoreError::UnsupportedAction)?;
                Ok(ActionOutcome::Executed)
            }
            Action::SwitchProfile { to } => {
                self.profile_actions.switch_profile(to).map_err(CoreError::UnsupportedAction)?;
                Ok(ActionOutcome::Executed)
            }
            Action::AutoSwitchPause { mode } => {
                let paused = self.profile_actions.set_paused(*mode).map_err(CoreError::UnsupportedAction)?;
                Ok(ActionOutcome::SetState(u32::from(paused)))
            }
            a @ (Action::ObsScene { .. }
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
            | Action::ObsAdjustInputVolume { .. }) => crate::obs::execute(&*self.obs, a, ctx).map_err(CoreError::Obs),
            Action::MediaTransport { command } => {
                self.media_transport(*command)?;
                Ok(ActionOutcome::Executed)
            }
            Action::MediaSeek { position } => {
                let fraction = position.unwrap_or(ctx.value).clamp(0.0, 1.0);
                let length = self
                    .media
                    .now_playing()?
                    .and_then(|np| np.length_s)
                    .filter(|l| l.is_finite() && *l > 0.0)
                    .ok_or_else(|| CoreError::UnsupportedAction("track length unknown".into()))?;
                self.media.control(MediaCommand::Seek { position_s: f64::from(fraction) * length })?;
                Ok(ActionOutcome::SetValue(fraction))
            }
            Action::MediaSeekBy { seconds } => {
                let steps = if ctx.delta == 0 { 1 } else { ctx.delta };
                self.media.control(MediaCommand::SeekBy { delta_s: f64::from(*seconds) * f64::from(steps) })?;
                Ok(ActionOutcome::Executed)
            }
            Action::MediaShuffle { mode } => {
                let on = match mode {
                    ObsToggle::On => true,
                    ObsToggle::Off => false,
                    ObsToggle::Toggle => !self.media.now_playing()?.and_then(|np| np.shuffle).unwrap_or(false),
                };
                self.media.control(MediaCommand::Shuffle { on })?;
                Ok(ActionOutcome::SetState(u32::from(on)))
            }
            Action::Plugin { plugin, action, settings } => {
                let trigger = ctx.trigger.unwrap_or_default();
                self.plugins.event(PluginEvent {
                    context: ctx.context.clone(),
                    plugin: plugin.clone(),
                    action: action.clone(),
                    settings: settings.clone(),
                    kind: PluginEventKind::from_trigger(trigger, ctx.delta, ctx.value),
                });
                Ok(ActionOutcome::Executed)
            }
            Action::MediaRepeat { mode } => {
                let new = match mode {
                    RepeatAction::Off => RepeatMode::Off,
                    RepeatAction::List => RepeatMode::List,
                    RepeatAction::Track => RepeatMode::Track,
                    RepeatAction::Cycle => {
                        match self.media.now_playing()?.and_then(|np| np.repeat).unwrap_or_default() {
                            RepeatMode::Off => RepeatMode::List,
                            RepeatMode::List => RepeatMode::Track,
                            RepeatMode::Track => RepeatMode::Off,
                        }
                    }
                };
                self.media.control(MediaCommand::Repeat { mode: new })?;
                Ok(ActionOutcome::SetState(match new {
                    RepeatMode::Off => 0,
                    RepeatMode::List => 1,
                    RepeatMode::Track => 2,
                }))
            }
            Action::Multi { steps } => {
                let total: u64 = steps.iter().map(|s| s.delay_ms).sum();
                if total > MAX_MULTI_DELAY_MS {
                    return Err(CoreError::UnsupportedAction(format!(
                        "multi-action delays total {total} ms (max {MAX_MULTI_DELAY_MS} ms)"
                    )));
                }
                if steps.iter().any(|s| matches!(s.action, Action::Multi { .. })) {
                    return Err(CoreError::UnsupportedAction("nested multi-actions are not supported".into()));
                }
                // Navigation / toggle outcomes are applied by the caller once; the last
                // non-`Executed` outcome wins. A failing step aborts the sequence.
                let mut outcome = ActionOutcome::Executed;
                for step in steps {
                    if step.delay_ms > 0 {
                        std::thread::sleep(Duration::from_millis(step.delay_ms));
                    }
                    match self.execute(&step.action, ctx)? {
                        ActionOutcome::Executed => {}
                        other => outcome = other,
                    }
                }
                Ok(outcome)
            }
        }
    }

    /// Send a transport command to the media session; fall back to the media hotkey when the backend
    /// cannot (`Unsupported`), or, for the commands where a hotkey may wake a player, on any error.
    fn media_transport(&self, command: MediaTransportCmd) -> Result<()> {
        let (cmd, key, any_error_falls_back) = match command {
            MediaTransportCmd::PlayPause => (MediaCommand::PlayPause, Key::MediaPlayPause, true),
            MediaTransportCmd::Play => (MediaCommand::Play, Key::MediaPlayPause, false),
            MediaTransportCmd::Pause => (MediaCommand::Pause, Key::MediaPlayPause, false),
            MediaTransportCmd::Next => (MediaCommand::Next, Key::MediaNext, true),
            MediaTransportCmd::Prev => (MediaCommand::Prev, Key::MediaPrev, true),
            MediaTransportCmd::Stop => (MediaCommand::Stop, Key::MediaStop, true),
        };
        match self.media.control(cmd) {
            Ok(()) => Ok(()),
            Err(e) => {
                if matches!(e, PlatformError::Unsupported(_)) || any_error_falls_back {
                    tracing::debug!(error = %e, ?command, "media control failed, using the media hotkey");
                    self.injector().key_combo(&KeyCombo { modifiers: vec![], key })?;
                    Ok(())
                } else {
                    Err(e.into())
                }
            }
        }
    }

    /// Execute multiple actions in sequence, returning all outcomes.
    pub fn execute_all(&self, actions: &[Action], ctx: &InputContext) -> Result<Vec<ActionOutcome>> {
        let mut outcomes = Vec::with_capacity(actions.len());
        for action in actions {
            outcomes.push(self.execute(action, ctx)?);
        }
        Ok(outcomes)
    }
}
