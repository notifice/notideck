// SPDX-License-Identifier: GPL-3.0-or-later
//! Agent side of QR pairing: which addresses go into the code, and ticket creation.

use crate::devices::DeviceRegistry;
use nd_config::{BackendError, PairingTicket};
use nd_transport::pairing::PairingUri;
use nd_transport::HostIdentity;
use std::net::Ipv4Addr;

/// Adapter name fragments that identify virtual or tunnel adapters a phone cannot reach.
const VIRTUAL_NAMES: &[&str] = &[
    "vethernet",
    "wsl",
    "hyper-v",
    "docker",
    "veth",
    "virbr",
    "br-",
    "vmware",
    "vmnet",
    "virtualbox",
    "vboxnet",
    "loopback",
    "isatap",
    "teredo",
    "pseudo",
    "bluetooth",
];

fn is_virtual(name: &str) -> bool {
    let n = name.to_lowercase();
    VIRTUAL_NAMES.iter().any(|v| n.contains(v)) || n == "lo" || n.starts_with("docker")
}

fn is_private(ip: &Ipv4Addr) -> bool {
    ip.is_private()
}

/// Pick the addresses to advertise from `(adapter name, address)` pairs: drop loopback,
/// link-local, unspecified and virtual adapters; private ranges first, then the rest; no duplicates.
pub fn select_addrs(candidates: &[(String, Ipv4Addr)]) -> Vec<Ipv4Addr> {
    let mut ok: Vec<Ipv4Addr> = Vec::new();
    for (name, ip) in candidates {
        if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast() || is_virtual(name) {
            continue;
        }
        if !ok.contains(ip) {
            ok.push(*ip);
        }
    }
    // Stable partition: private ranges first.
    let (mut private, other): (Vec<_>, Vec<_>) = ok.into_iter().partition(is_private);
    private.extend(other);
    private
}

/// The host's reachable IPv4 addresses right now (empty if enumeration fails).
pub fn host_addrs() -> Vec<Ipv4Addr> {
    let ifs = match if_addrs::get_if_addrs() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("cannot list network interfaces: {e}");
            return Vec::new();
        }
    };
    let cands: Vec<(String, Ipv4Addr)> = ifs
        .into_iter()
        .filter(|i| i.is_oper_up())
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(v4) => Some((i.name, v4)),
            _ => None,
        })
        .collect();
    select_addrs(&cands)
}

/// Create a ticket for the QR code. `addrs` come from [`host_addrs`] (injected for tests).
pub fn create_ticket(
    registry: &DeviceRegistry,
    identity: &HostIdentity,
    host_name: &str,
    addrs: &[Ipv4Addr],
    port: u16,
) -> Result<PairingTicket, BackendError> {
    if addrs.is_empty() {
        return Err(BackendError::Conflict(
            "no network connection found: connect this PC to Wi-Fi or Ethernet to pair over Wi-Fi".into(),
        ));
    }
    let (secret, expires_at) = registry.new_ticket();
    let uri = PairingUri {
        host_id: identity.host_id().to_string(),
        host_name: host_name.to_string(),
        addrs: addrs.iter().map(|a| (*a).into()).collect(),
        port,
        host_fp: identity.fingerprint(),
        secret,
        expires_at,
    }
    .to_uri();
    Ok(PairingTicket {
        qr_svg: nd_config::qr_svg(&uri),
        uri,
        expires_at,
        addrs: addrs.iter().map(|a| a.to_string()).collect(),
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, ip: &str) -> (String, Ipv4Addr) {
        (name.into(), ip.parse().unwrap())
    }

    #[test]
    fn filters_and_orders() {
        let got = select_addrs(&[
            c("lo", "127.0.0.1"),
            c("Loopback Pseudo-Interface 1", "127.0.0.1"),
            c("Ethernet", "169.254.3.4"),
            c("vEthernet (WSL)", "172.20.0.1"),
            c("vEthernet (Default Switch)", "172.30.0.1"),
            c("docker0", "172.17.0.1"),
            c("br-1a2b", "172.18.0.1"),
            c("VMware Network Adapter VMnet8", "192.168.100.1"),
            c("Tailscale", "100.64.1.2"),
            c("Wi-Fi", "192.168.1.20"),
            c("Wi-Fi", "192.168.1.20"),
            c("Ethernet 2", "10.0.0.7"),
            c("eth0", "0.0.0.0"),
        ]);
        let want: Vec<Ipv4Addr> =
            ["192.168.1.20", "10.0.0.7", "100.64.1.2"].iter().map(|s| s.parse().unwrap()).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn empty_when_nothing_usable() {
        assert!(select_addrs(&[c("lo", "127.0.0.1")]).is_empty());
    }

    #[test]
    fn ticket_uri_roundtrips_and_is_single_live() {
        let dir = crate::devices::tests::tmpdir("ticket");
        let id = HostIdentity::load_or_create(&dir).unwrap();
        let reg = DeviceRegistry::open(&dir, id.fingerprint());
        let addrs = ["192.168.1.20".parse().unwrap()];
        let t = create_ticket(&reg, &id, "My PC", &addrs, 47800).unwrap();
        let parsed = PairingUri::parse(&t.uri).unwrap();
        assert_eq!(parsed.host_id, id.host_id());
        assert_eq!(parsed.host_fp, id.fingerprint());
        assert_eq!(parsed.port, 47800);
        assert_eq!(parsed.host_name, "My PC");
        assert_eq!(parsed.expires_at, t.expires_at);
        assert!(t.qr_svg.starts_with("<svg"));
        assert_eq!(t.addrs, ["192.168.1.20"]);
        assert!(reg.ticket_active());
        reg.cancel_ticket();
        assert!(!reg.ticket_active());
    }

    #[test]
    fn no_addresses_is_a_conflict() {
        let dir = crate::devices::tests::tmpdir("noaddr");
        let id = HostIdentity::load_or_create(&dir).unwrap();
        let reg = DeviceRegistry::open(&dir, id.fingerprint());
        assert!(matches!(create_ticket(&reg, &id, "PC", &[], 1), Err(BackendError::Conflict(_))));
        assert!(!reg.ticket_active());
    }
}
