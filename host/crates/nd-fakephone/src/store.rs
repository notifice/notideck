// SPDX-License-Identifier: GPL-3.0-or-later
//! Persistent state of the fake phone, mirroring what the real app keeps: its own TLS identity
//! and the hosts it has pinned (`hosts.json`).
//!
//! Layout of an identity directory:
//! * `client-cert.der`, `client-key.der` - ECDSA P-256 self-signed certificate and PKCS#8 key
//! * `hosts.json` - `[{host_id, name, fp, addrs, port}]`

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use nd_proto::v1::{Goodbye, GoodbyeReason};
use nd_transport::tls::generate_client_identity;
use nd_transport::Fingerprint;
use serde::{Deserialize, Serialize};

use crate::error::FakePhoneError;

pub const CERT_FILE: &str = "client-cert.der";
pub const KEY_FILE: &str = "client-key.der";
pub const HOSTS_FILE: &str = "hosts.json";

/// Default identity directory: `$XDG_STATE_HOME/notideck/fakephone`, else
/// `~/.local/state/notideck/fakephone`, else `<temp dir>/notideck-fakephone`.
pub fn default_identity_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(x).join("notideck").join("fakephone");
    }
    if let Some(h) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h).join(".local/state/notideck/fakephone");
    }
    std::env::temp_dir().join("notideck-fakephone")
}

/// The phone's TLS client certificate and key.
#[derive(Clone)]
pub struct ClientIdentity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fp: Fingerprint,
}

impl std::fmt::Debug for ClientIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientIdentity").field("fp", &self.fp.short()).finish_non_exhaustive()
    }
}

impl ClientIdentity {
    /// A fresh, unsaved identity.
    pub fn generate(name: &str) -> Result<Self, FakePhoneError> {
        let (cert_der, key_der) = generate_client_identity(name).map_err(|e| FakePhoneError::Tls(e.to_string()))?;
        let fp = Fingerprint::of_der(&cert_der);
        Ok(Self { cert_der, key_der, fp })
    }

    /// Load the identity from `dir`, generating and saving one when missing.
    pub fn load_or_create(dir: &Path) -> Result<Self, FakePhoneError> {
        std::fs::create_dir_all(dir)?;
        let (cp, kp) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
        if let (Ok(cert_der), Ok(key_der)) = (std::fs::read(&cp), std::fs::read(&kp)) {
            let fp = Fingerprint::of_der(&cert_der);
            return Ok(Self { cert_der, key_der, fp });
        }
        let id = Self::generate("Notideck Fake Phone")?;
        write_private(&cp, &id.cert_der)?;
        write_private(&kp, &id.key_der)?;
        Ok(id)
    }
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// A host the phone has pinned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedHost {
    pub host_id: String,
    pub name: String,
    /// Hex SHA-256 of the host certificate.
    pub fp: Fingerprint,
    pub addrs: Vec<IpAddr>,
    pub port: u16,
}

/// `hosts.json` in the identity directory.
#[derive(Debug, Clone)]
pub struct HostStore {
    path: PathBuf,
    pub hosts: Vec<PinnedHost>,
}

impl HostStore {
    /// Load `hosts.json` from `dir` (empty when missing or unreadable).
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(HOSTS_FILE);
        let hosts = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path, hosts }
    }

    pub fn save(&self) -> Result<(), FakePhoneError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(&self.hosts).map_err(|e| FakePhoneError::Other(e.to_string()))?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    /// Insert or replace (by `host_id`).
    pub fn upsert(&mut self, host: PinnedHost) {
        match self.hosts.iter_mut().find(|h| h.host_id == host.host_id) {
            Some(slot) => *slot = host,
            None => self.hosts.push(host),
        }
    }

    pub fn find(&self, host_id: &str) -> Option<&PinnedHost> {
        self.hosts.iter().find(|h| h.host_id == host_id)
    }

    /// Drop every host with certificate fingerprint `fp`. Returns whether one existed.
    pub fn forget_fp(&mut self, fp: &Fingerprint) -> bool {
        let before = self.hosts.len();
        self.hosts.retain(|h| &h.fp != fp);
        self.hosts.len() != before
    }
}

/// Whether a Goodbye means "this host no longer trusts us": the stored pin is useless.
pub fn goodbye_invalidates_pin(g: &Goodbye) -> bool {
    matches!(GoodbyeReason::try_from(g.reason), Ok(GoodbyeReason::Revoked) | Ok(GoodbyeReason::Unauthorized))
}

/// Human-readable explanation of a Goodbye reason, for the CLI.
pub fn describe_goodbye(reason: GoodbyeReason, message: &str) -> String {
    let text = match reason {
        GoodbyeReason::Revoked => "this phone was removed (revoked) on the PC; stored host forgotten, pair again",
        GoodbyeReason::Unauthorized => {
            "the PC does not trust this phone (unauthorized); stored host forgotten, pair again"
        }
        GoodbyeReason::PairingDenied => "the PC refused this phone (Deny pressed)",
        GoodbyeReason::Unpaired => "this phone is not paired with the PC (pairing timed out)",
        GoodbyeReason::VersionMismatch => "protocol version mismatch, update the older side",
        GoodbyeReason::Superseded => "another connection (e.g. USB) replaced this session",
        GoodbyeReason::Shutdown => "the host is shutting down",
        GoodbyeReason::Unspecified => "the host closed the connection",
    };
    if message.is_empty() {
        text.to_string()
    } else {
        format!("{text} (host says: {message})")
    }
}
