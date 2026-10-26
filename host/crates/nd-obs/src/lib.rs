// SPDX-License-Identifier: GPL-3.0-or-later
//! OBS Studio integration (round 14, M8): an obs-websocket v5 client that keeps an
//! [`nd_core::obs::ObsSnapshot`] current, detection of OBS's own websocket settings, and a fake
//! obs-websocket server for tests and `NOTIDECK_FAKE_OBS=1`.
//!
//! - [`client::ObsClient`] implements [`nd_core::obs::ObsControl`].
//! - [`detect`] reads / enables OBS's `plugin_config/obs-websocket/config.json` (zero-setup connection).
//! - [`fake::FakeObs`] speaks enough of the protocol for the client and the agent e2e tests.

pub mod client;
pub mod detect;
pub mod fake;
mod util;

pub use client::{ObsClient, ObsInfo, ObsSettings};
pub use detect::DetectedConfig;
pub use fake::FakeObs;

#[cfg(test)]
mod tests;
