// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(feature = "camera")]

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nd_fakephone::store::ClientIdentity;
use nd_fakephone::{FakeCamera, FakePhone, FakePhoneConfig};
use nd_proto::media::MediaPacket;
use nd_proto::v1::{CameraControl, CameraStart, CameraState, Lens};
use nd_transport::{ControlServer, HostIdentity, MediaStream, MemoryTrustStore, PeerInfo, ServerHandle, TrustStore};
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

struct Rig {
    handle: ServerHandle,
    media_rx: mpsc::Receiver<MediaStream>,
    _phone: Arc<FakePhone>,
    camera: FakeCamera,
    shutdown: watch::Sender<bool>,
    dir: std::path::PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

async fn rig() -> Rig {
    let dir = std::env::temp_dir().join(format!("nd-fakecam-test-{}", uuid::Uuid::new_v4()));
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
    Rig { handle, media_rx, _phone: phone, camera, shutdown, dir }
}

async fn next(ms: &mut MediaStream) -> MediaPacket {
    timeout(Duration::from_secs(3), ms.next_packet()).await.expect("packet in time").unwrap().expect("stream open")
}

#[tokio::test]
async fn camera_streams_on_start_and_stops() {
    let mut r = rig().await;
    let session = r.handle.all_sessions().pop().unwrap();

    // Initial status with modes.
    let t = Instant::now();
    while session.last_camera_status().is_none() {
        assert!(t.elapsed() < Duration::from_secs(2), "no CameraStatus");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let st = session.last_camera_status().unwrap();
    assert_eq!(st.state, CameraState::Stopped as i32);
    assert_eq!(st.supported_modes.len(), 6);

    session
        .camera_start(CameraStart { width: 640, height: 360, fps: 30, bitrate_bps: 1_000_000, ..Default::default() })
        .await
        .unwrap();
    let mut ms = timeout(Duration::from_secs(5), r.media_rx.recv()).await.unwrap().unwrap();
    assert_eq!(ms.session_id, session.session_id());

    let first = next(&mut ms).await;
    assert!(first.header.config, "first packet must be CONFIG");
    assert!(first.data.starts_with(&[0, 0, 0, 1]));

    let t0 = Instant::now();
    let mut key_seen = false;
    let mut count = 0;
    let mut dec = openh264::decoder::Decoder::new().unwrap();
    dec.decode(&first.data).ok();
    let mut decoded = false;
    while t0.elapsed() < Duration::from_secs(1) {
        let p = next(&mut ms).await;
        assert!(!p.header.config);
        if p.header.key_frame {
            key_seen = true;
        }
        if key_seen {
            if let Ok(Some(y)) = dec.decode(&p.data) {
                use openh264::formats::YUVSource;
                assert_eq!(y.dimensions(), (640, 360));
                decoded = true;
            }
        }
        count += 1;
    }
    assert!(key_seen, "no key frame within 1 s");
    assert!(decoded, "openh264 could not decode the stream");
    assert!(count >= 20, "only {count} packets in 1 s");

    // STREAMING status reported.
    let t = Instant::now();
    while session.last_camera_status().map(|s| s.state) != Some(CameraState::Streaming as i32) {
        assert!(t.elapsed() < Duration::from_secs(2), "no STREAMING status");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // request_keyframe -> key frame within 200 ms (drain queued frames first).
    session.camera_control(CameraControl { request_keyframe: true, ..Default::default() }).await.unwrap();
    let t = Instant::now();
    loop {
        let p = next(&mut ms).await;
        if p.header.key_frame {
            break;
        }
        assert!(t.elapsed() < Duration::from_millis(400), "no key frame after request");
    }
    // Lens and torch are applied.
    session
        .camera_control(CameraControl { lens: Some(Lens::Front as i32), torch: Some(true), ..Default::default() })
        .await
        .unwrap();
    let stats = r.camera.stats();
    let t = Instant::now();
    while !stats.torch.load(Ordering::SeqCst) {
        assert!(t.elapsed() < Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(stats.lens.load(Ordering::SeqCst), Lens::Front as i32);

    // Stop ends the stream and reports STOPPED.
    session.camera_stop().await.unwrap();
    let ended = timeout(Duration::from_secs(3), async {
        loop {
            if !matches!(ms.next_packet().await, Ok(Some(_))) {
                break;
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "stream did not end after CameraStop");
    let t = Instant::now();
    while session.last_camera_status().map(|s| s.state) != Some(CameraState::Stopped as i32) {
        assert!(t.elapsed() < Duration::from_secs(2), "no STOPPED status");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!stats.streaming.load(Ordering::SeqCst));
}
