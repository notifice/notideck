// SPDX-License-Identifier: GPL-3.0-or-later
//! Connection management: USB (adb) device tracking, reverse port forwarding,
//! and the mutually-authenticated TLS control server (SPEC §4, §5; ADR 0004).

pub mod adb;
pub mod error;
pub mod handle;
pub mod identity;
pub mod media;
pub mod pairing;
pub mod server;
pub mod session;
pub mod tls;
pub mod trust;

pub use adb::{
    parse_devices_output, resolve_adb_path, AdbClient, AdbDevice, AdbDeviceEvent, AdbDeviceState, AdbTracker,
};
pub use error::{Result, TransportError};
pub use handle::SessionHandle;
pub use identity::HostIdentity;
pub use media::MediaStream;
pub use pairing::{Fingerprint, PairingUri};
pub use server::{get_host_name, ControlServer, ServerHandle};
pub use session::TransportEvent;
pub use trust::{Link, MemoryTrustStore, PeerInfo, PendingPeer, TrustState, TrustStore};

/// Wall clock in unix ms (round 21: `Pong.host_time_ms`).
pub(crate) fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
