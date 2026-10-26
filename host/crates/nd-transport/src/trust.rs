// SPDX-License-Identifier: GPL-3.0-or-later
//! Contract between the TLS control server and whoever owns the paired-device registry
//! (nd-agent's `devices.json`). ADR 0004 §3–§5, §10.
//!
//! The server never stores pins itself: it asks the [`TrustStore`] after the TLS handshake and
//! after `Hello`, and the agent implements the store (registry + pairing tickets).

use std::net::SocketAddr;

use crate::pairing::Fingerprint;

/// How the phone reached us. Loopback peers come through `adb reverse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    Usb,
    Lan,
}

impl Link {
    pub fn of(addr: &SocketAddr) -> Self {
        if addr.ip().is_loopback() {
            Link::Usb
        } else {
            Link::Lan
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Link::Usb => "usb",
            Link::Lan => "lan",
        }
    }
}

/// What the server knows about a phone once it has sent `Hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// From the TLS client certificate: the real identity.
    pub fp: Fingerprint,
    /// `Hello.device_id` (informational; may change on app reinstall, the fp changes too).
    pub device_id: String,
    /// `Hello.device_name`, falling back to `Hello.device_model`.
    pub device_name: String,
    pub device_model: String,
    pub addr: SocketAddr,
    pub link: Link,
}

/// Where a session stands with respect to pairing. Exposed as `SessionHandle::trust_state()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    /// Pinned cert: full session (layout, input, ...).
    Paired,
    /// Unknown cert over USB, waiting for Allow/Deny on the PC.
    PendingUsb,
    /// Unknown cert over LAN: only `PairRequest` is accepted.
    PairingOnly,
}

/// Implemented by nd-agent. Called from the server's tasks, so keep it quick and non-blocking
/// (in-memory state plus an atomic file write is fine).
pub trait TrustStore: Send + Sync + 'static {
    /// Whether this certificate is pinned.
    fn is_paired(&self, fp: &Fingerprint) -> bool;

    /// QR pairing: check `proof` against the live ticket (expiry, single use, constant-time HMAC
    /// via [`crate::pairing::verify_proof`] with the host's own fingerprint). On success, pin
    /// the peer and burn the ticket. `Err` carries a short user-facing reason for `PairResult`.
    fn redeem(&self, peer: &PeerInfo, proof: &[u8]) -> Result<(), String>;

    /// The user allowed a pending USB phone: pin it.
    fn pin(&self, peer: &PeerInfo);

    /// A paired session was established or ended (`connected=false`). Updates last_seen/link.
    fn touch(&self, peer: &PeerInfo, connected: bool);
}

/// A USB phone waiting for Allow/Deny. Shown in the PC app (and as a tray notification).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPeer {
    pub session_id: String,
    pub peer: PeerInfo,
}

/// Simple in-memory store for tests and the fake phone tooling.
#[derive(Debug, Default)]
pub struct MemoryTrustStore {
    pub host_fp: Option<Fingerprint>,
    inner: std::sync::Mutex<MemoryInner>,
}

#[derive(Debug, Default)]
struct MemoryInner {
    pinned: Vec<Fingerprint>,
    ticket: Option<([u8; crate::pairing::SECRET_LEN], u64)>,
}

impl MemoryTrustStore {
    pub fn new(host_fp: Fingerprint) -> Self {
        Self { host_fp: Some(host_fp), inner: Default::default() }
    }

    /// Arm a single-use pairing secret valid until `expires_at` (unix seconds).
    pub fn set_ticket(&self, secret: [u8; crate::pairing::SECRET_LEN], expires_at: u64) {
        self.inner.lock().unwrap().ticket = Some((secret, expires_at));
    }

    pub fn unpin(&self, fp: &Fingerprint) {
        self.inner.lock().unwrap().pinned.retain(|p| p != fp);
    }

    pub fn pinned(&self) -> Vec<Fingerprint> {
        self.inner.lock().unwrap().pinned.clone()
    }
}

impl TrustStore for MemoryTrustStore {
    fn is_paired(&self, fp: &Fingerprint) -> bool {
        self.inner.lock().unwrap().pinned.contains(fp)
    }

    fn redeem(&self, peer: &PeerInfo, proof: &[u8]) -> Result<(), String> {
        let host_fp = self.host_fp.ok_or("host fingerprint unknown")?;
        let mut inner = self.inner.lock().unwrap();
        let (secret, expires_at) = inner.ticket.ok_or("no pairing code is active")?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if now > expires_at {
            inner.ticket = None;
            return Err("pairing code expired".into());
        }
        if !crate::pairing::verify_proof(&secret, &peer.fp, &host_fp, proof) {
            return Err("wrong pairing code".into());
        }
        inner.ticket = None;
        if !inner.pinned.contains(&peer.fp) {
            inner.pinned.push(peer.fp);
        }
        Ok(())
    }

    fn pin(&self, peer: &PeerInfo) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.pinned.contains(&peer.fp) {
            inner.pinned.push(peer.fp);
        }
    }

    fn touch(&self, _peer: &PeerInfo, _connected: bool) {}
}
