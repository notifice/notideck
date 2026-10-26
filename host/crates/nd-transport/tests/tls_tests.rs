// SPDX-License-Identifier: GPL-3.0-or-later
//! Pairing, trust and TLS behaviour of the control server over real TLS on 127.0.0.1.

mod common;

use std::time::Duration;

use common::*;
use nd_proto::framing::{read_frame, write_frame};
use nd_proto::v1::{GoodbyeReason, Hello, PairRequest};
use nd_proto::{Envelope, Msg, PROTO_VERSION};
use nd_transport::pairing::{compute_proof, new_secret};
use nd_transport::{Fingerprint, TransportEvent, TrustStore};
use tokio::net::TcpStream;

fn far_future() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 300
}

fn pair_request(secret: &[u8; 16], phone: &Phone, host_fp: &Fingerprint) -> Msg {
    Msg::PairRequest(PairRequest {
        proof: compute_proof(secret, &phone.fp, host_fp).to_vec(),
        device_name: "My Phone".into(),
    })
}

async fn expect_goodbye<R: tokio::io::AsyncRead + Unpin>(s: &mut R, reason: GoodbyeReason) {
    match expect_skip_ping(s).await {
        Msg::Goodbye(g) => assert_eq!(g.reason, reason as i32, "message: {}", g.message),
        other => panic!("expected Goodbye({reason:?}), got {other:?}"),
    }
}

async fn expect_closed<R: tokio::io::AsyncRead + Unpin>(s: &mut R) {
    loop {
        match recv(s).await {
            None => return,
            Some(Msg::Ping(_)) => continue,
            Some(other) => panic!("expected close, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn v1_plaintext_phone_gets_version_mismatch() {
    let h = start(false).await;
    let mut tcp = TcpStream::connect(h.addr).await.unwrap();
    // A v1 Hello has no cert_fp / device_name.
    let hello = Hello { proto_version: 1, device_id: "old".into(), ..Default::default() };
    write_frame(&mut tcp, &Envelope::new(1, Msg::Hello(hello))).await.unwrap();
    let env = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut tcp)).await.unwrap().unwrap().unwrap();
    match env.msg {
        Some(Msg::Goodbye(g)) => {
            assert_eq!(g.reason, GoodbyeReason::VersionMismatch as i32);
            assert!(g.message.contains("Update the Notideck app"));
        }
        other => panic!("expected Goodbye, got {other:?}"),
    }
    assert_eq!(h.handle.session_count(), 0);
}

#[tokio::test]
async fn pinned_phone_gets_welcome_with_host_id() {
    let h = start(true).await; // LAN: a pinned phone needs no pairing on either link
    let phone = Phone::new("phone");
    h.trust.pin(&phone.peer_info());
    let mut s = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut s, Msg::Hello(hello("p1"))).await;
    match expect(&mut s).await {
        Msg::Welcome(w) => {
            assert_eq!(w.host_id, h.identity.host_id());
            assert_eq!(w.proto_version, PROTO_VERSION);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    let session = h.handle.active_session().unwrap();
    assert_eq!(session.link(), nd_transport::Link::Lan);
    assert_eq!(session.transport(), "lan");
}

#[tokio::test]
async fn usb_unknown_phone_pending_then_allowed() {
    let h = start(false).await;
    let mut events = h.handle.subscribe_events();
    let phone = Phone::new("phone");
    let mut s = phone.connect(h.addr, None).await.unwrap(); // trust on first use
    let mut hl = hello("unknown-usb");
    hl.device_name = "Xperia XZ".into();
    send(&mut s, Msg::Hello(hl)).await;

    match expect(&mut s).await {
        Msg::PairingPending(p) => assert!(!p.host_name.is_empty()),
        other => panic!("expected PairingPending, got {other:?}"),
    }
    // Pings are answered while pending.
    send(&mut s, Msg::Ping(nd_proto::v1::Ping { nonce: 7 })).await;
    assert!(matches!(expect(&mut s).await, Msg::Pong(p) if p.nonce == 7));

    let pending = loop {
        match events.recv().await.unwrap() {
            TransportEvent::PairingPending(p) => break p,
            _ => continue,
        }
    };
    assert_eq!(pending.peer.fp, phone.fp);
    assert_eq!(pending.peer.device_name, "Xperia XZ");
    assert_eq!(h.handle.pending(), vec![pending.clone()]);
    assert!(!h.trust.is_paired(&phone.fp));

    assert!(!h.handle.resolve_pending("no-such-id", true));
    assert!(h.handle.resolve_pending(&pending.session_id, true));
    assert!(!h.handle.resolve_pending(&pending.session_id, true), "second resolve must fail");

    assert!(matches!(expect_skip_ping(&mut s).await, Msg::Welcome(_)));
    assert!(h.trust.is_paired(&phone.fp));
    assert!(h.handle.pending().is_empty());
    loop {
        match events.recv().await.unwrap() {
            TransportEvent::PendingResolved(id) => {
                assert_eq!(id, pending.session_id);
                break;
            }
            _ => continue,
        }
    }
    // Reconnecting is now instant.
    drop(s);
    let mut s2 = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut s2, Msg::Hello(hello("unknown-usb"))).await;
    assert!(matches!(expect(&mut s2).await, Msg::Welcome(_)));
}

#[tokio::test]
async fn usb_unknown_phone_denied() {
    let h = start(false).await;
    let mut events = h.handle.subscribe_events();
    let phone = Phone::new("phone");
    let mut s = phone.connect(h.addr, None).await.unwrap();
    send(&mut s, Msg::Hello(hello("deny-me"))).await;
    assert!(matches!(expect(&mut s).await, Msg::PairingPending(_)));
    let id = loop {
        if let TransportEvent::PairingPending(p) = events.recv().await.unwrap() {
            break p.session_id;
        }
    };
    assert!(h.handle.resolve_pending(&id, false));
    expect_goodbye(&mut s, GoodbyeReason::PairingDenied).await;
    expect_closed(&mut s).await;
    assert!(!h.trust.is_paired(&phone.fp));
    assert_eq!(h.handle.session_count(), 0);
}

#[tokio::test]
async fn usb_pending_can_be_completed_by_qr_proof() {
    let h = start(false).await;
    let secret = new_secret();
    h.trust.set_ticket(secret, far_future());
    let phone = Phone::new("phone");
    let mut s = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut s, Msg::Hello(hello("both"))).await;
    assert!(matches!(expect(&mut s).await, Msg::PairingPending(_)));
    send(&mut s, pair_request(&secret, &phone, &h.host_fp)).await;
    assert!(matches!(expect(&mut s).await, Msg::PairResult(r) if r.ok));
    assert!(matches!(expect(&mut s).await, Msg::Welcome(_)));
    assert!(h.handle.pending().is_empty());
}

#[tokio::test]
async fn usb_pending_revoked_gets_goodbye() {
    let h = start(false).await;
    let phone = Phone::new("phone");
    let mut s = phone.connect(h.addr, None).await.unwrap();
    send(&mut s, Msg::Hello(hello("pending-revoke"))).await;
    assert!(matches!(expect(&mut s).await, Msg::PairingPending(_)));
    assert!(h.handle.revoke(&phone.fp));
    expect_goodbye(&mut s, GoodbyeReason::Revoked).await;
    expect_closed(&mut s).await;
    assert!(h.handle.pending().is_empty());
}

#[tokio::test]
async fn lan_qr_pairing_good_proof() {
    let h = start(true).await;
    let secret = new_secret();
    h.trust.set_ticket(secret, far_future());
    let phone = Phone::new("phone");
    let mut s = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut s, Msg::Hello(hello("qr-phone"))).await;
    // Nothing but pings until the PairRequest arrives; other messages are ignored.
    send(&mut s, Msg::Ping(nd_proto::v1::Ping { nonce: 1 })).await;
    assert!(matches!(expect(&mut s).await, Msg::Pong(_)));
    send(&mut s, pair_request(&secret, &phone, &h.host_fp)).await;
    match expect(&mut s).await {
        Msg::PairResult(r) => {
            assert!(r.ok, "error: {}", r.error);
            assert_eq!(r.host_id, h.identity.host_id());
            assert!(!r.host_name.is_empty());
        }
        other => panic!("expected PairResult, got {other:?}"),
    }
    assert!(matches!(expect(&mut s).await, Msg::Welcome(_)));
    assert!(h.trust.is_paired(&phone.fp));
    assert_eq!(h.handle.session_count(), 1);
    assert_eq!(h.handle.active_session().unwrap().link(), nd_transport::Link::Lan);
}

#[tokio::test]
async fn lan_qr_pairing_wrong_proof_and_replay() {
    let h = start(true).await;
    let secret = new_secret();
    h.trust.set_ticket(secret, far_future());

    // Wrong secret.
    let attacker = Phone::new("attacker");
    let mut s = attacker.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut s, Msg::Hello(hello("evil"))).await;
    send(&mut s, pair_request(&new_secret(), &attacker, &h.host_fp)).await;
    assert!(matches!(expect(&mut s).await, Msg::PairResult(r) if !r.ok && !r.error.is_empty()));
    expect_goodbye(&mut s, GoodbyeReason::Unauthorized).await;
    expect_closed(&mut s).await;
    assert!(!h.trust.is_paired(&attacker.fp));

    // The right secret still works (a failed attempt does not burn the ticket)...
    let phone = Phone::new("phone");
    let mut s = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut s, Msg::Hello(hello("good"))).await;
    send(&mut s, pair_request(&secret, &phone, &h.host_fp)).await;
    assert!(matches!(expect(&mut s).await, Msg::PairResult(r) if r.ok));
    assert!(matches!(expect(&mut s).await, Msg::Welcome(_)));

    // ...but only once: replaying it from another phone fails.
    let replayer = Phone::new("replayer");
    let mut s = replayer.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut s, Msg::Hello(hello("replay"))).await;
    send(&mut s, pair_request(&secret, &replayer, &h.host_fp)).await;
    assert!(matches!(expect(&mut s).await, Msg::PairResult(r) if !r.ok));
    expect_goodbye(&mut s, GoodbyeReason::Unauthorized).await;
    assert!(!h.trust.is_paired(&replayer.fp));
}

#[tokio::test]
async fn lan_unknown_phone_without_ticket_is_rejected() {
    let h = start(true).await;
    let phone = Phone::new("phone");
    let mut s = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut s, Msg::Hello(hello("no-ticket"))).await;
    send(&mut s, pair_request(&new_secret(), &phone, &h.host_fp)).await;
    assert!(matches!(expect(&mut s).await, Msg::PairResult(r) if !r.ok));
    expect_goodbye(&mut s, GoodbyeReason::Unauthorized).await;
}

#[tokio::test]
async fn revoke_closes_live_session() {
    let h = start(false).await;
    let phone = Phone::new("phone");
    h.trust.pin(&phone.peer_info());
    let mut s = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut s, Msg::Hello(hello("revoke-me"))).await;
    assert!(matches!(expect(&mut s).await, Msg::Welcome(_)));
    assert_eq!(h.handle.session_count(), 1);

    assert!(!h.handle.revoke(&Fingerprint([9; 32])));
    h.trust.unpin(&phone.fp);
    assert!(h.handle.revoke(&phone.fp));
    expect_goodbye(&mut s, GoodbyeReason::Revoked).await;
    expect_closed(&mut s).await;

    for _ in 0..50 {
        if h.handle.session_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(h.handle.session_count(), 0);
    assert!(!h.handle.revoke(&phone.fp));

    // And the phone is no longer trusted: back to pending.
    let mut s = phone.connect(h.addr, None).await.unwrap();
    send(&mut s, Msg::Hello(hello("revoke-me"))).await;
    assert!(matches!(expect(&mut s).await, Msg::PairingPending(_)));
}

#[tokio::test]
async fn second_session_supersedes_first() {
    let h = start(false).await;
    let phone = Phone::new("phone");
    h.trust.pin(&phone.peer_info());
    let mut a = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut a, Msg::Hello(hello("dup"))).await;
    assert!(matches!(expect(&mut a).await, Msg::Welcome(_)));

    let mut b = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut b, Msg::Hello(hello("dup"))).await;
    let second = match expect(&mut b).await {
        Msg::Welcome(w) => w.session_id,
        other => panic!("expected Welcome, got {other:?}"),
    };

    expect_goodbye(&mut a, GoodbyeReason::Superseded).await;
    expect_closed(&mut a).await;
    for _ in 0..50 {
        if h.handle.session_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(h.handle.session_count(), 1);
    assert_eq!(h.handle.active_session().unwrap().session_id(), second);
    // The newer session is unaffected.
    send(&mut b, Msg::Ping(nd_proto::v1::Ping { nonce: 5 })).await;
    assert!(matches!(expect_skip_ping(&mut b).await, Msg::Pong(p) if p.nonce == 5));
}

#[tokio::test]
async fn non_p256_client_certificate_is_rejected() {
    let h = start(false).await;
    let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let cert = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap().self_signed(&kp).unwrap();
    let res = connect_with(h.addr, cert.der().to_vec(), kp.serialize_der(), None).await;
    // TLS 1.3: the client may finish its side first; the failure shows on the first read.
    if let Ok(mut s) = res {
        let _ = write_frame(&mut s, &Envelope::new(1, Msg::Hello(hello("ed")))).await;
        assert!(recv(&mut s).await.is_none(), "server must not talk to an Ed25519 client");
    }
    assert_eq!(h.handle.session_count(), 0);
    assert!(h.handle.pending().is_empty());
}

#[tokio::test]
async fn client_without_certificate_is_rejected() {
    let h = start(false).await;
    // A TLS client that offers no certificate at all.
    let cfg =
        rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(AcceptAll))
            .with_no_client_auth();
    let tcp = TcpStream::connect(h.addr).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("x").unwrap();
    if let Ok(mut s) = tokio_rustls::TlsConnector::from(std::sync::Arc::new(cfg)).connect(name, tcp).await {
        let _ = write_frame(&mut s, &Envelope::new(1, Msg::Hello(hello("anon")))).await;
        assert!(recv(&mut s).await.is_none());
    }
    assert_eq!(h.handle.session_count(), 0);
}

#[derive(Debug)]
struct AcceptAll;
impl rustls::client::danger::ServerCertVerifier for AcceptAll {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![rustls::SignatureScheme::ECDSA_NISTP256_SHA256]
    }
}

#[tokio::test]
async fn client_pinning_wrong_host_fingerprint_fails() {
    let h = start(false).await;
    let phone = Phone::new("phone");
    h.trust.pin(&phone.peer_info());
    let res = phone.connect(h.addr, Some(Fingerprint([1; 32]))).await;
    assert!(res.is_err(), "pin mismatch must fail the handshake");
    // Correct pin works.
    assert!(phone.connect(h.addr, Some(h.host_fp)).await.is_ok());
}
