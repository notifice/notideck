// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugins end to end (round 19): real ControlServer, a fake phone, `AgentState`, the real live ticker and a
//! real `PluginHost` running the staged `dev.notideck.clock` plugin (skipped when it is not staged:
//! `scripts/stage-plugins.sh host/target/plugins-staged`).
#![cfg(not(windows))]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nd_agent::ticks::live_ticker_with;
use nd_agent::AgentState;
use nd_core::mock::{MockAppAudio, MockAudio, MockLauncher};
use nd_core::plugin::{plugin_context, PluginControl, PluginSettings};
use nd_core::{Action, Control, LiveProviders, Mixer, Page, Profile, ProfileStore};
use nd_fakephone::store::ClientIdentity;
use nd_fakephone::{FakePhone, FakePhoneConfig};
use nd_plugin::{HostConfig, HostEvent, PluginHost};
use nd_proto::v1::{FeedbackKind, UpdateControls};
use nd_proto::Msg;
use nd_transport::{ControlServer, HostIdentity, MemoryTrustStore, PeerInfo, TrustStore};
use tokio::sync::{mpsc, Notify};

const CLOCK: &str = "dev.notideck.clock";

fn staged_dir() -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/plugins-staged");
    dir.join(CLOCK).join("manifest.json").is_file().then_some(dir)
}

struct Rig {
    phone: FakePhone,
    state: AgentState,
    events: mpsc::UnboundedSender<HostEvent>,
    host: Arc<PluginHost>,
    updates: Arc<Mutex<Vec<UpdateControls>>>,
    feedback: Arc<Mutex<Vec<nd_proto::v1::Feedback>>>,
    layouts: Arc<Mutex<usize>>,
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
        // `shutdown` blocks on a condvar: never run it on a runtime worker.
        let host = self.host.clone();
        std::thread::spawn(move || host.shutdown()).join().ok();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn profile() -> Profile {
    let mut p = Profile::new("default", "Main");
    let mut page = Page::new("p1", "Plugins");
    let mut settings = PluginSettings::new();
    settings.insert("format".into(), "24h".into());
    page.add_control(Control::key("clk", 0, 0, "Clock").with_action(Action::Plugin {
        plugin: CLOCK.into(),
        action: "clock".into(),
        settings,
    }));
    p.add_page(page);
    p
}

async fn rig(staged: std::path::PathBuf) -> Rig {
    let dir = std::env::temp_dir().join(format!("nd-agent-plugin-test-{}", uuid::Uuid::new_v4()));
    let state = AgentState::open(ProfileStore::new(dir.join("profiles"))).unwrap();
    state.put_profile(profile()).unwrap();
    state.activate_profile("default").unwrap();

    let poke = Arc::new(Notify::new());
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let p2 = poke.clone();
    let icon_state = state.clone();
    let host = PluginHost::start(HostConfig {
        bundled_dirs: vec![staged],
        user_dir: dir.join("plugins"),
        state_file: dir.join("plugins.json"),
        icons: Arc::new(move |png| icon_state.put_icon_bytes(png).ok()),
        poke: Arc::new(move || p2.notify_one()),
        events: ev_tx.clone(),
        agent_version: "test".into(),
    })
    .unwrap();

    let audio = Arc::new(MockAudio::new());
    let providers = LiveProviders {
        audio: audio.clone(),
        media: Arc::new(nd_platform::NullMedia),
        stats: Arc::new(nd_platform::NullStats),
        camera: Arc::new(nd_core::NullCamera),
        mic: Arc::new(nd_core::NullMic),
        profile_actions: Arc::new(nd_core::autoswitch::NullProfileActions),
        mixer: Arc::new(Mixer::new(Arc::new(MockAppAudio::new()), audio)),
        obs: Arc::new(nd_core::NullObs),
        icons: None,
    };

    let identity = Arc::new(HostIdentity::load_or_create(&dir).unwrap());
    let trust = Arc::new(MemoryTrustStore::new(identity.fingerprint()));
    let mut server =
        ControlServer::bind_tls("127.0.0.1:0", identity.clone(), trust.clone() as Arc<dyn TrustStore>).await.unwrap();
    let addr: SocketAddr = server.local_addr();
    let handle = server.handle();
    let _input_rx = server.take_input_receiver().unwrap();
    let (_task, shutdown) = server.spawn();

    let mut tasks = Vec::new();
    tasks.push(tokio::spawn(nd_agent::plugins::event_loop(
        ev_rx,
        state.clone(),
        handle.clone(),
        Arc::new(MockLauncher::new()),
    )));

    let cid = Arc::new(ClientIdentity::generate("plugins").unwrap());
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
    let feedback: Arc<Mutex<Vec<nd_proto::v1::Feedback>>> = Arc::default();
    let layouts: Arc<Mutex<usize>> = Arc::default();
    let (mut rx, u, f, l) = (phone.subscribe(), updates.clone(), feedback.clone(), layouts.clone());
    tasks.push(tokio::spawn(async move {
        while let Ok(env) = rx.recv().await {
            match env.msg {
                Some(Msg::UpdateControls(x)) => u.lock().unwrap().push(x),
                Some(Msg::Feedback(x)) => f.lock().unwrap().push(x),
                Some(Msg::SetLayout(_)) => *l.lock().unwrap() += 1,
                _ => {}
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
    let control: Arc<dyn PluginControl> = host.clone();
    tasks.push(tokio::spawn(live_ticker_with(state.clone(), handle, providers, poke, control)));
    Rig { phone, state, events: ev_tx, host, updates, feedback, layouts, shutdown, tasks, dir }
}

async fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t = tokio::time::Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clock_plugin_drives_the_key_title_and_host_events_reach_the_phone() {
    let Some(staged) = staged_dir() else {
        eprintln!("skipped: run scripts/stage-plugins.sh host/target/plugins-staged first");
        return;
    };
    tokio::time::timeout(Duration::from_secs(60), async {
        let r = rig(staged).await;
        // The clock sets a "HH:MM" title through willAppear -> setTitle -> poke -> live tick.
        let title = until("clock title", || {
            let all = r.updates.lock().unwrap();
            all.iter()
                .flat_map(|u| u.patches.iter())
                .filter(|p| p.id == "clk")
                .filter_map(|p| p.control.as_ref())
                .map(|c| c.title.clone())
                .find(|t| t.contains(':') && t.chars().any(|c| c.is_ascii_digit()))
        })
        .await;
        assert!(title.len() >= 4, "{title}");
        let _ = &r.phone;

        // showOk / showAlert from a plugin flash the control on the phone.
        let ctx = plugin_context("default", "clk");
        r.events.send(HostEvent::Feedback { context: ctx.clone(), ok: true }).unwrap();
        r.events.send(HostEvent::Feedback { context: ctx.clone(), ok: false }).unwrap();
        r.events.send(HostEvent::Feedback { context: plugin_context("default", "ghost"), ok: true }).unwrap();
        until("feedback", || (r.feedback.lock().unwrap().len() >= 2).then_some(())).await;
        let fb = r.feedback.lock().unwrap().clone();
        assert_eq!(fb.len(), 2, "unknown contexts are ignored: {fb:?}");
        assert!(fb.iter().all(|f| f.control_id == "clk"));
        assert_eq!((fb[0].kind, fb[1].kind), (FeedbackKind::ShowOk as i32, FeedbackKind::ShowAlert as i32));

        // setSettings persists into the stored profile and the running session, without a layout push.
        let layouts_before = *r.layouts.lock().unwrap();
        let mut s = PluginSettings::new();
        s.insert("format".into(), "12h".into());
        s.insert("seconds".into(), true.into());
        r.events.send(HostEvent::SetSettings { context: ctx, plugin: CLOCK.into(), settings: s.clone() }).unwrap();
        let stored = until("persisted settings", || {
            let p = r.state.get_profile("default").unwrap();
            match &p.pages[0].controls[0].actions[0].action {
                Action::Plugin { settings, .. } if *settings == s => Some(p),
                _ => None,
            }
        })
        .await;
        assert_eq!(stored.pages[0].controls[0].title, "Clock", "runtime titles must not leak into the profile");
        let live = r.state.plugin_instances_all();
        assert_eq!(live[0].1.settings, s, "session copy updated in place");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(*r.layouts.lock().unwrap(), layouts_before, "no SetLayout for a settings save");
        assert!(!r.state.poll_changes(), "own write must not trigger a profile reload");
    })
    .await
    .expect("hung");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn play_and_stop_events_reach_the_phone_player() {
    tokio::time::timeout(Duration::from_secs(60), async {
        // No plugin needed: the host events are what `phone.play` / `phone.stop` produce.
        let r = rig(std::path::PathBuf::from("/nonexistent-plugin-dir")).await;
        r.events
            .send(HostEvent::PlayOnPhone {
                plugin: "dev.test.p".into(),
                context: None,
                url: "https://example.com/a.m3u8".into(),
                title: "chan".into(),
                subtitle: "sub".into(),
                max_height: 720,
                muted: true,
                sync_delay_ms: 8000,
                session: "s1".into(),
            })
            .unwrap();
        let t = tokio::time::Instant::now();
        while r.phone.play_stream_history().await.is_empty() {
            assert!(t.elapsed() < Duration::from_secs(5), "no PlayStream received");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let p = r.phone.latest_play_stream().await.unwrap();
        assert_eq!(
            (p.url.as_str(), p.title.as_str(), p.subtitle.as_str(), p.max_height),
            ("https://example.com/a.m3u8", "chan", "sub", 720)
        );
        assert_eq!((p.muted, p.sync_delay_ms, p.session_id.as_str()), (true, 8000, "dev.test.p/s1"));

        r.events.send(HostEvent::StopOnPhone).unwrap();
        while r.phone.play_stream_history().await.len() < 2 {
            assert!(t.elapsed() < Duration::from_secs(5), "no stop PlayStream received");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(r.phone.latest_play_stream().await.unwrap(), nd_proto::v1::PlayStream::default());
    })
    .await
    .expect("hung");
}
