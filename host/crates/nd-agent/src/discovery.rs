// SPDX-License-Identifier: GPL-3.0-or-later
//! mDNS advertisement of the control port (`_notideck._tcp`, ADR 0004 §7).
//!
//! Runs only while LAN mode is effective. Re-registers when the set of IPv4 addresses changes
//! (polled). Any mDNS failure is logged once and otherwise ignored: pairing by QR and the
//! phone's remembered addresses keep working without it.

use mdns_sd::{ServiceDaemon, ServiceInfo};
use nd_transport::pairing::{Fingerprint, MDNS_SERVICE_TYPE};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{info, warn};

const POLL: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Advert {
    pub host_id: String,
    pub host_name: String,
    pub fp: Fingerprint,
    pub port: u16,
}

/// Flags the selftest/UI can read.
#[derive(Default)]
pub struct DiscoveryStatus {
    pub started: AtomicBool,
}

/// DNS-safe host label for the `.local.` name.
fn host_label(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if s.is_empty() {
        "notideck-host".into()
    } else {
        s
    }
}

fn service_info(a: &Advert, addrs: &[Ipv4Addr]) -> Result<ServiceInfo, mdns_sd::Error> {
    let props: HashMap<String, String> =
        [("id", a.host_id.clone()), ("name", a.host_name.clone()), ("proto", "2".to_string()), ("fp8", a.fp.short())]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
    let ips: Vec<IpAddr> = addrs.iter().map(|a| IpAddr::V4(*a)).collect();
    ServiceInfo::new(
        MDNS_SERVICE_TYPE,
        &a.host_name,
        &format!("{}.local.", host_label(&a.host_name)),
        &ips[..],
        a.port,
        props,
    )
}

/// Advertise while `lan` is true. `addrs` supplies the current address set (injected for tests).
pub async fn run(
    advert: Advert,
    mut lan: watch::Receiver<bool>,
    status: Arc<DiscoveryStatus>,
    addrs: impl Fn() -> Vec<Ipv4Addr> + Send + 'static,
) {
    let mut daemon: Option<ServiceDaemon> = None;
    let mut registered: Vec<Ipv4Addr> = Vec::new();
    let mut warned = false;
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let want = if *lan.borrow() { addrs() } else { Vec::new() };
        if want != registered {
            if want.is_empty() {
                if let Some(d) = daemon.take() {
                    let _ = d.shutdown();
                    info!("mDNS advertisement stopped");
                }
                status.started.store(false, Ordering::SeqCst);
            } else {
                if daemon.is_none() {
                    match ServiceDaemon::new() {
                        Ok(d) => daemon = Some(d),
                        Err(e) => {
                            if !warned {
                                warn!("mDNS is unavailable ({e}); phones must use the QR code or saved addresses");
                                warned = true;
                            }
                        }
                    }
                }
                if let Some(d) = &daemon {
                    match service_info(&advert, &want).and_then(|i| d.register(i)) {
                        Ok(()) => {
                            info!(addrs = ?want, port = advert.port, "advertising _notideck._tcp");
                            status.started.store(true, Ordering::SeqCst);
                        }
                        Err(e) => {
                            if !warned {
                                warn!("cannot register the mDNS service: {e}");
                                warned = true;
                            }
                            status.started.store(false, Ordering::SeqCst);
                        }
                    }
                }
            }
            registered = want;
        }
        tokio::select! {
            _ = tick.tick() => {}
            r = lan.changed() => { if r.is_err() { break; } }
        }
    }
    if let Some(d) = daemon {
        let _ = d.shutdown();
    }
    status.started.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_label_is_dns_safe() {
        assert_eq!(host_label("Alex's PC"), "Alex-s-PC");
        assert_eq!(host_label("***"), "notideck-host");
    }

    #[test]
    fn service_info_has_txt_fields() {
        let a =
            Advert { host_id: "id-1".into(), host_name: "My PC".into(), fp: Fingerprint::of_der(b"h"), port: 47800 };
        let i = service_info(&a, &["192.168.1.5".parse().unwrap()]).unwrap();
        assert_eq!(i.get_port(), 47800);
        assert_eq!(i.get_property_val_str("proto"), Some("2"));
        assert_eq!(i.get_property_val_str("id"), Some("id-1"));
        assert_eq!(i.get_property_val_str("fp8"), Some(a.fp.short().as_str()));
        assert!(i.get_fullname().ends_with("_notideck._tcp.local."));
    }
}
