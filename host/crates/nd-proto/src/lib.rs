// SPDX-License-Identifier: GPL-3.0-or-later
//! Notideck wire protocol (SPEC §4.5, §5).
//!
//! * [`v1`] — prost types generated from `proto/notideck/v1/control.proto`.
//! * [`framing`] — control channel: `u32` big-endian length + `Envelope`.
//! * [`media`] — media channel: 12-byte header + H.264 Annex-B payload.

pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/notideck.v1.rs"));
}

pub mod framing;
pub mod media;

pub use v1::envelope::Msg;
pub use v1::Envelope;

/// Protocol version spoken by this build. v2 = TLS + pairing (ADR 0004); the host accepts only this version.
pub const PROTO_VERSION: u32 = 2;

/// TCP port of the control channel (phone connects; USB via `adb reverse`).
pub const CONTROL_PORT: u16 = 47800;

/// Local config UI port (SPEC §3.3).
pub const CONFIG_UI_PORT: u16 = 47801;

/// Heartbeat interval in milliseconds (SPEC §5).
pub const PING_INTERVAL_MS: u64 = 2000;

/// Missed pings before the session is declared dead.
pub const MAX_MISSED_PINGS: u32 = 3;

/// Default grid when the host has no other preference.
pub const DEFAULT_COLS: u32 = 5;
pub const DEFAULT_ROWS: u32 = 3;

impl Envelope {
    /// Convenience constructor.
    pub fn new(seq: u32, msg: Msg) -> Self {
        Envelope { seq, msg: Some(msg) }
    }
}
