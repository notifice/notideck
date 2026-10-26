// SPDX-License-Identifier: GPL-3.0-or-later
//! Client vs FakeObs, and detect on temp dirs.

use crate::detect;
use crate::{FakeObs, ObsClient, ObsSettings};
use nd_core::obs::{ObsCommand, ObsControl, ObsSnapshot, ObsStatus, SpecialInput};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn settings(port: u16, password: &str) -> ObsSettings {
    ObsSettings { enabled: true, auto_detect: false, host: "127.0.0.1".into(), port, password: password.into() }
}

fn start_client(port: u16, password: &str) -> (ObsClient, Arc<AtomicUsize>) {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let c = ObsClient::start(
        settings(port, password),
        Arc::new(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        }),
    );
    (c, n)
}

async fn wait_for(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(tokio::time::Instant::now() < end, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn connected(c: &ObsClient) {
    wait_for("connected", 5, || c.snapshot().status == ObsStatus::Connected).await;
}

/// Snapshot without durations (they depend on the clock).
fn norm(mut s: ObsSnapshot) -> ObsSnapshot {
    for o in [&mut s.stream, &mut s.record, &mut s.replay_buffer, &mut s.virtual_cam] {
        o.duration_ms = 0;
    }
    s
}

/// The client snapshot becomes equal to the fake's state.
async fn synced(c: &ObsClient, f: &FakeObs) {
    wait_for("client to match fake", 3, || norm(c.snapshot()) == norm(f.state())).await;
}

#[tokio::test]
async fn connects_without_password_and_matches_initial_state() {
    let fake = FakeObs::start(0, None).await.unwrap();
    let (c, _) = start_client(fake.addr().port(), "");
    connected(&c).await;
    let s = c.snapshot();
    assert_eq!(norm(s.clone()), norm(fake.state()));
    assert_eq!(s.scenes, ["Main", "BRB", "Starting Soon"]);
    assert_eq!(s.program_scene.as_deref(), Some("Main"));
    assert_eq!(s.special_inputs[&SpecialInput::Mic1], "Mic/Aux");
    assert_eq!(s.special_inputs[&SpecialInput::Desktop1], "Desktop Audio");
    assert!(!s.special_inputs.contains_key(&SpecialInput::Mic2));
    let webcam = s.inputs.iter().find(|i| i.name == "Webcam").unwrap();
    assert_eq!((webcam.muted, webcam.volume_mul), (None, None));
    let items: Vec<_> = s.scene_items["Main"].iter().map(|i| i.source.as_str()).collect();
    assert_eq!(items, ["Webcam", "Display Capture"]);
    let info = c.info();
    assert_eq!(info.status, ObsStatus::Connected);
    assert_eq!(info.obs_version.as_deref(), Some("30.2.0-fake"));
    assert_eq!(info.endpoint, format!("127.0.0.1:{}", fake.addr().port()));
}

#[tokio::test]
async fn password_auth_ok_and_wrong() {
    let fake = FakeObs::start(0, Some("secret")).await.unwrap();
    let port = fake.addr().port();
    let (good, _) = start_client(port, "secret");
    connected(&good).await;
    assert_eq!(norm(good.snapshot()), norm(fake.state()));

    let (bad, _) = start_client(port, "nope");
    wait_for("auth failed", 5, || bad.snapshot().status == ObsStatus::AuthFailed).await;
    assert_eq!(bad.info().last_error.as_deref(), Some("authentication failed"));
    assert!(bad.send(ObsCommand::SaveReplayBuffer).is_err());
    // Fixing the password reconnects at once.
    bad.update_settings(settings(port, "secret"));
    connected(&bad).await;
    assert!(bad.info().last_error.is_none());
}

#[tokio::test]
async fn commands_change_fake_and_snapshot_follows() {
    let fake = FakeObs::start(0, None).await.unwrap();
    let (c, _) = start_client(fake.addr().port(), "");
    connected(&c).await;
    let cmds = [
        ObsCommand::SetProgramScene("BRB".into()),
        ObsCommand::SetStudioMode(true),
        ObsCommand::SetPreviewScene("Starting Soon".into()),
        ObsCommand::StudioTransition,
        ObsCommand::SetStudioMode(false),
        ObsCommand::Record(true),
        ObsCommand::RecordPause(true),
        ObsCommand::RecordPause(false),
        ObsCommand::ReplayBuffer(true),
        ObsCommand::SaveReplayBuffer,
        ObsCommand::VirtualCam(true),
        ObsCommand::Stream(true),
        ObsCommand::SetInputMute { input: "Mic/Aux".into(), muted: true },
        ObsCommand::SetInputVolumeMul { input: "Desktop Audio".into(), mul: 0.125 },
        ObsCommand::SetSceneItemEnabled { scene: "Main".into(), item_id: 1, enabled: false },
    ];
    for cmd in cmds {
        let n = fake.requests().len();
        c.send(cmd).unwrap();
        // Requests are answered in order, so wait for the fake to have seen it, then for the events.
        wait_for("request", 3, || fake.requests().len() > n).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        synced(&c, &fake).await;
    }
    let st = fake.state();
    assert_eq!(st.program_scene.as_deref(), Some("Starting Soon"));
    assert!(!st.studio_mode && st.preview_scene.is_none());
    assert!(st.record.active && !st.record.paused);
    assert!(st.replay_buffer.active && st.virtual_cam.active && st.stream.active);
    assert_eq!(st.inputs[0].muted, Some(true));
    assert_eq!(st.inputs[1].volume_mul, Some(0.125));
    assert!(!st.scene_items["Main"][0].enabled);
    let reqs = fake.requests();
    for r in
        ["SetCurrentProgramScene", "TriggerStudioModeTransition", "SaveReplayBuffer", "PauseRecord", "ResumeRecord"]
    {
        assert!(reqs.iter().any(|x| x == r), "missing {r} in {reqs:?}");
    }
    // Stop everything again.
    for cmd in [
        ObsCommand::Stream(false),
        ObsCommand::Record(false),
        ObsCommand::ReplayBuffer(false),
        ObsCommand::VirtualCam(false),
    ] {
        c.send(cmd).unwrap();
    }
    wait_for("all stopped", 3, || {
        let s = c.snapshot();
        !s.stream.active && !s.record.active && !s.replay_buffer.active && !s.virtual_cam.active
    })
    .await;
    synced(&c, &fake).await;
}

#[tokio::test]
async fn obs_side_changes_reach_the_snapshot_and_on_change() {
    let fake = FakeObs::start(0, None).await.unwrap();
    let (c, changes) = start_client(fake.addr().port(), "");
    connected(&c).await;
    let before = changes.load(Ordering::SeqCst);
    assert!(before > 0);
    fake.mutate(|s| {
        s.program_scene = Some("BRB".into());
        s.record.active = true;
        s.inputs[1].muted = Some(true);
        s.scene_items.get_mut("BRB").unwrap()[0].enabled = false;
    });
    synced(&c, &fake).await;
    assert!(changes.load(Ordering::SeqCst) > before);
    assert!(c.snapshot().record.active);

    // Scene list changes (new scene on top, reorder) keep the OBS UI order.
    fake.mutate(|s| {
        s.scenes = vec!["Intro".into(), "Starting Soon".into(), "Main".into(), "BRB".into()];
        s.scene_items.insert("Intro".into(), vec![]);
    });
    synced(&c, &fake).await;
    assert_eq!(c.snapshot().scenes[0], "Intro");

    // Studio mode with a preview scene.
    fake.mutate(|s| {
        s.studio_mode = true;
        s.preview_scene = Some("Main".into());
    });
    synced(&c, &fake).await;
    assert_eq!(c.snapshot().preview_scene.as_deref(), Some("Main"));
}

#[tokio::test]
async fn shutdown_goes_offline_and_reconnects_to_new_fake() {
    let fake = FakeObs::start(0, None).await.unwrap();
    let port = fake.addr().port();
    let (c, _) = start_client(port, "");
    connected(&c).await;
    fake.shutdown().await;
    wait_for("offline", 3, || c.snapshot().status == ObsStatus::Offline).await;
    assert!(c.snapshot().scenes.is_empty());
    assert!(c.send(ObsCommand::Stream(true)).is_err());
    let fake2 = FakeObs::start(port, None).await.unwrap();
    connected(&c).await;
    assert_eq!(norm(c.snapshot()), norm(fake2.state()));
}

#[tokio::test]
async fn offline_when_nothing_listens_and_disabled() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let (c, _) = start_client(port, "");
    wait_for("offline", 3, || c.snapshot().status == ObsStatus::Offline).await;
    // Usually "connection refused"; under a parallel test run another test can grab the freed port, which gives
    // a different connection error. Offline + some error is what matters.
    assert!(c.info().last_error.is_some());
    assert_eq!(c.send(ObsCommand::Record(true)), Err("OBS is not connected".to_string()));
    let mut s = c.settings();
    s.enabled = false;
    c.update_settings(s);
    wait_for("disabled", 3, || c.snapshot().status == ObsStatus::Disabled).await;
}

#[tokio::test]
async fn durations_advance_and_freeze_while_paused() {
    let fake = FakeObs::start(0, None).await.unwrap();
    let (c, _) = start_client(fake.addr().port(), "");
    connected(&c).await;
    c.send(ObsCommand::Record(true)).unwrap();
    wait_for("recording", 3, || c.snapshot().record.active).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let d1 = c.snapshot().record.duration_ms;
    assert!((250..1500).contains(&d1), "d1 = {d1}");
    c.send(ObsCommand::RecordPause(true)).unwrap();
    wait_for("paused", 3, || c.snapshot().record.paused).await;
    let frozen = c.snapshot().record.duration_ms;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(c.snapshot().record.duration_ms, frozen);
    assert!(frozen >= d1);
    c.send(ObsCommand::RecordPause(false)).unwrap();
    wait_for("resumed", 3, || !c.snapshot().record.paused).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(c.snapshot().record.duration_ms >= frozen + 150);
    assert_eq!(c.snapshot().stream.duration_ms, 0);
    // A client connecting mid-recording picks up the running duration.
    let (c2, _) = start_client(fake.addr().port(), "");
    connected(&c2).await;
    assert!(c2.snapshot().record.duration_ms >= frozen);
    c.send(ObsCommand::Record(false)).unwrap();
    wait_for("stopped", 3, || c.snapshot().record.duration_ms == 0 && !c.snapshot().record.active).await;
}

// ---- detect ----

fn temp_dir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "nd-obs-test-{}-{name}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn detect_missing_file_gives_defaults() {
    let d = temp_dir("missing");
    let cfg = detect::read_from(&d.join("config.json")).unwrap();
    assert!(!cfg.exists && !cfg.server_enabled && cfg.auth_required);
    assert_eq!((cfg.server_port, cfg.server_password.as_str()), (4455, ""));
}

#[test]
fn detect_reads_file_and_rejects_garbage() {
    let d = temp_dir("read");
    let p = d.join("config.json");
    std::fs::write(&p, r#"{"server_enabled":true,"server_port":4466,"auth_required":false,"server_password":"pw"}"#)
        .unwrap();
    let cfg = detect::read_from(&p).unwrap();
    assert!(cfg.exists && cfg.server_enabled && !cfg.auth_required);
    assert_eq!((cfg.server_port, cfg.server_password.as_str()), (4466, "pw"));
    std::fs::write(&p, "not json").unwrap();
    assert!(detect::read_from(&p).is_none());
    assert!(detect::enable_server_at(&p).is_err());
}

#[test]
fn detect_enable_creates_file_with_password() {
    let d = temp_dir("create");
    let p = d.join("plugin_config").join("obs-websocket").join("config.json");
    let cfg = detect::enable_server_at(&p).unwrap();
    assert!(cfg.exists && cfg.server_enabled && cfg.auth_required);
    assert_eq!(cfg.server_port, 4455);
    assert_eq!(cfg.server_password.len(), 16);
    assert!(cfg.server_password.chars().all(|c| c.is_ascii_alphanumeric()));
    let names: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 1, "temp file left behind: {names:?}");
}

#[test]
fn detect_enable_keeps_unknown_keys_and_existing_password() {
    let d = temp_dir("keep");
    let p = d.join("config.json");
    std::fs::write(
        &p,
        r#"{"alerts_enabled":true,"first_load":true,"server_enabled":false,"server_port":5555,"server_password":"keepme","extra":{"a":[1,2]}}"#,
    )
    .unwrap();
    let cfg = detect::enable_server_at(&p).unwrap();
    assert_eq!((cfg.server_port, cfg.server_password.as_str()), (5555, "keepme"));
    assert!(cfg.server_enabled && cfg.auth_required);
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["extra"]["a"][1], 2);
    assert_eq!(v["alerts_enabled"], true);
    assert_eq!(v["first_load"], false);
    // Empty password gets generated, and a second enable keeps it.
    std::fs::write(&p, r#"{"server_password":""}"#).unwrap();
    let a = detect::enable_server_at(&p).unwrap();
    assert_eq!(a.server_password.len(), 16);
    assert_eq!(detect::enable_server_at(&p).unwrap().server_password, a.server_password);
}
