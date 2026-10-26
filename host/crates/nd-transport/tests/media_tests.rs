// SPDX-License-Identifier: GPL-3.0-or-later
mod common;

use std::time::Duration;

use common::*;
use nd_proto::media::{write_packet, MediaPacket};
use nd_proto::v1::{CameraStart, CameraState, CameraStatus, MediaAttach};
use nd_proto::Msg;
use nd_transport::{TransportEvent, TrustStore};
use tokio::time::timeout;

/// Connect a pinned phone, return its control stream and the session id.
async fn control(h: &Harness, phone: &Phone, id: &str) -> (Stream, String) {
    h.trust.pin(&phone.peer_info());
    let mut s = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut s, Msg::Hello(hello(id))).await;
    match expect_skip_ping(&mut s).await {
        Msg::Welcome(w) => (s, w.session_id),
        other => panic!("expected Welcome, got {other:?}"),
    }
}

async fn attach(h: &Harness, phone: &Phone, session_id: &str) -> (Stream, bool, String) {
    let mut m = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut m, Msg::MediaAttach(MediaAttach { session_id: session_id.into(), ..Default::default() })).await;
    match recv(&mut m).await {
        Some(Msg::MediaAttached(a)) => (m, a.ok, a.error),
        other => panic!("expected MediaAttached, got {other:?}"),
    }
}

fn pkt(n: u64) -> MediaPacket {
    MediaPacket::new(n * 33_000, false, n == 0, vec![0, 0, 0, 1, 0x65, n as u8])
}

#[tokio::test]
async fn attach_ok_packets_flow() {
    let mut h = start(false).await;
    let phone = Phone::new("cam");
    let (_c, sid) = control(&h, &phone, "dev1").await;
    let (mut m, ok, _) = attach(&h, &phone, &sid).await;
    assert!(ok);
    let mut ms = timeout(Duration::from_secs(2), h.media_rx.recv()).await.unwrap().unwrap();
    assert_eq!(ms.session_id, sid);
    assert_eq!(ms.fp, phone.fp);
    for n in 0..5 {
        write_packet(&mut m, &pkt(n)).await.unwrap();
    }
    for n in 0..5 {
        let p = ms.next_packet().await.unwrap().unwrap();
        assert_eq!(p, pkt(n));
    }
    // Phone closes -> clean end.
    drop(m);
    assert!(ms.next_packet().await.unwrap_or(None).is_none());
}

#[tokio::test]
async fn wrong_session_id_refused() {
    let mut h = start(false).await;
    let phone = Phone::new("cam");
    let (_c, _sid) = control(&h, &phone, "dev1").await;
    let (_m, ok, err) = attach(&h, &phone, "no-such-session").await;
    assert!(!ok);
    assert!(!err.is_empty());
    assert!(h.media_rx.try_recv().is_err());
}

#[tokio::test]
async fn other_paired_cert_refused() {
    let mut h = start(false).await;
    let a = Phone::new("a");
    let b = Phone::new("b");
    h.trust.pin(&b.peer_info());
    let (_c, sid) = control(&h, &a, "devA").await;
    let (_m, ok, _) = attach(&h, &b, &sid).await;
    assert!(!ok);
    assert!(h.media_rx.try_recv().is_err());
}

#[tokio::test]
async fn newer_attach_supersedes_older() {
    let mut h = start(false).await;
    let phone = Phone::new("cam");
    let (_c, sid) = control(&h, &phone, "dev1").await;
    let (_m1, ok1, _) = attach(&h, &phone, &sid).await;
    assert!(ok1);
    let mut s1 = h.media_rx.recv().await.unwrap();
    let (mut m2, ok2, _) = attach(&h, &phone, &sid).await;
    assert!(ok2);
    let mut s2 = h.media_rx.recv().await.unwrap();
    let r = timeout(Duration::from_secs(2), s1.next_packet()).await.expect("old stream must end");
    assert!(matches!(r, Ok(None)));
    write_packet(&mut m2, &pkt(1)).await.unwrap();
    assert_eq!(s2.next_packet().await.unwrap().unwrap(), pkt(1));
}

#[tokio::test]
async fn control_end_closes_media() {
    let mut h = start(false).await;
    let phone = Phone::new("cam");
    let (c, sid) = control(&h, &phone, "dev1").await;
    let (_m, ok, _) = attach(&h, &phone, &sid).await;
    assert!(ok);
    let mut s = h.media_rx.recv().await.unwrap();
    drop(c);
    let r = timeout(Duration::from_secs(5), s.next_packet()).await.expect("media must end with control");
    assert!(matches!(r, Ok(None)));
}

#[tokio::test]
async fn camera_helpers_and_status() {
    let h = start(false).await;
    let phone = Phone::new("cam");
    let mut events = h.handle.subscribe_events();
    let (mut c, sid) = control(&h, &phone, "dev1").await;
    let session = h.handle.all_sessions().pop().unwrap();
    assert_eq!(session.session_id(), sid);
    assert!(session.last_camera_status().is_none());
    session.camera_start(CameraStart { width: 640, height: 360, fps: 30, ..Default::default() }).await.unwrap();
    assert!(matches!(expect_skip_ping(&mut c).await, Msg::CameraStart(_)));
    session.camera_stop().await.unwrap();
    assert!(matches!(expect_skip_ping(&mut c).await, Msg::CameraStop(_)));
    send(&mut c, Msg::CameraStatus(CameraStatus { state: CameraState::Streaming as i32, ..Default::default() })).await;
    let mut seen_from = false;
    let mut seen_old = false;
    while !(seen_from && seen_old) {
        match timeout(Duration::from_secs(2), events.recv()).await.unwrap().unwrap() {
            TransportEvent::CameraStatusFrom { session_id, .. } => {
                assert_eq!(session_id, sid);
                seen_from = true;
            }
            TransportEvent::CameraStatus(_) => seen_old = true,
            _ => {}
        }
    }
    assert_eq!(session.last_camera_status().unwrap().state, CameraState::Streaming as i32);
}
