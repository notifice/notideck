// SPDX-License-Identifier: GPL-3.0-or-later
//! Notideck headless fake phone test client and mock transport host.
//!
//! Provides:
//! - [`FakePhone`]: Headless phone client speaking the v2 control channel (mutual TLS 1.3,
//!   `Envelope` + length-prefixed framing, Hello / PairRequest / Welcome).
//! - [`FakePhoneConfig`]: Mock device identity, screen information, pin and pairing settings.
//! - [`store`]: persistent client identity and pinned hosts (`hosts.json`).
//! - [`connect_stored`]: connect the way the real phone does (stored pin, QR pairing, TOFU).
//! - [`MockHost`]: Mock TLS transport server and action router for unit and integration testing.

#[cfg(feature = "camera")]
pub mod camera;
pub mod client;
pub mod config;
pub mod error;
pub mod mic;
pub mod mock;
pub mod session;
pub mod store;

#[cfg(feature = "camera")]
pub use camera::{CameraStats, FakeCamera};
pub use client::{plaintext_v1_probe, FakePhone, PhoneState};
pub use config::{FakePhoneConfig, PendingHook};
pub use error::FakePhoneError;
pub use mic::{FakeMic, MicMode, MicStats};
pub use mock::{MockAction, MockHost, TriggeredAction};
pub use session::{connect_stored, forget_if_invalidated, ConnectRequest, StoredSession};
pub use store::{default_identity_dir, ClientIdentity, HostStore, PinnedHost};
