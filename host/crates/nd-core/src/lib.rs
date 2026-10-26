// SPDX-License-Identifier: GPL-3.0-or-later
//! Notideck core logic (SPEC §3.2, §6.2, §6.4).
//!
//! Pure logic with no direct OS calls:
//! - Data models: [`Profile`], [`Page`], [`Control`], [`Action`], [`ActionDefinition`].
//! - Action routing: [`ActionRouter`] executing actions against `nd_platform` traits.
//! - Session state: [`DeckSession`] tracking pages, folder stacks, and control updates.
//! - Mocks: [`mock::MockInputInjector`] and [`mock::MockLauncher`] for testing.

pub mod autoswitch;
pub mod camera;
pub mod error;
pub mod icons;
pub mod live;
pub mod mic;
pub mod mixer;
pub mod mock;
pub mod model;
pub mod obs;
pub mod plugin;
pub mod rail;
pub mod router;
pub mod session;
pub mod store;

#[cfg(test)]
mod media_tests;
#[cfg(test)]
mod obs_tests;
#[cfg(test)]
mod tests;

pub use camera::{CameraActions, CameraSnapshot, NullCamera};
pub use error::{CoreError, Result};
pub use icons::IconStore;
pub use live::{sample_all, LiveProviders, LiveSample};
pub use mic::{MicActions, MicSnapshot, NullMic};
pub use mixer::{Mixer, MixerApp, MixerConfig};
pub use model::{
    Action, ActionDefinition, AudioApp, Control, ControlKind, ControlStateVisual, DataSource, MultiStep, Page,
    PauseMode, Profile, TriggerKind, CURRENT_SCHEMA_VERSION,
};
pub use obs::{NullObs, ObsCommand, ObsControl, ObsSnapshot};
pub use plugin::{NullPlugins, PluginControl, PluginEvent, PluginEventKind, PluginInstance, PluginSettings};
pub use rail::{load_rail, rail_icon_hashes, save_rail, validate_rail, RailConfig, MAX_RAIL_KEYS, RAIL_PAGE_ID};
pub use router::{ActionOutcome, ActionRouter, InputContext, NavigationTarget};
pub use session::{DeckSession, InputReaction};
pub use store::{default_config_root, default_profile, referenced_icon_hashes, ProfileStore, ProfileSummary};
