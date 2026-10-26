// SPDX-License-Identifier: GPL-3.0-or-later
//! Library half of the Notideck agent: shared state API used by the daemon and the config server.

pub mod app;
pub mod autoswitch;
pub mod camera;
pub mod config_backend;
pub mod devices;
pub mod discovery;
pub mod fake_media;
#[cfg(any(windows, feature = "linux-gui"))]
pub mod gui;
pub mod instance;
pub mod logging;
pub mod mic;
pub mod mixer_cfg;
pub mod nav;
pub mod obs_cfg;
pub mod pairing;
pub mod phone_usb;
pub mod plugins;
pub mod selftest;
pub mod settings;
pub mod site_icon;
pub mod state;
pub mod ticks;

pub use app::{AgentHandle, AgentOptions, AgentStatus, DeviceSummary, ShellEvent};
pub use config_backend::AgentConfigBackend;
pub use state::{AgentState, NavSnapshot, StateEvent};
