// SPDX-License-Identifier: GPL-3.0-or-later
//! Notideck plugin host (SPEC §8, round 19 / M9). The wire protocol and manifest format are specified in
//! `docs/PLUGINS.md`; this crate is the agent side.
//!
//! - [`manifest`]: `manifest.json` types and validation.
//! - [`host::PluginHost`]: discovery, approval state, process supervision, the WebSocket/JSON-RPC server, and
//!   the per-context look cache. Implements [`nd_core::PluginControl`].

mod discovery;
pub mod host;
mod image;
mod install;
pub mod manifest;
mod server;
mod state;
mod supervisor;

pub use host::{HostConfig, HostEvent, PluginCommand, PluginHost, PluginInfo, PluginStatus};
pub use manifest::{Entry, FieldType, Manifest, ManifestAction, ManifestError, Permission, SelectOption, SettingField};

#[cfg(test)]
mod tests;
