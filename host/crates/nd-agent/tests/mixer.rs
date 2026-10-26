// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-app mixer end to end: real ControlServer, a fake phone, `AgentState`, the real live and
//! meter tickers and a `MockAppAudio` (two apps, a mixer page with a slot slider and a mute key).
#![cfg(not(windows))]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nd_agent::ticks::{live_ticker, meter_ticker};
use nd_agent::AgentState;
use nd_core::mock::{AppAudioCall, MockAppAudio, MockAudio, MockInputInjector, MockLauncher};
use nd_core::{
    Action, ActionRouter, AudioApp, Control, DataSource, InputReaction, LiveProviders, Mixer, Page, Profile,
    ProfileStore, TriggerKind,
};
use nd_fakephone::store::ClientIdentity;
use nd_fakephone::{FakePhone, FakePhoneConfig};
use nd_proto::v1::UpdateControls;
use nd_proto::Msg;
use nd_transport::{ControlServer, HostIdentity, MemoryTrustStore, PeerInfo, TrustStore};
use tokio::sync::Notify;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-body";

struct Rig {
    phone: FakePhone,
    state: AgentState,
    mock: MockAppAudio,
    poke: Arc<Notify>,
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

fn slot0() -> AudioApp {
    AudioApp::Slot { index: 0 }
}

fn mixer_profile() -> Profile {
    let mut p = Profile::new("default", "Main");
    let mut page = Page::new("mix", "Mixer");
    page.add_control(
        Control::slider("s0", 0, 0, 1, 2, false)
            .with_source(DataSource::AppAudio { app: slot0() })
            .with_trigger_action(TriggerKind::SliderValue, Action::SetAppVolume { app: slot0(), level: None }),
    );
    page.add_control(
        Control::key("m0", 1, 0, "Mute")
            .with_source(DataSource::AppAudio { app: slot0() })
            .with_action(Action::ToggleAppMute { app: slot0() }),
    );
    p.add_page(page);
    p
}

async fn rig() -> Rig {
    let dir = std::env::temp_dir().join(format!("nd-agent-mixer-test-{}", uuid::Uuid::new_v4()));
    let state = AgentState::open(ProfileStore::new(dir.join("profiles"))).unwrap();
    state.put_profile(mixer_profile()).unwrap();
    state.activate_profile("default").unwrap();

    let mock = MockAppAudio::new();
    mock.set_sessions(vec![
        MockAppAudio::session("sp", "Spotify.exe", "Spotify", 0.5),
        MockAppAudio::session("ch", "chrome.exe", "Chrome", 0.8),
    ]);
    mock.set_icon("Spotify.exe", PNG);
    mock.set_peak("sp", 0.4);
    let icon_state = state.clone();
    let mixer = Arc::new(
        Mixer::new(Arc::new(mock.clone()), Arc::new(MockAudio::new()))
            .with_icons(Arc::new(move |png| icon_state.put_icon_bytes(png).ok())),
    );
    let router =
        ActionRouter::new(Arc::new(MockInputInjector::new()), Arc::new(MockLauncher::new())).with_mixer(mixer.clone());
    let providers = LiveProviders {
        audio: Arc::new(MockAudio::new()),
        media: Arc::new(nd_platform::NullMedia),
        stats: Arc::new(nd_platform::NullStats),
        camera: Arc::new(nd_core::NullCamera),
        mic: Arc::new(nd_core::NullMic),
        profile_actions: Arc::new(nd_core::autoswitch::NullProfileActions),
        mixer,
        obs: std::sync::Arc::new(nd_core::obs::NullObs),
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

    let cid = Arc::new(ClientIdentity::generate("mixer").unwrap());
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

    // The first layout, then the tickers (as app.rs does after a connect).
    let t = tokio::time::Instant::now();
    while handle.all_sessions().is_empty() {
        assert!(t.elapsed() < Duration::from_secs(5), "phone never connected");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.set_layout(state.current_layout().unwrap()).await.unwrap();
    phone.ensure_layout(Duration::from_secs(5)).await.unwrap();
    let poke = Arc::new(Notify::new());
    tasks.push(tokio::spawn(live_ticker(state.clone(), handle.clone(), providers.clone(), poke.clone())));
    tasks.push(tokio::spawn(meter_ticker(state.clone(), handle, providers)));
    Rig { phone, state, mock, poke, updates, shutdown, tasks, dir }
}

impl Rig {
    fn seen(&self) -> usize {
        self.updates.lock().unwrap().len()
    }

    /// Wait for an update patch of control `id` whose changed fields satisfy `pred`, among the
    /// updates received after the first `from`.
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
async fn slider_and_mute_drive_the_mock_backend() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let r = rig().await;
        // Let the first live tick settle so the slider shows the app's volume.
        r.patch(0, "s0", "initial live data", |f, _| has(f, "title")).await;
        let from = r.seen();

        r.phone.send_slider_value("s0", 0.3).await.unwrap();
        let c = r.patch(from, "s0", "slider value patch", |f, c| has(f, "value") && (c.value - 0.3).abs() < 1e-4).await;
        assert_eq!(c.id, "s0");
        assert!(r.mock.calls().contains(&AppAudioCall::Volume("sp".into(), 0.3)), "{:?}", r.mock.calls());
        assert!((r.mock.get("sp").unwrap().volume - 0.3).abs() < 1e-4);
        // Only slot 0 (Spotify) moved.
        assert!((r.mock.get("ch").unwrap().volume - 0.8).abs() < 1e-4);

        let from = r.seen();
        r.phone.simulate_tap("m0").await.unwrap();
        let c = r.patch(from, "m0", "mute state patch", |f, _| has(f, "state_index")).await;
        assert_eq!(c.state_index, 1);
        assert!(r.mock.get("sp").unwrap().muted);
        // nd-core's mixer serves toggles from a snapshot up to CACHE_MAX_AGE old (500 ms); a second tap
        // inside that window would mute again, so wait it out.
        tokio::time::sleep(nd_core::mixer::CACHE_MAX_AGE + Duration::from_millis(100)).await;
        let from = r.seen();
        r.phone.simulate_tap("m0").await.unwrap();
        let c = r.patch(from, "m0", "unmute state patch", |f, _| has(f, "state_index")).await;
        assert_eq!(c.state_index, 0);
        assert!(!r.mock.get("sp").unwrap().muted);
    })
    .await
    .expect("hung");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_tick_patches_title_icon_value_and_dims_when_the_app_leaves() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let r = rig().await;
        let c = r
            .patch(0, "s0", "slot data", |f, _| {
                has(f, "title") && has(f, "icon_hash") && has(f, "value") && has(f, "meter")
            })
            .await;
        assert_eq!(c.title, "Spotify");
        assert!((c.value - 0.5).abs() < 1e-4);
        assert!(c.meter);
        // The icon went through the agent's icon store.
        assert_eq!(c.icon_hash.len(), 64);
        assert!(r.state.get_icon(&c.icon_hash).is_some());
        assert_eq!(r.mock.icon_calls(), 1);

        // The volume changes elsewhere (e.g. in the Windows mixer): the next tick patches it.
        let from = r.seen();
        let mut s = r.mock.get("sp").unwrap();
        s.volume = 0.9;
        r.mock.set_sessions(vec![s, MockAppAudio::session("ch", "chrome.exe", "Chrome", 0.8)]);
        r.poke.notify_one();
        let c = r.patch(from, "s0", "external volume change", |f, _| has(f, "value")).await;
        assert!((c.value - 0.9).abs() < 1e-4);

        // Spotify quits: slot 0 becomes Chrome (title/icon change); then nothing is left but Chrome.
        let from = r.seen();
        r.mock.set_sessions(vec![MockAppAudio::session("ch", "chrome.exe", "Chrome", 0.8)]);
        r.poke.notify_one();
        let c = r.patch(from, "s0", "slot reassigned", |f, _| has(f, "title")).await;
        assert_eq!(c.title, "Chrome");
        let from = r.seen();
        r.mock.set_sessions(vec![]);
        r.poke.notify_one();
        let c = r.patch(from, "s0", "dimmed", |f, _| has(f, "dimmed")).await;
        assert!(c.dimmed && c.title.is_empty());
    })
    .await
    .expect("hung");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn meter_tick_sends_only_level_patches_and_nothing_when_unchanged() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let r = rig().await;
        let c = r.patch(0, "s0", "first level", |f, c| f == ["level"] && c.level > 0.0).await;
        assert!((c.level - 0.4).abs() < 0.021, "level {}", c.level);
        // Every level update carries nothing but the "level" field.
        for u in r.updates.lock().unwrap().iter() {
            for p in &u.patches {
                if has(&p.fields, "level") {
                    assert_eq!(p.fields, ["level"], "{p:?}");
                }
            }
        }

        // Steady peaks: no patches at all (live data is change-only too).
        tokio::time::sleep(Duration::from_millis(300)).await;
        let from = r.seen();
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(r.seen(), from, "unchanged levels must not be re-sent: {:?}", r.updates);

        // A new peak is one level patch; going silent zeroes the level.
        r.mock.set_peak("sp", 0.8);
        let c = r.patch(from, "s0", "louder", |f, c| f == ["level"] && c.level > 0.7).await;
        assert!((c.level - 0.8).abs() < 0.021);
        let from = r.seen();
        r.mock.set_peak("sp", 0.0);
        let c = r.patch(from, "s0", "silent", |f, c| f == ["level"] && c.level == 0.0).await;
        assert_eq!(c.id, "s0");
    })
    .await
    .expect("hung");
}
