// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone-like connection logic on top of [`FakePhone`]: persistent identity, pinned hosts,
//! trust-on-first-use and QR pairing.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use nd_proto::v1::Goodbye;
use nd_transport::{Fingerprint, PairingUri};

use crate::client::FakePhone;
use crate::config::FakePhoneConfig;
use crate::error::FakePhoneError;
use crate::store::{goodbye_invalidates_pin, ClientIdentity, HostStore, PinnedHost};

/// What to connect to.
#[derive(Debug, Clone, Default)]
pub struct ConnectRequest {
    /// Explicit address. Overrides the addresses of the pairing URI / stored host.
    pub addr: Option<SocketAddr>,
    /// Pair with this QR link: pins its `f`, connects to its addresses, sends a `PairRequest`.
    pub pair_uri: Option<PairingUri>,
    /// Stored host to use (by `host_id`) when several are pinned.
    pub host_id: Option<String>,
    /// Override the pin (e.g. `--expect-host-fp`).
    pub expect_host_fp: Option<Fingerprint>,
}

/// A connected phone plus the host-store bookkeeping that went with it.
pub struct StoredSession {
    pub phone: FakePhone,
    /// The host entry saved (or refreshed) after Welcome.
    pub host: PinnedHost,
    /// True when no pin existed before and the host fingerprint was stored on first contact.
    pub trusted_on_first_use: bool,
}

/// Connect like the real phone does, persisting state in `dir` (see [`crate::store`]).
///
/// * pairing URI -> pin `f`, connect to its addresses (unless `addr`), send `PairRequest`
/// * stored host -> pin its fingerprint, connect to its addresses
/// * nothing known -> trust on first use (`addr` or `127.0.0.1:47800`), store the fingerprint
///
/// A `REVOKED` / `UNAUTHORIZED` Goodbye during the handshake drops the stored host.
pub async fn connect_stored(
    dir: &Path,
    req: ConnectRequest,
    mut config: FakePhoneConfig,
) -> Result<StoredSession, FakePhoneError> {
    let identity = Arc::new(ClientIdentity::load_or_create(dir)?);
    let mut store = HostStore::load(dir);
    config.identity = Some(identity);

    let mut addrs: Vec<SocketAddr> = Vec::new();
    let mut pin = None;
    let mut known = false;

    if let Some(uri) = &req.pair_uri {
        pin = Some(uri.host_fp);
        config.pair_secret = Some(uri.secret);
        addrs = uri.addrs.iter().map(|ip| SocketAddr::new(*ip, uri.port)).collect();
        known = store.hosts.iter().any(|h| h.host_id == uri.host_id);
    } else {
        let stored = match &req.host_id {
            Some(id) => Some(
                store.find(id).ok_or_else(|| FakePhoneError::Other(format!("no stored host with id {id}")))?.clone(),
            ),
            None => match store.hosts.as_slice() {
                [] => None,
                [one] => Some(one.clone()),
                _ => {
                    return Err(FakePhoneError::Other(
                        "several hosts are pinned; choose one with host_id (--host)".into(),
                    ))
                }
            },
        };
        if let Some(h) = stored {
            pin = Some(h.fp);
            addrs = h.addrs.iter().map(|ip| SocketAddr::new(*ip, h.port)).collect();
            known = true;
        }
    }
    if let Some(fp) = req.expect_host_fp {
        pin = Some(fp);
    }
    if let Some(a) = req.addr {
        addrs = vec![a];
    }
    if addrs.is_empty() {
        addrs.push(SocketAddr::from(([127, 0, 0, 1], nd_proto::CONTROL_PORT)));
    }
    config.pin = pin;

    let phone = match FakePhone::connect_any(&addrs, config).await {
        Ok(p) => p,
        Err(e) => {
            if let (FakePhoneError::Goodbye { reason, message }, Some(fp)) = (&e, pin) {
                let g = Goodbye { reason: *reason as i32, message: message.clone() };
                if goodbye_invalidates_pin(&g) && store.forget_fp(&fp) {
                    store.save()?;
                }
            }
            return Err(e);
        }
    };

    let welcome = phone.welcome().await.ok_or(FakePhoneError::ConnectionClosed)?;
    let paired = phone.pair_result().await;
    let host_id = if !welcome.host_id.is_empty() {
        welcome.host_id.clone()
    } else {
        paired.as_ref().map(|r| r.host_id.clone()).unwrap_or_default()
    };
    let used = phone_addr_hint(&addrs, req.addr);
    let host = PinnedHost {
        host_id,
        name: welcome.host_name.clone(),
        fp: phone.host_fp(),
        addrs: merge_ips(&store, &req, &used),
        port: used.first().map(|a| a.port()).unwrap_or(nd_proto::CONTROL_PORT),
    };
    store.upsert(host.clone());
    store.save()?;
    Ok(StoredSession { phone, host, trusted_on_first_use: !known && req.pair_uri.is_none() && pin.is_none() })
}

fn phone_addr_hint(addrs: &[SocketAddr], explicit: Option<SocketAddr>) -> Vec<SocketAddr> {
    explicit.map(|a| vec![a]).unwrap_or_else(|| addrs.to_vec())
}

/// Addresses to remember: the ones used now (loopback only when that is all we have).
fn merge_ips(store: &HostStore, req: &ConnectRequest, used: &[SocketAddr]) -> Vec<std::net::IpAddr> {
    let mut ips: Vec<std::net::IpAddr> = used.iter().map(|a| a.ip()).collect();
    if let Some(uri) = &req.pair_uri {
        if let Some(old) = store.find(&uri.host_id) {
            for ip in &old.addrs {
                if !ips.contains(ip) {
                    ips.push(*ip);
                }
            }
        }
    }
    ips
}

/// Forget the host with fingerprint `fp` when `goodbye` revokes the pin (REVOKED / UNAUTHORIZED).
/// Use after a session ended mid-way. Returns whether a stored host was dropped.
pub fn forget_if_invalidated(dir: &Path, fp: &Fingerprint, goodbye: &Goodbye) -> Result<bool, FakePhoneError> {
    if !goodbye_invalidates_pin(goodbye) {
        return Ok(false);
    }
    let mut store = HostStore::load(dir);
    let dropped = store.forget_fp(fp);
    if dropped {
        store.save()?;
    }
    Ok(dropped)
}
