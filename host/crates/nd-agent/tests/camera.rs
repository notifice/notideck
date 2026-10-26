// SPDX-License-Identifier: GPL-3.0-or-later
//! End to end: real ControlServer + fake phone camera + the camera manager, with an in-memory
//! shared region standing in for the Windows mapping.
#![cfg(not(windows))]

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nd_agent::camera::{CameraManager, ManagerConfig};
use nd_config::{CameraLens, CameraSettings};
use nd_core::CameraActions;
use nd_fakephone::store::ClientIdentity;
use nd_fakephone::{FakeCamera, FakePhone, FakePhoneConfig};
use nd_proto::v1::Lens;
use nd_transport::{ControlServer, HostIdentity, MemoryTrustStore, PeerInfo, TrustStore};
use nd_vcam::shm::{nv12_len, ReadResult};
use nd_vcam::{HasShm, MemShm};

struct Rig {
    manager: Arc<CameraManager>,
    shm: MemShm,
    settings: Arc<Mutex<CameraSettings>>,
    camera: FakeCamera,
    _phone: Arc<FakePhone>,
    shutdown: tokio::sync::watch::Sender<bool>,
    dir: std::path::PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.manager.shutdown();
        let _ = self.shutdown.send(true);
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

async fn rig() -> Rig {
    let dir = std::env::temp_dir().join(format!("nd-agent-cam-test-{}", uuid::Uuid::new_v4()));
    let identity = Arc::new(HostIdentity::load_or_create(&dir).unwrap());
    let trust = Arc::new(MemoryTrustStore::new(identity.fingerprint()));
    let mut server =
        ControlServer::bind_tls("127.0.0.1:0", identity.clone(), trust.clone() as Arc<dyn TrustStore>).await.unwrap();
    let addr: SocketAddr = server.local_addr();
    let handle = server.handle();
    let media_rx = server.take_media_receiver().unwrap();
    let (_task, shutdown) = server.spawn();

    let cid = Arc::new(ClientIdentity::generate("cam").unwrap());
    trust.pin(&PeerInfo {
        fp: cid.fp,
        device_id: "fake".into(),
        device_name: "Fake".into(),
        device_model: "Fake".into(),
        addr: "127.0.0.1:1".parse().unwrap(),
        link: nd_transport::Link::Usb,
    });
    let config = FakePhoneConfig {
        identity: Some(cid),
        pin: Some(identity.fingerprint()),
        require_layout: false,
        ..FakePhoneConfig::default()
    };
    let phone = Arc::new(FakePhone::connect_with_config(addr, config).await.unwrap());
    let camera = FakeCamera::attach(phone.clone()).await.unwrap();

    let shm = MemShm::new();
    let settings = Arc::new(Mutex::new(CameraSettings::default()));
    let (s1, s2, shm2) = (settings.clone(), settings.clone(), shm.clone());
    let manager = CameraManager::start(ManagerConfig {
        server: handle,
        media_rx,
        settings: Arc::new(move || s1.lock().unwrap().clone()),
        set_lens: Arc::new(move |l| s2.lock().unwrap().lens = l),
        shm_provider: Arc::new(move || Some(Arc::new(shm2.clone()) as Arc<dyn HasShm>)),
        fake_consumer: None,
        decoder_factory: None,
        manage_vcam: false,
        vcam_mem: None,
    });
    Rig { manager, shm, settings, camera, _phone: phone, shutdown, dir }
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consumer_drives_phone_camera_frames_reach_shm_and_stop_after_grace() {
    let r = rig().await;
    let stats = r.camera.stats();
    let (w, h) = (640u32, 360u32);

    // No consumer: nothing starts.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(stats.starts.load(Ordering::SeqCst), 0);
    assert_eq!(r.manager.status_json()["state"], "idle");

    // A consumer (what the DLL does) appears.
    r.shm.shm().set_consumer(true, w, h, 30);
    let mut buf = vec![0u8; nv12_len(w, h)];
    let mut last = 0u64;
    let t0 = Instant::now();
    loop {
        match r.shm.shm().read_frame(last, w, h, &mut buf) {
            ReadResult::Frame(seq) => {
                last = seq;
                break;
            }
            _ => {
                assert!(t0.elapsed() < Duration::from_secs(3), "no 640x360 frame within 3 s");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }
    assert_eq!(stats.starts.load(Ordering::SeqCst), 1);

    // Steady state: >= 20 distinct frames in a second, all exactly 640x360.
    let t = Instant::now();
    let mut frames = 0;
    while t.elapsed() < Duration::from_secs(1) {
        if let ReadResult::Frame(seq) = r.shm.shm().read_frame(last, w, h, &mut buf) {
            last = seq;
            frames += 1;
        }
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
    assert!(frames >= 20, "only {frames} frames in 1 s");
    let st = r.manager.status_json();
    assert_eq!(st["state"], "streaming", "{st}");
    assert_eq!(st["active_mode"]["width"], 640);
    wait_for("fps stats", 4, || r.manager.status_json()["stats"]["fps_out"].as_f64().unwrap_or(0.0) > 15.0).await;
    assert_eq!(st["consumer"]["width"], 640);

    // Lens switch from a deck action reaches the phone.
    assert!(r.manager.switch_lens().unwrap());
    assert_eq!(r.settings.lock().unwrap().lens, CameraLens::Front);
    wait_for("lens on phone", 3, || stats.lens.load(Ordering::SeqCst) == Lens::Front as i32).await;
    // Torch and zoom too.
    assert!(r.manager.toggle_torch().unwrap());
    wait_for("torch on phone", 3, || stats.torch.load(Ordering::SeqCst)).await;
    assert_eq!(r.manager.zoom_by(10.0).unwrap(), 8.0);

    // Consumer leaves: the phone keeps streaming for the grace period, then stops.
    r.shm.shm().set_consumer(false, w, h, 30);
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert!(stats.streaming.load(Ordering::SeqCst), "must not stop before the 5 s grace");
    let t = Instant::now();
    wait_for("CameraStop", 6, || !stats.streaming.load(Ordering::SeqCst)).await;
    assert!(t.elapsed() < Duration::from_secs(4));
    wait_for("idle", 2, || r.manager.status_json()["state"] == "idle").await;
    // Torch and zoom are forgotten with the stream.
    assert!(!r.manager.snapshot().torch);
    assert_eq!(r.manager.snapshot().zoom, 1.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_camera_streams_without_an_app_and_resizes_with_the_consumer() {
    let r = rig().await;
    let stats = r.camera.stats();
    r.manager.set_test(true);
    wait_for("test stream", 5, || stats.streaming.load(Ordering::SeqCst)).await;
    wait_for("frames", 5, || r.manager.status_json()["stats"]["fps_out"].as_f64().unwrap_or(0.0) > 10.0).await;
    let st = r.manager.status_json();
    assert_eq!(st["consumer"]["source"], "test");
    assert_eq!(st["active_mode"]["width"], 1280);
    // The preview appears once somebody asks for it.
    wait_for("preview", 5, || r.manager.preview_png().is_some_and(|p| &p[1..4] == b"PNG")).await;
    // Deck toggle turns the test stream off again; the phone stops after the grace period.
    assert!(!r.manager.toggle().unwrap());
    wait_for("stop", 8, || !stats.streaming.load(Ordering::SeqCst)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn image_settings_reach_the_phone_once_and_caps_and_readout_show_in_status() {
    use nd_config::{CameraImageSettings, FocusMode};
    let r = rig().await;
    let stats = r.camera.stats();
    r.manager.set_test(true);
    wait_for("test stream", 5, || stats.streaming.load(Ordering::SeqCst)).await;
    // CameraStart carried the (default) image settings, so no extra control was needed.
    assert_eq!(stats.image.lock().unwrap().focus_mode, nd_proto::v1::FocusMode::Continuous as i32);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(stats.image_controls.load(Ordering::SeqCst), 0, "unchanged settings must not be re-sent");

    r.settings.lock().unwrap().image =
        CameraImageSettings { focus: FocusMode::Manual, focus_permille: 500, ..Default::default() };
    wait_for("image control", 3, || stats.image_controls.load(Ordering::SeqCst) == 1).await;
    {
        let img = stats.image.lock().unwrap();
        assert_eq!((img.focus_mode, img.focus_permille), (nd_proto::v1::FocusMode::Manual as i32, 500));
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(stats.image_controls.load(Ordering::SeqCst), 1, "sent once, not every tick");

    // Status: lens caps, readout (reflects the manual focus) and the settings echo.
    wait_for("readout", 5, || r.manager.status_json()["readout"]["focus_permille"] == 500).await;
    let st = r.manager.status_json();
    let caps = st["phone"]["lens_caps"].as_array().expect("lens_caps");
    assert_eq!(caps.len(), 2, "{st}");
    assert_eq!(caps[0]["lens"], "back");
    assert_eq!(caps[0]["focus_manual"], true);
    assert_eq!(caps[0]["antibandings"], serde_json::json!(["auto", "off", "50hz", "60hz"]));
    assert_eq!(caps[0]["white_balances"][3], "warm_fluorescent");
    assert_eq!(caps[1]["lens"], "front");
    assert_eq!(caps[1]["focus_manual"], false);
    assert_eq!(st["readout"]["focused"], true);
    assert_eq!(st["image"]["focus"], "manual");
    assert_eq!(st["image"]["focus_permille"], 500);

    // A new start gets the stored settings in CameraStart itself.
    r.manager.set_test(false);
    wait_for("stop", 8, || !stats.streaming.load(Ordering::SeqCst)).await;
    assert!(r.manager.status_json()["readout"].is_null());
    *stats.image.lock().unwrap() = Default::default();
    r.manager.set_test(true);
    wait_for("restart", 5, || stats.streaming.load(Ordering::SeqCst) && stats.starts.load(Ordering::SeqCst) == 2).await;
    assert_eq!(stats.image.lock().unwrap().focus_permille, 500);
    assert_eq!(stats.image_controls.load(Ordering::SeqCst), 1);
}
