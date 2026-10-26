// SPDX-License-Identifier: GPL-3.0-or-later
//! Paired-device registry (`devices.json`) and the live pairing ticket (ADR 0004 §3–§5, §10).
//!
//! [`DeviceRegistry`] is the agent's [`TrustStore`]: the TLS server asks it whether a certificate
//! is pinned, lets it redeem QR proofs and tells it about connects/disconnects. Everything is
//! in memory behind one mutex, plus an atomic file write when the pin list or `last_seen` changes.
//! Secrets and proofs are never logged.

use nd_transport::pairing::{self, Fingerprint, SECRET_LEN, TICKET_TTL_SECS};
use nd_transport::{Link, PeerInfo, TrustStore};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::{info, warn};

pub const FILE: &str = "devices.json";

/// Failed proofs after which the live ticket is burned (stops brute-forcing the secret).
pub const MAX_FAILED_PROOFS: u32 = 5;

pub const ERR_EXPIRED: &str = "pairing code expired";
pub const ERR_USED: &str = "pairing code already used";
pub const ERR_NONE: &str = "no pairing code is active";
pub const ERR_WRONG: &str = "wrong pairing code";

/// One pinned phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    /// Lowercase hex certificate fingerprint: the registry id.
    pub fingerprint: String,
    pub name: String,
    pub model: String,
    pub device_id: String,
    /// Unix seconds.
    pub paired_at: u64,
    /// Unix seconds; 0 = never connected since pairing.
    #[serde(default)]
    pub last_seen: u64,
    /// "usb" | "lan" | "".
    #[serde(default)]
    pub last_link: String,
}

#[derive(Serialize, Deserialize, Default)]
struct FileFormat {
    #[serde(default)]
    devices: Vec<DeviceRecord>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TicketState {
    Live,
    Used,
}

struct Ticket {
    secret: [u8; SECRET_LEN],
    expires_at: u64,
    failures: u32,
    state: TicketState,
}

#[derive(Default)]
struct Inner {
    devices: Vec<DeviceRecord>,
    connected: HashMap<String, Link>,
    ticket: Option<Ticket>,
}

pub struct DeviceRegistry {
    path: PathBuf,
    host_fp: Fingerprint,
    inner: Mutex<Inner>,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl DeviceRegistry {
    /// Load `<dir>/devices.json` (missing = empty; unreadable = moved aside and empty).
    pub fn open(dir: &Path, host_fp: Fingerprint) -> Self {
        let path = dir.join(FILE);
        let devices = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<FileFormat>(&bytes) {
                Ok(f) => f.devices,
                Err(e) => {
                    warn!("devices.json is invalid ({e}); moving it aside and starting empty");
                    let _ = std::fs::rename(&path, dir.join("devices.json.bad"));
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Self { path, host_fp, inner: Mutex::new(Inner { devices, ..Default::default() }) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, inner: &Inner) {
        if let Err(e) = write_atomic(&self.path, &inner.devices) {
            warn!("cannot save devices.json: {e}");
        }
    }

    /// Paired records with the current link when connected.
    pub fn list(&self) -> Vec<(DeviceRecord, Option<Link>)> {
        let g = self.lock();
        g.devices.iter().map(|d| (d.clone(), g.connected.get(&d.fingerprint).copied())).collect()
    }

    pub fn get(&self, fp_hex: &str) -> Option<DeviceRecord> {
        self.lock().devices.iter().find(|d| d.fingerprint == fp_hex).cloned()
    }

    /// Remove a pin. Returns whether it existed.
    pub fn remove(&self, fp_hex: &str) -> bool {
        let mut g = self.lock();
        let before = g.devices.len();
        g.devices.retain(|d| d.fingerprint != fp_hex);
        g.connected.remove(fp_hex);
        let removed = g.devices.len() != before;
        if removed {
            self.save(&g);
        }
        removed
    }

    // ---- tickets

    /// Replace any live ticket with a fresh one. Returns `(secret, expires_at)`.
    pub fn new_ticket(&self) -> ([u8; SECRET_LEN], u64) {
        self.new_ticket_at(now_secs())
    }

    pub(crate) fn new_ticket_at(&self, now: u64) -> ([u8; SECRET_LEN], u64) {
        let secret = pairing::new_secret();
        let expires_at = now + TICKET_TTL_SECS;
        self.lock().ticket = Some(Ticket { secret, expires_at, failures: 0, state: TicketState::Live });
        (secret, expires_at)
    }

    pub fn cancel_ticket(&self) {
        self.lock().ticket = None;
    }

    pub fn ticket_active(&self) -> bool {
        let g = self.lock();
        matches!(&g.ticket, Some(t) if t.state == TicketState::Live && now_secs() <= t.expires_at)
    }

    pub(crate) fn redeem_at(&self, peer: &PeerInfo, proof: &[u8], now: u64) -> Result<(), String> {
        let mut g = self.lock();
        let t = g.ticket.as_mut().ok_or_else(|| ERR_NONE.to_string())?;
        if t.state == TicketState::Used {
            return Err(ERR_USED.into());
        }
        if now > t.expires_at {
            return Err(ERR_EXPIRED.into());
        }
        if !pairing::verify_proof(&t.secret, &peer.fp, &self.host_fp, proof) {
            t.failures += 1;
            if t.failures >= MAX_FAILED_PROOFS {
                t.state = TicketState::Used;
                warn!("pairing code burned after {MAX_FAILED_PROOFS} wrong proofs");
            }
            return Err(ERR_WRONG.into());
        }
        t.state = TicketState::Used;
        pin_locked(&mut g, peer, now);
        self.save(&g);
        info!(fp = %peer.fp.short(), name = %peer.device_name, "phone paired over QR");
        Ok(())
    }
}

fn pin_locked(g: &mut Inner, peer: &PeerInfo, now: u64) {
    let id = peer.fp.to_hex();
    match g.devices.iter_mut().find(|d| d.fingerprint == id) {
        Some(d) => {
            d.name = peer.device_name.clone();
            d.model = peer.device_model.clone();
            d.device_id = peer.device_id.clone();
        }
        None => g.devices.push(DeviceRecord {
            fingerprint: id,
            name: peer.device_name.clone(),
            model: peer.device_model.clone(),
            device_id: peer.device_id.clone(),
            paired_at: now,
            last_seen: 0,
            last_link: String::new(),
        }),
    }
}

fn write_atomic(path: &Path, devices: &[DeviceRecord]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(&FileFormat { devices: devices.to_vec() }).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, path)
}

impl TrustStore for DeviceRegistry {
    fn is_paired(&self, fp: &Fingerprint) -> bool {
        let id = fp.to_hex();
        self.lock().devices.iter().any(|d| d.fingerprint == id)
    }

    fn redeem(&self, peer: &PeerInfo, proof: &[u8]) -> Result<(), String> {
        self.redeem_at(peer, proof, now_secs())
    }

    fn pin(&self, peer: &PeerInfo) {
        let mut g = self.lock();
        pin_locked(&mut g, peer, now_secs());
        self.save(&g);
        info!(fp = %peer.fp.short(), name = %peer.device_name, "phone allowed over USB");
    }

    fn touch(&self, peer: &PeerInfo, connected: bool) {
        let id = peer.fp.to_hex();
        let mut g = self.lock();
        let Some(d) = g.devices.iter_mut().find(|d| d.fingerprint == id) else { return };
        d.last_seen = now_secs();
        if connected {
            d.last_link = peer.link.as_str().to_string();
            g.connected.insert(id, peer.link);
        } else {
            g.connected.remove(&id);
        }
        self.save(&g);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nd-devices-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    pub fn peer(seed: &[u8], link: Link) -> PeerInfo {
        PeerInfo {
            fp: Fingerprint::of_der(seed),
            device_id: "dev-1".into(),
            device_name: "Pixel".into(),
            device_model: "Pixel 6".into(),
            addr: "127.0.0.1:1".parse().unwrap(),
            link,
        }
    }

    fn host() -> Fingerprint {
        Fingerprint::of_der(b"host")
    }

    fn proof_for(secret: &[u8; SECRET_LEN], p: &PeerInfo) -> Vec<u8> {
        pairing::compute_proof(secret, &p.fp, &host()).to_vec()
    }

    #[test]
    fn persists_and_reloads() {
        let d = tmpdir("persist");
        let reg = DeviceRegistry::open(&d, host());
        let p = peer(b"phone", Link::Usb);
        assert!(!reg.is_paired(&p.fp));
        reg.pin(&p);
        assert!(reg.is_paired(&p.fp));
        assert!(!d.join("devices.json.tmp").exists());
        let reg2 = DeviceRegistry::open(&d, host());
        assert!(reg2.is_paired(&p.fp));
        let rec = reg2.get(&p.fp.to_hex()).unwrap();
        assert_eq!((rec.name.as_str(), rec.model.as_str(), rec.device_id.as_str()), ("Pixel", "Pixel 6", "dev-1"));
        assert!(rec.paired_at > 0);
    }

    #[test]
    fn unpin_removes_and_persists() {
        let d = tmpdir("unpin");
        let reg = DeviceRegistry::open(&d, host());
        let p = peer(b"phone", Link::Usb);
        reg.pin(&p);
        assert!(reg.remove(&p.fp.to_hex()));
        assert!(!reg.remove(&p.fp.to_hex()));
        assert!(!reg.is_paired(&p.fp));
        assert!(!DeviceRegistry::open(&d, host()).is_paired(&p.fp));
    }

    #[test]
    fn touch_tracks_connection_and_last_seen() {
        let d = tmpdir("touch");
        let reg = DeviceRegistry::open(&d, host());
        let p = peer(b"phone", Link::Lan);
        reg.touch(&p, true); // unknown phone: ignored
        assert!(reg.list().is_empty());
        reg.pin(&p);
        assert_eq!(reg.list()[0].1, None);
        reg.touch(&p, true);
        let (rec, link) = reg.list().remove(0);
        assert_eq!(link, Some(Link::Lan));
        assert_eq!(rec.last_link, "lan");
        assert!(rec.last_seen > 0);
        reg.touch(&p, false);
        assert_eq!(reg.list()[0].1, None);
        assert_eq!(DeviceRegistry::open(&d, host()).list()[0].0.last_link, "lan");
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let d = tmpdir("corrupt");
        std::fs::write(d.join(FILE), b"{nope").unwrap();
        let reg = DeviceRegistry::open(&d, host());
        assert!(reg.list().is_empty());
        assert!(d.join("devices.json.bad").exists());
    }

    #[test]
    fn ticket_redeems_once() {
        let reg = DeviceRegistry::open(&tmpdir("once"), host());
        let p = peer(b"phone", Link::Lan);
        assert_eq!(reg.redeem_at(&p, b"x", 100), Err(ERR_NONE.into()));
        let (secret, exp) = reg.new_ticket_at(100);
        assert_eq!(exp, 100 + TICKET_TTL_SECS);
        assert_eq!(reg.redeem_at(&p, &proof_for(&secret, &p), 150), Ok(()));
        assert!(reg.is_paired(&p.fp));
        let p2 = peer(b"other", Link::Lan);
        assert_eq!(reg.redeem_at(&p2, &proof_for(&secret, &p2), 151), Err(ERR_USED.into()));
        assert!(!reg.is_paired(&p2.fp));
        // a new ticket works again; cancelling removes it
        let (s2, _) = reg.new_ticket_at(200);
        reg.cancel_ticket();
        assert_eq!(reg.redeem_at(&p2, &proof_for(&s2, &p2), 201), Err(ERR_NONE.into()));
    }

    #[test]
    fn ticket_expires() {
        let reg = DeviceRegistry::open(&tmpdir("expiry"), host());
        let p = peer(b"phone", Link::Lan);
        let (secret, exp) = reg.new_ticket_at(1000);
        assert_eq!(reg.redeem_at(&p, &proof_for(&secret, &p), exp + 1), Err(ERR_EXPIRED.into()));
        assert!(!reg.is_paired(&p.fp));
        // right at the deadline is still fine
        let (secret, exp) = reg.new_ticket_at(1000);
        assert_eq!(reg.redeem_at(&p, &proof_for(&secret, &p), exp), Ok(()));
    }

    #[test]
    fn wrong_proof_and_five_failure_burn() {
        let reg = DeviceRegistry::open(&tmpdir("burn"), host());
        let p = peer(b"phone", Link::Lan);
        let (secret, _) = reg.new_ticket_at(10);
        for _ in 0..MAX_FAILED_PROOFS - 1 {
            assert_eq!(reg.redeem_at(&p, b"garbage", 11), Err(ERR_WRONG.into()));
        }
        // still alive: the right proof would work now
        assert_eq!(reg.redeem_at(&p, b"garbage", 11), Err(ERR_WRONG.into()));
        // burned: even the right proof is refused
        assert_eq!(reg.redeem_at(&p, &proof_for(&secret, &p), 12), Err(ERR_USED.into()));
        assert!(!reg.is_paired(&p.fp));
    }

    #[test]
    fn proof_bound_to_client_cert() {
        let reg = DeviceRegistry::open(&tmpdir("bound"), host());
        let (p, q) = (peer(b"phone", Link::Lan), peer(b"mitm", Link::Lan));
        let (secret, _) = reg.new_ticket_at(10);
        assert_eq!(reg.redeem_at(&q, &proof_for(&secret, &p), 11), Err(ERR_WRONG.into()));
    }
}
