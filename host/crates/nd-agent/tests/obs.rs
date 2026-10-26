// SPDX-License-Identifier: GPL-3.0-or-later
//! OBS end to end: real ControlServer, a fake phone, `AgentState`, the real live ticker, an
//! `ObsClient` connected to an in-process `FakeObs`.
#![cfg(not(windows))]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nd_agent::ticks::live_ticker;
use nd_agent::AgentState;
use nd_core::mock::{MockAppAudio, MockAudio, MockInputInjector, MockLauncher};
use nd_core::obs::{ObsInput, ObsScene, ObsToggle};
use nd_core::{
    Action, ActionRouter, Control, DataSource, InputReaction, LiveProviders, Mixer, Page, Profile, ProfileStore,
};
use nd_fakephone::store::ClientIdentity;
use nd_fakephone::{FakePhone, FakePhoneConfig};
use nd_obs::{FakeObs, ObsClient, ObsSettings};
use nd_proto::v1::UpdateControls;
use nd_proto::Msg;
use nd_transport::{ControlServer, HostIdentity, MemoryTrustStore, PeerInfo, TrustStore};
use tokio::sync::Notify;

struct Rig {
    phone: FakePhone,
    fake: FakeObs,
    updates: Arc<Mutex<Vec<UpdateControls>>>,
    shutdown: tokio::sync::watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    dir: std::path::PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        let _ = self.shutdown.send(true);
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn profile() -> Profile {
    let mut p = Profile::new("default", "Main");
    let mut page = Page::new("obs", "OBS");
    page.add_control(
        Control::key("rec", 0, 0, "Record")
            .with_source(DataSource::ObsRecord)
            .with_action(Action::ObsRecord { mode: ObsToggle::Toggle }),
    );
    page.add_control(
        Control::key("sc1", 1, 0, "Scene")
            .with_source(DataSource::ObsScene { scene: ObsScene::Slot { index: 1 } })
            .with_action(Action::ObsScene { scene: ObsScene::Slot { index: 1 }, preview: false }),
    );
    let _ = ObsInput::Name { name: String::new() };
    p.add_page(page);
    p
}

async fn rig() -> Rig {
    let dir = std::env::temp_dir().join(format!("nd-agent-obs-test-{}", uuid::Uuid::new_v4()));
    let state = AgentState::open(ProfileStore::new(dir.join("profiles"))).unwrap();
    state.put_profile(profile()).unwrap();
    state.activate_profile("default").unwrap();

    let fake = FakeObs::start(0, None).await.unwrap();
    let poke = Arc::new(Notify::new());
    let p2 = poke.clone();
    let obs = Arc::new(ObsClient::start(
        ObsSettings {
            enabled: true,
            auto_detect: false,
            host: "127.0.0.1".into(),
            port: fake.addr().port(),
            password: String::new(),
        },
        Arc::new(move || p2.notify_one()),
    ));
    let audio = Arc::new(MockAudio::new());
    let router =
        ActionRouter::new(Arc::new(MockInputInjector::new()), Arc::new(MockLauncher::new())).with_obs(obs.clone());
    let providers = LiveProviders {
        audio: audio.clone(),
        media: Arc::new(nd_platform::NullMedia),
        stats: Arc::new(nd_platform::NullStats),
        camera: Arc::new(nd_core::NullCamera),
        mic: Arc::new(nd_core::NullMic),
        profile_actions: Arc::new(nd_core::autoswitch::NullProfileActions),
        mixer: Arc::new(Mixer::new(Arc::new(MockAppAudio::new()), audio)),
        obs: obs.clone(),
        icons: None,
    };

    let identity = Arc::new(HostIdentity::load_or_create(&dir).unwrap());
    let trust = Arc::new(MemoryTrustStore::new(identity.fingerprint()));
    let mut server =
        ControlServer::bind_tls("127.0.0.1:0", identity.clone(), trust.clone() as Arc<dyn TrustStore>).await.unwrap();
    let addr: SocketAddr = server.local_addr();
    let handle = server.handle();
    let mut input_rx = server.take_input_receiver().unwrap();
    let (_task, shutdown) = server.spawn();

    let mut tasks = Vec::new();
    let (st, h, r) = (state.clone(), handle.clone(), router.clone());
    tasks.push(tokio::spawn(async move {
        while let Some(input) = input_rx.recv().await {
            let (st, r, inp) = (st.clone(), r.clone(), input.clone());
            let res = tokio::task::spawn_blocking(move || st.handle_input(&inp, &r)).await.unwrap();
            if let Ok(InputReaction::UpdateControls(u)) = res {
                let _ = h.update_controls(u).await;
            }
        }
    }));

    let cid = Arc::new(ClientIdentity::generate("obs").unwrap());
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
    let phone = FakePhone::connect_with_config(addr, config).await.unwrap();
    let updates: Arc<Mutex<Vec<UpdateControls>>> = Arc::default();
    let (mut rx, sink) = (phone.subscribe(), updates.clone());
    tasks.push(tokio::spawn(async move {
        while let Ok(env) = rx.recv().await {
            if let Some(Msg::UpdateControls(u)) = env.msg {
                sink.lock().unwrap().push(u);
            }
        }
    }));

    let t = tokio::time::Instant::now();
    while handle.all_sessions().is_empty() {
        assert!(t.elapsed() < Duration::from_secs(5), "phone never connected");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.set_layout(state.current_layout().unwrap()).await.unwrap();
    phone.ensure_layout(Duration::from_secs(5)).await.unwrap();
    tasks.push(tokio::spawn(live_ticker(state.clone(), handle, providers, poke)));
    Rig { phone, fake, updates, shutdown, tasks, dir }
}

impl Rig {
    async fn patch(
        &self,
        from: usize,
        id: &str,
        what: &str,
        pred: impl Fn(&[String], &nd_proto::v1::Control) -> bool,
    ) -> nd_proto::v1::Control {
        let t = tokio::time::Instant::now();
        loop {
            {
                let all = self.updates.lock().unwrap();
                for u in all.iter().skip(from) {
                    for p in u.patches.iter().filter(|p| p.id == id) {
                        if let Some(c) = &p.control {
                            if pred(&p.fields, c) {
                                return c.clone();
                            }
                        }
                    }
                }
            }
            assert!(t.elapsed() < Duration::from_secs(5), "timed out waiting for {what}; saw {:?}", self.updates);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn has(fields: &[String], f: &str) -> bool {
    fields.iter().any(|x| x == f)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn record_key_starts_recording_and_lights_up_and_scene_slot_shows_its_name() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let r = rig().await;
        // Slot 1 of the demo scenes is "BRB" once the client has connected and the live tick ran.
        let c = r.patch(0, "sc1", "scene slot title", |f, c| has(f, "title") && c.title == "BRB").await;
        assert_eq!(c.title, "BRB");

        let from = r.updates.lock().unwrap().len();
        r.phone.simulate_tap("rec").await.unwrap();
        let c = r.patch(from, "rec", "record state", |f, c| has(f, "state_index") && c.state_index == 1).await;
        assert_eq!(c.state_index, 1);
        assert!(r.fake.requests().iter().any(|q| q == "StartRecord"), "{:?}", r.fake.requests());
        assert!(r.fake.state().record.active);
    })
    .await
    .expect("hung");
}
