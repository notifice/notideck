// SPDX-License-Identifier: GPL-3.0-or-later

use nd_fakephone::{FakePhone, FakePhoneConfig, MockAction, MockHost};
use nd_proto::v1::{Control, ControlPatch, DeviceHealth, Feedback, FeedbackKind, InputKind, UpdateControls};
use std::time::Duration;

#[tokio::test]
async fn test_handshake_and_button_tap() {
    // 1. Run the mock transport server / core router
    let host = MockHost::bind_ephemeral().await.expect("bind mock host");
    host.register_action("btn-mute", MockAction::Hotkey("Ctrl+Shift+M".to_string())).await;

    // 2. Connect nd-fakephone
    let fake = FakePhone::connect(host.local_addr()).await.expect("connect fake phone");

    // 3. Verifies Hello -> Welcome -> SetLayout exchange
    let welcome = fake.welcome().await.expect("welcome received");
    assert_eq!(welcome.proto_version, nd_proto::PROTO_VERSION);
    assert!(!welcome.session_id.is_empty());
    assert_eq!(welcome.host_name, "MockNotideckHost");

    let layout = fake.latest_layout().await.expect("layout received");
    assert_eq!(layout.page_id, "main");
    assert_eq!(layout.cols, 5);
    assert_eq!(layout.rows, 3);
    assert!(layout.controls.iter().any(|c| c.id == "btn-mute"));

    // 4. Simulates a button tap and verifies the action was received/triggered
    fake.simulate_tap("btn-mute").await.expect("simulate tap");

    let action = host.wait_for_action("btn-mute", Duration::from_secs(2)).await.expect("action triggered");

    assert_eq!(action.control_id, "btn-mute");
    assert_eq!(action.action, MockAction::Hotkey("Ctrl+Shift+M".to_string()));

    // Verify both KEY_DOWN and KEY_UP arrived at the mock host
    host.wait_for_input_matching(Duration::from_secs(2), |i| {
        i.control_id == "btn-mute" && i.kind == InputKind::KeyUp as i32
    })
    .await
    .expect("key up received on host");

    let inputs = host.received_inputs().await;
    let down_count =
        inputs.iter().filter(|i| i.control_id == "btn-mute" && i.kind == InputKind::KeyDown as i32).count();
    let up_count = inputs.iter().filter(|i| i.control_id == "btn-mute" && i.kind == InputKind::KeyUp as i32).count();

    assert!(down_count >= 1, "expected at least 1 KEY_DOWN");
    assert!(up_count >= 1, "expected at least 1 KEY_UP");
}

#[tokio::test]
async fn test_ping_pong_response() {
    let host = MockHost::bind_ephemeral().await.expect("bind mock host");
    let fake = FakePhone::connect(host.local_addr()).await.expect("connect fake phone");

    // Host sends ping to fake phone
    let nonce = 0xdeadbeef12345678;
    host.send_ping(nonce).await.expect("send ping");

    // Host receives pong from fake phone
    let pong = host.wait_for_pong(nonce, Duration::from_secs(2)).await.expect("wait for pong");
    assert_eq!(pong.nonce, nonce);

    // Verify phone counters
    // Give a brief moment for reader task state to update
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(fake.pings_received_count().await >= 1);
    assert!(fake.pongs_sent_count().await >= 1);
}

#[tokio::test]
async fn test_update_controls_propagation() {
    let host = MockHost::bind_ephemeral().await.expect("bind mock host");
    let fake = FakePhone::connect(host.local_addr()).await.expect("connect fake phone");

    // Host pushes UpdateControls patch
    let update = UpdateControls {
        page_id: "main".to_string(),
        patches: vec![ControlPatch {
            id: "btn-mute".to_string(),
            fields: vec!["title".to_string(), "state_index".to_string()],
            control: Some(Control {
                id: "btn-mute".to_string(),
                title: "MUTED".to_string(),
                state_index: 1,
                ..Default::default()
            }),
        }],
    };

    host.send_update_controls(update.clone()).await.expect("send update controls");

    let received = fake.wait_for_update(Duration::from_secs(2)).await.expect("fake phone wait for update");

    assert_eq!(received.page_id, "main");
    assert_eq!(received.patches.len(), 1);
    assert_eq!(received.patches[0].id, "btn-mute");
    assert_eq!(received.patches[0].control.as_ref().map(|c| c.title.as_str()), Some("MUTED"));

    // Check latest_update_controls inspection helper
    let latest = fake.latest_update_controls().await.expect("latest update controls");
    assert_eq!(latest.patches[0].id, "btn-mute");
}

#[tokio::test]
async fn test_dial_and_slider_inputs() {
    let host = MockHost::bind_ephemeral().await.expect("bind mock host");
    let fake = FakePhone::connect(host.local_addr()).await.expect("connect fake phone");

    // Simulate dial rotation
    fake.send_dial_rotate("dial-vol", 4).await.expect("send dial rotate");

    // Simulate slider adjustment
    fake.send_slider_value("slider-mix", 0.85).await.expect("send slider value");

    // Wait a brief moment for frames to arrive
    tokio::time::sleep(Duration::from_millis(50)).await;

    let inputs = host.received_inputs().await;
    let dial_event = inputs
        .iter()
        .find(|i| i.control_id == "dial-vol" && i.kind == InputKind::DialRotate as i32)
        .expect("dial event received");
    assert_eq!(dial_event.delta, 4);

    let slider_event = inputs
        .iter()
        .find(|i| i.control_id == "slider-mix" && i.kind == InputKind::SliderValue as i32)
        .expect("slider event received");
    assert!((slider_event.value - 0.85).abs() < 1e-4);
}

#[tokio::test]
async fn test_feedback_reception() {
    let host = MockHost::bind_ephemeral().await.expect("bind mock host");
    let fake = FakePhone::connect(host.local_addr()).await.expect("connect fake phone");

    let feedback =
        Feedback { kind: FeedbackKind::Haptic as i32, control_id: "btn-mute".to_string(), text: "".to_string() };
    host.send_feedback(feedback).await.expect("send feedback");

    let received = fake.wait_for_feedback(Duration::from_secs(2)).await.expect("wait for feedback");
    assert_eq!(received.kind, FeedbackKind::Haptic as i32);
    assert_eq!(received.control_id, "btn-mute");
}

#[tokio::test]
async fn test_device_health_reporting() {
    let host = MockHost::bind_ephemeral().await.expect("bind mock host");
    let fake = FakePhone::connect(host.local_addr()).await.expect("connect fake phone");

    let health = DeviceHealth { battery_percent: 78, battery_temp_c: 34.2, charging: true, thermal_throttled: false };
    fake.send_health(health).await.expect("send health");

    let received = host.wait_for_health(Duration::from_secs(2)).await.expect("wait for health on host");

    assert_eq!(received.battery_percent, 78);
    assert!((received.battery_temp_c - 34.2).abs() < 1e-3);
    assert!(received.charging);
    assert!(!received.thermal_throttled);
}

#[tokio::test]
async fn test_multiple_action_routing() {
    let host = MockHost::bind_ephemeral().await.expect("bind mock host");
    host.register_action("btn-mute", MockAction::Hotkey("Mute".to_string())).await;
    host.register_action("btn-cam", MockAction::Launch("obs".to_string())).await;

    let fake = FakePhone::connect(host.local_addr()).await.expect("connect fake phone");

    fake.simulate_tap("btn-mute").await.expect("tap mute");
    fake.simulate_tap("btn-cam").await.expect("tap cam");

    let act1 = host.wait_for_action("btn-mute", Duration::from_secs(2)).await.expect("mute action");
    assert_eq!(act1.action, MockAction::Hotkey("Mute".to_string()));

    let act2 = host.wait_for_action("btn-cam", Duration::from_secs(2)).await.expect("cam action");
    assert_eq!(act2.action, MockAction::Launch("obs".to_string()));
}

#[tokio::test]
async fn test_custom_config_handshake() {
    let host = MockHost::bind_ephemeral().await.expect("bind mock host");
    let config = FakePhoneConfig {
        device_id: "custom-phone-99".to_string(),
        device_model: "Custom Samsung Galaxy S23".to_string(),
        ..Default::default()
    };

    let fake = FakePhone::connect_with_config(host.local_addr(), config).await.expect("connect with custom config");

    assert_eq!(fake.config().device_id, "custom-phone-99");
    assert_eq!(fake.config().device_model, "Custom Samsung Galaxy S23");
}
