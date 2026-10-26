// SPDX-License-Identifier: GPL-3.0-or-later

mod common;

use std::time::Duration;

use common::*;
use nd_proto::v1::{Control, ControlKind, GoodbyeReason, InputEvent, InputKind, Ping, SetLayout};
use nd_proto::{Msg, PROTO_VERSION};
use nd_transport::adb::{parse_devices_output, AdbDeviceState};
use nd_transport::{TransportEvent, TrustStore};

#[test]
fn test_parse_adb_devices() {
    let raw = r#"
List of devices attached
emulator-5554          device product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emu64xa transport_id:1
R58M34ABCD             device model:SM_G781B product:r8q
12345678               unauthorized transport_id:3
deadbeef               offline
"#;

    let devices = parse_devices_output(raw);
    assert_eq!(devices.len(), 4);

    assert_eq!(devices[0].serial, "emulator-5554");
    assert_eq!(devices[0].state, AdbDeviceState::Device);
    assert_eq!(devices[0].model.as_deref(), Some("sdk_gphone64_x86_64"));
    assert_eq!(devices[0].transport_id.as_deref(), Some("1"));

    assert_eq!(devices[1].serial, "R58M34ABCD");
    assert_eq!(devices[1].state, AdbDeviceState::Device);
    assert_eq!(devices[1].model.as_deref(), Some("SM_G781B"));
    assert_eq!(devices[1].product.as_deref(), Some("r8q"));

    assert_eq!(devices[2].serial, "12345678");
    assert_eq!(devices[2].state, AdbDeviceState::Unauthorized);

    assert_eq!(devices[3].serial, "deadbeef");
    assert_eq!(devices[3].state, AdbDeviceState::Offline);
}

#[tokio::test]
async fn test_control_server_handshake_layout_and_input() {
    let mut h = start(false).await;
    let phone = Phone::new("phone");
    h.trust.pin(&phone.peer_info());
    let handle = h.handle.clone();
    let mut event_rx = handle.subscribe_events();

    let mut stream = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    send(&mut stream, Msg::Hello(hello("test-phone-1"))).await;

    match expect(&mut stream).await {
        Msg::Welcome(w) => {
            assert_eq!(w.proto_version, PROTO_VERSION);
            assert!(!w.session_id.is_empty());
            assert_eq!(w.host_id, h.identity.host_id());
        }
        other => panic!("Expected Welcome, got {other:?}"),
    }

    match event_rx.recv().await.unwrap() {
        TransportEvent::SessionConnected(session) => {
            assert_eq!(session.device_id(), "test-phone-1");
            assert_eq!(session.device_model(), "Pixel 6");
            assert_eq!(session.android_sdk(), 34);
            assert_eq!(session.peer_fp(), phone.fp);
            assert_eq!(session.peer_info().device_name, "Pixel 6");
            assert_eq!(session.trust_state(), nd_transport::TrustState::Paired);
            assert_eq!(session.link(), nd_transport::Link::Usb);
            assert_eq!(session.transport(), "usb");
        }
        other => panic!("Expected SessionConnected event, got {other:?}"),
    }

    assert_eq!(handle.session_count(), 1);
    assert_eq!(handle.active_session().unwrap().device_id(), "test-phone-1");

    let layout = SetLayout {
        page_id: "main_page".into(),
        cols: 5,
        rows: 3,
        controls: vec![Control {
            id: "btn_1".into(),
            kind: ControlKind::Key as i32,
            span_w: 1,
            span_h: 1,
            title: "Mute".into(),
            ..Default::default()
        }],
    };
    handle.set_layout(layout).await.unwrap();
    match expect_skip_ping(&mut stream).await {
        Msg::SetLayout(l) => {
            assert_eq!(l.page_id, "main_page");
            assert_eq!(l.controls[0].title, "Mute");
        }
        other => panic!("Expected SetLayout, got {other:?}"),
    }

    let input = InputEvent {
        page_id: "main_page".into(),
        control_id: "btn_1".into(),
        kind: InputKind::KeyDown as i32,
        delta: 0,
        value: 0.0,
        phone_time_ms: 12345,
    };
    send(&mut stream, Msg::Input(input)).await;
    let got = tokio::time::timeout(Duration::from_secs(2), h.input_rx.recv()).await.unwrap().unwrap();
    assert_eq!(got.control_id, "btn_1");
    assert_eq!(got.kind, InputKind::KeyDown as i32);

    send(&mut stream, Msg::Ping(Ping { nonce: 98765 })).await;
    match expect_skip_ping(&mut stream).await {
        Msg::Pong(p) => assert_eq!(p.nonce, 98765),
        other => panic!("Expected Pong, got {other:?}"),
    }
}

#[tokio::test]
async fn test_control_server_rejects_version_mismatch() {
    let h = start(false).await;
    let phone = Phone::new("phone");
    h.trust.pin(&phone.peer_info());
    let mut stream = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();

    let mut hl = hello("ancient-phone");
    hl.proto_version = 999;
    send(&mut stream, Msg::Hello(hl)).await;
    match expect(&mut stream).await {
        Msg::Goodbye(g) => assert_eq!(g.reason, GoodbyeReason::VersionMismatch as i32),
        other => panic!("Expected Goodbye, got {other:?}"),
    }
}

#[tokio::test]
async fn test_older_proto_version_is_rejected_too() {
    let h = start(false).await;
    let phone = Phone::new("phone");
    h.trust.pin(&phone.peer_info());
    let mut stream = phone.connect(h.addr, None).await.unwrap();
    let mut hl = hello("v1-over-tls");
    hl.proto_version = PROTO_VERSION - 1;
    send(&mut stream, Msg::Hello(hl)).await;
    match expect(&mut stream).await {
        Msg::Goodbye(g) => assert_eq!(g.reason, GoodbyeReason::VersionMismatch as i32),
        other => panic!("Expected Goodbye, got {other:?}"),
    }
}

/// Connect a phone with the given capabilities and wait for the Welcome.
async fn connect_phone(h: &Harness, device_id: &str, caps: Vec<i32>) -> Stream {
    let phone = Phone::new(device_id);
    h.trust.pin(&phone.peer_info());
    let mut stream = phone.connect(h.addr, Some(h.host_fp)).await.unwrap();
    let mut hi = hello(device_id);
    hi.capabilities = caps;
    send(&mut stream, Msg::Hello(hi)).await;
    assert!(matches!(expect(&mut stream).await, Msg::Welcome(_)));
    stream
}

fn play(url: &str) -> nd_proto::v1::PlayStream {
    nd_proto::v1::PlayStream {
        url: url.into(),
        title: "t".into(),
        subtitle: "s".into(),
        max_height: 480,
        ..Default::default()
    }
}

#[tokio::test]
async fn test_play_stream_picks_a_capable_session() {
    let h = start(false).await;
    let handle = h.handle.clone();
    // No sessions at all.
    assert!(matches!(
        handle.play_stream(play("https://x/a.m3u8")).await,
        Err(nd_transport::TransportError::NoActiveSession)
    ));

    // A phone without the player: error.
    let plain = connect_phone(&h, "plain", vec![1, 2]).await;
    assert!(handle.play_stream(play("https://x/a.m3u8")).await.is_err());
    handle.stop_stream().await; // nothing capable: no-op, no panic

    // The active phone has the player: it gets the message.
    let mut capable = connect_phone(&h, "capable", vec![1, 4]).await;
    handle.play_stream(play("https://x/b.m3u8")).await.unwrap();
    match expect_skip_ping(&mut capable).await {
        Msg::PlayStream(p) => assert_eq!((p.url.as_str(), p.max_height), ("https://x/b.m3u8", 480)),
        other => panic!("Expected PlayStream, got {other:?}"),
    }

    // A newer, non-capable phone becomes active: the capable one is still picked.
    let mut newer = connect_phone(&h, "newer", vec![1, 2]).await;
    assert_eq!(handle.active_session().unwrap().device_id(), "newer");
    handle.play_stream(play("https://x/c.m3u8")).await.unwrap();
    match expect_skip_ping(&mut capable).await {
        Msg::PlayStream(p) => assert_eq!(p.url, "https://x/c.m3u8"),
        other => panic!("Expected PlayStream, got {other:?}"),
    }

    // stop_stream closes the player only on capable phones (an empty url).
    handle.stop_stream().await;
    match expect_skip_ping(&mut capable).await {
        Msg::PlayStream(p) => assert!(p.url.is_empty()),
        other => panic!("Expected PlayStream stop, got {other:?}"),
    }
    // The non-capable active phone saw no PlayStream: the next thing it gets is a layout we push.
    handle.set_layout(SetLayout { page_id: "p".into(), cols: 1, rows: 1, controls: vec![] }).await.unwrap();
    match expect_skip_ping(&mut newer).await {
        Msg::SetLayout(l) => assert_eq!(l.page_id, "p"),
        other => panic!("non-capable phone got {other:?}"),
    }
    drop(plain);
}
