// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake phone against a real `nd-transport` TLS `ControlServer`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nd_fakephone::{
    connect_stored, forget_if_invalidated, plaintext_v1_probe, ConnectRequest, FakePhoneConfig, FakePhoneError,
    HostStore, PendingHook,
};
use nd_proto::v1::{GoodbyeReason, SetLayout};
use nd_transport::pairing::new_secret;
use nd_transport::{
    ControlServer, Fingerprint, HostIdentity, Link, MemoryTrustStore, PairingUri, ServerHandle, TrustStore,
};
use tokio::sync::watch;

fn always_lan(_: &SocketAddr) -> Link {
    Link::Lan
}

struct Host {
    addr: SocketAddr,
    handle: ServerHandle,
    identity: Arc<HostIdentity>,
    trust: Arc<MemoryTrustStore>,
    shutdown: watch::Sender<bool>,
    dirs: Vec<PathBuf>,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        for d in &self.dirs {
            std::fs::remove_dir_all(d).ok();
        }
    }
}

fn tmp(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("nd-fakephone-{label}-{}", uuid::Uuid::new_v4()))
}

async fn start(lan: bool) -> Host {
    let dir = tmp("host");
    let identity = Arc::new(HostIdentity::load_or_create(&dir).unwrap());
    let trust = Arc::new(MemoryTrustStore::new(identity.fingerprint()));
    let mut server =
        ControlServer::bind_tls("127.0.0.1:0", identity.clone(), trust.clone() as Arc<dyn TrustStore>).await.unwrap();
    if lan {
        server.set_link_classifier(always_lan);
    }
    let addr = server.local_addr();
    let handle = server.handle();
    let (_task, shutdown) = server.spawn();
    Host { addr, handle, identity, trust, shutdown, dirs: vec![dir] }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn uri_for(host: &Host, secret: [u8; 16], expires_at: u64) -> PairingUri {
    PairingUri {
        host_id: host.identity.host_id().to_string(),
        host_name: "Test PC".into(),
        addrs: vec![host.addr.ip()],
        port: host.addr.port(),
        host_fp: host.identity.fingerprint(),
        secret,
        expires_at,
    }
}

fn cfg() -> FakePhoneConfig {
    FakePhoneConfig { require_layout: false, ..Default::default() }
}

fn layout() -> SetLayout {
    SetLayout { page_id: "main".into(), cols: 5, rows: 3, controls: vec![] }
}

#[tokio::test]
async fn usb_pending_then_allow_gets_layout() {
    let host = start(false).await;
    let dir = tmp("phone");
    let saw_pending = Arc::new(AtomicBool::new(false));
    let mut config = cfg();
    config.on_pending = Some(PendingHook(Arc::new({
        let f = saw_pending.clone();
        move |_| f.store(true, Ordering::SeqCst)
    })));
    let task = tokio::spawn({
        let (dir, addr) = (dir.clone(), host.addr);
        async move { connect_stored(&dir, ConnectRequest { addr: Some(addr), ..Default::default() }, config).await }
    });

    let pending = loop {
        if let Some(p) = host.handle.pending().into_iter().next() {
            break p;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(pending.peer.link, Link::Usb);
    assert!(!task.is_finished(), "phone must wait for approval");
    assert!(host.handle.resolve_pending(&pending.session_id, true));

    let s = task.await.unwrap().expect("session after allow");
    assert!(saw_pending.load(Ordering::SeqCst));
    assert!(s.phone.was_pending().await);
    assert!(s.trusted_on_first_use);
    assert_eq!(s.phone.host_fp(), host.identity.fingerprint());
    assert_eq!(s.host.host_id, host.identity.host_id());
    assert!(host.trust.is_paired(&s.phone.own_fp()));

    host.handle.set_layout(layout()).await.unwrap();
    let l = s.phone.ensure_layout(Duration::from_secs(3)).await.unwrap();
    assert_eq!(l.page_id, "main");

    // The pin was stored (TOFU).
    let store = HostStore::load(&dir);
    assert_eq!(store.hosts.len(), 1);
    assert_eq!(store.hosts[0].fp, host.identity.fingerprint());
    assert_eq!(store.hosts[0].port, host.addr.port());

    // Pings keep working: the server pings every 2 s.
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert!(s.phone.pings_received_count().await >= 1);
    drop(s);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn usb_pending_deny() {
    let host = start(false).await;
    let dir = tmp("phone");
    let task = tokio::spawn({
        let (dir, addr) = (dir.clone(), host.addr);
        async move { connect_stored(&dir, ConnectRequest { addr: Some(addr), ..Default::default() }, cfg()).await }
    });
    let pending = loop {
        if let Some(p) = host.handle.pending().into_iter().next() {
            break p;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    host.handle.resolve_pending(&pending.session_id, false);
    match task.await.unwrap() {
        Err(FakePhoneError::Goodbye { reason: GoodbyeReason::PairingDenied, .. }) => {}
        other => panic!("expected PairingDenied, got {:?}", other.err()),
    }
    // Nothing was stored for a refused host.
    assert!(HostStore::load(&dir).hosts.is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn qr_pairing_over_lan_then_reconnect_with_stored_pin() {
    let host = start(true).await;
    let dir = tmp("phone");
    let secret = new_secret();
    host.trust.set_ticket(secret, now() + 300);
    let uri = uri_for(&host, secret, now() + 300);

    let s = connect_stored(&dir, ConnectRequest { pair_uri: Some(uri), ..Default::default() }, cfg())
        .await
        .expect("QR pairing");
    let r = s.phone.pair_result().await.expect("PairResult");
    assert!(r.ok);
    assert_eq!(r.host_id, host.identity.host_id());
    assert!(!s.phone.was_pending().await);
    assert!(host.trust.is_paired(&s.phone.own_fp()));
    assert_eq!(s.host.host_id, host.identity.host_id());
    let own_fp = s.phone.own_fp();
    drop(s);

    // Later: no URI, no address. The stored pin and address are used.
    let s2 = connect_stored(&dir, ConnectRequest::default(), cfg()).await.expect("reconnect with stored pin");
    assert!(!s2.trusted_on_first_use);
    assert_eq!(s2.phone.own_fp(), own_fp, "identity persists");
    assert_eq!(s2.phone.host_fp(), host.identity.fingerprint());
    host.handle.set_layout(layout()).await.unwrap();
    s2.phone.ensure_layout(Duration::from_secs(3)).await.unwrap();
    drop(s2);

    // A wrong expected fingerprint is refused at the TLS layer.
    let wrong = ConnectRequest { expect_host_fp: Some(Fingerprint([7; 32])), ..Default::default() };
    match connect_stored(&dir, wrong, cfg()).await {
        Err(FakePhoneError::Tls(m)) => assert!(m.contains("mismatch"), "{m}"),
        other => panic!("expected TLS pin mismatch, got {:?}", other.err()),
    }
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn revoke_ends_session_and_forgets_host() {
    let host = start(true).await;
    let dir = tmp("phone");
    let secret = new_secret();
    host.trust.set_ticket(secret, now() + 300);
    let s = connect_stored(
        &dir,
        ConnectRequest { pair_uri: Some(uri_for(&host, secret, now() + 300)), ..Default::default() },
        cfg(),
    )
    .await
    .unwrap();
    assert_eq!(HostStore::load(&dir).hosts.len(), 1);

    let fp = s.phone.own_fp();
    host.trust.unpin(&fp);
    assert!(host.handle.revoke(&fp));
    assert!(s.phone.wait_closed(Some(Duration::from_secs(3))).await);
    let g = s.phone.goodbye().await.expect("goodbye");
    assert_eq!(g.reason, GoodbyeReason::Revoked as i32);
    assert!(forget_if_invalidated(&dir, &s.phone.host_fp(), &g).unwrap());
    assert!(HostStore::load(&dir).hosts.is_empty());
    drop(s);

    // A revoked phone that tries QR pairing with a wrong code is rejected, not paired again.
    let bad = uri_for(&host, new_secret(), now() + 300);
    let res = connect_stored(&dir, ConnectRequest { pair_uri: Some(bad), ..Default::default() }, cfg()).await;
    assert!(matches!(res, Err(FakePhoneError::PairingRejected(_))), "{:?}", res.err());
    assert!(!host.trust.is_paired(&fp));
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn expired_ticket_is_rejected() {
    let host = start(true).await;
    let dir = tmp("phone");
    let secret = new_secret();
    host.trust.set_ticket(secret, now() - 10);
    let res = connect_stored(
        &dir,
        ConnectRequest { pair_uri: Some(uri_for(&host, secret, now() - 10)), ..Default::default() },
        cfg(),
    )
    .await;
    match res {
        Err(FakePhoneError::PairingRejected(m)) => assert!(m.contains("expired"), "{m}"),
        other => panic!("expected rejection, got {:?}", other.err()),
    }
    assert!(HostStore::load(&dir).hosts.is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn v1_plaintext_phone_is_refused() {
    let host = start(false).await;
    let g = plaintext_v1_probe(host.addr, &cfg()).await.expect("goodbye");
    assert_eq!(g.reason, GoodbyeReason::VersionMismatch as i32);
    assert!(g.message.contains("Update the Notideck app"), "{}", g.message);
}
