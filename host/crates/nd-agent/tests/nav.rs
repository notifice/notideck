// SPDX-License-Identifier: GPL-3.0-or-later
//! Navigation rail end to end: real ControlServer, a fake phone, the real `Nav`, `AgentState` and
//! `AutoSwitchService` (no focus backend), on ephemeral ports. The input path runs `handle_input`
//! under the state write lock, so a deadlock in Home / Main-menu handling shows up as a timeout.
#![cfg(not(windows))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use nd_agent::autoswitch::AutoSwitchService;
use nd_agent::nav::Nav;
use nd_agent::{AgentState, StateEvent};
use nd_core::autoswitch::{AutoSwitchConfig, ProfileActions, SwitchTarget};
use nd_core::mock::{MockInputInjector, MockLauncher};
use nd_core::{Action, ActionRouter, Control, InputReaction, Page, Profile, ProfileStore};
use nd_fakephone::store::ClientIdentity;
use nd_fakephone::{FakePhone, FakePhoneConfig};
use nd_platform::NullFocus;
use nd_proto::v1::{NavAction, NavRequest, NavState};
use nd_proto::Msg;
use nd_transport::{ControlServer, HostIdentity, MemoryTrustStore, PeerInfo, TransportEvent, TrustStore};
use tokio::sync::Notify;

struct Rig {
    phone: FakePhone,
    state: AgentState,
    svc: Arc<AutoSwitchService>,
    shutdown: tokio::sync::watch::Sender<bool>,
    dir: std::path::PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.svc.stop();
        let _ = self.shutdown.send(true);
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn main_profile() -> Profile {
    let mut p = Profile::new("default", "Main");
    let mut home = Page::new("home", "Home");
    home.add_control(Control::key("folder", 0, 0, "Folder").with_action(Action::open_folder("f")));
    p.add_page(home);
    p.add_page(Page::new("f", "Folder"));
    p.add_page(Page::new("p2", "Page 2"));
    p
}

fn other_profile() -> Profile {
    let mut p = Profile::new("other", "Other");
    let mut page = Page::new("o1", "O1");
    page.add_control(Control::key("menu", 0, 0, "Menu").with_action(Action::GoHome { pause_auto_switch: true }));
    p.add_page(page);
    p
}

async fn rig() -> Rig {
    let dir = std::env::temp_dir().join(format!("nd-agent-nav-test-{}", uuid::Uuid::new_v4()));
    let state = AgentState::open(ProfileStore::new(dir.join("profiles"))).unwrap();
    state.put_profile(main_profile()).unwrap();
    state.put_profile(other_profile()).unwrap();
    let svc = AutoSwitchService::start(dir.clone(), state.clone(), Box::new(NullFocus));
    svc.put_config(AutoSwitchConfig { home_profile_id: Some("default".into()), ..Default::default() }).unwrap();
    let router = ActionRouter::new(Arc::new(MockInputInjector::new()), Arc::new(MockLauncher::new()))
        .with_profile_actions(svc.clone());

    let identity = Arc::new(HostIdentity::load_or_create(&dir).unwrap());
    let trust = Arc::new(MemoryTrustStore::new(identity.fingerprint()));
    let mut server =
        ControlServer::bind_tls("127.0.0.1:0", identity.clone(), trust.clone() as Arc<dyn TrustStore>).await.unwrap();
    let addr: SocketAddr = server.local_addr();
    let handle = server.handle();
    let mut input_rx = server.take_input_receiver().unwrap();
    let mut events = handle.subscribe_events();
    let (_task, shutdown) = server.spawn();

    // The same wiring as app.rs.
    let nav = Nav::new(state.clone(), svc.clone(), handle.clone());
    let poke = Arc::new(Notify::new());
    {
        let poke = poke.clone();
        svc.set_on_change(move || poke.notify_one());
    }
    let n = nav.clone();
    tokio::spawn(async move {
        loop {
            poke.notified().await;
            n.publish(false).await;
        }
    });
    let n = nav.clone();
    let st = state.clone();
    tokio::spawn(async move {
        while let Ok(ev) = events.recv().await {
            match ev {
                TransportEvent::SessionConnected(s) => {
                    if let Ok(l) = st.current_layout() {
                        let _ = s.set_layout(l).await;
                    }
                    n.send_to(&s).await;
                    n.send_rail_to(&s).await;
                }
                TransportEvent::Other(env) => {
                    if let Some(Msg::NavRequest(req)) = env.msg {
                        let n = n.clone();
                        tokio::spawn(async move { n.handle_request(req).await });
                    }
                }
                _ => {}
            }
        }
    });
    let n = nav.clone();
    let mut state_rx = state.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = state_rx.recv().await {
            match ev {
                StateEvent::LayoutChanged(l) => n.push_layout(l).await,
                StateEvent::RailChanged(r) => n.broadcast_rail(r).await,
                _ => {}
            }
        }
    });
    let (n, st, h) = (nav.clone(), state.clone(), handle.clone());
    tokio::spawn(async move {
        while let Some(input) = input_rx.recv().await {
            let (st, router) = (st.clone(), router.clone());
            let res = tokio::task::spawn_blocking(move || st.handle_input(&input, &router)).await.unwrap();
            match res {
                Ok(InputReaction::SetLayout(l)) => n.push_layout(l).await,
                Ok(InputReaction::UpdateControls(u)) => {
                    let _ = h.update_controls(u).await;
                }
                _ => {}
            }
        }
    });

    let cid = Arc::new(ClientIdentity::generate("nav").unwrap());
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
    Rig { phone, state, svc, shutdown, dir }
}

/// Next `NavState` (skipping the ones before it) that satisfies `pred`.
async fn nav_where(
    rx: &mut tokio::sync::broadcast::Receiver<nd_proto::Envelope>,
    what: &str,
    pred: impl Fn(&NavState) -> bool,
) -> NavState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(env)) => {
                if let Some(Msg::NavState(n)) = env.msg {
                    if pred(&n) {
                        return n;
                    }
                    seen.push(n);
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            _ => panic!("timed out waiting for NavState: {what}; saw {seen:?}"),
        }
    }
}

/// Let trailing duplicate navigation messages of an earlier change arrive, then drop everything queued.
async fn settle(rx: &mut tokio::sync::broadcast::Receiver<nd_proto::Envelope>) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    while rx.try_recv().is_ok() {}
}

/// No `NavState` / `SetLayout` for 400 ms. Other traffic (live-data patches, icons) is ignored.
async fn quiet(rx: &mut tokio::sync::broadcast::Receiver<nd_proto::Envelope>, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
    while let Ok(res) = tokio::time::timeout_at(deadline, rx.recv()).await {
        if let Ok(env) = res {
            if matches!(env.msg, Some(Msg::NavState(_)) | Some(Msg::SetLayout(_))) {
                panic!("{what}: expected no navigation message, got {:?}", env.msg);
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nav_requests_and_nav_state() {
    tokio::time::timeout(Duration::from_secs(60), scenario()).await.expect("nav scenario hung (lock deadlock?)");
}

async fn scenario() {
    let r = rig().await;
    let phone = &r.phone;
    let mut rx = phone.subscribe();

    // Initial: layout then NavState (already sent before we subscribed, so read the recorded one).
    let t = tokio::time::Instant::now();
    let initial = loop {
        if let Some(n) = phone.latest_nav_state().await {
            break n;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "no initial NavState");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        initial,
        NavState {
            can_go_back: false,
            auto_switch_paused: false,
            on_home: true,
            profile_name: "Main".into(),
            page_index: 0,
            page_count: 3,
        }
    );

    // Inside a folder (opened by a real tap): Back is available, no longer home. BACK closes it.
    phone.send_key_down_on_page("home", "folder").await.unwrap();
    let n = nav_where(&mut rx, "folder", |n| n.can_go_back).await;
    assert!(!n.on_home);
    assert_eq!(r.state.active_page_id(), "f");
    phone.send_nav_request(NavAction::Back).await.unwrap();
    let n = nav_where(&mut rx, "back from folder", |n| !n.can_go_back).await;
    assert!(n.on_home);
    assert_eq!(r.state.active_page_id(), "home");

    // Non-initial page: BACK returns to the initial page; a second BACK is a no-op.
    let to_p2 = SwitchTarget { profile_id: "default".into(), page_id: Some("p2".into()), rule_id: None };
    r.state.apply_switch(&to_p2).unwrap();
    let n = nav_where(&mut rx, "page 2", |n| n.can_go_back).await;
    assert!(!n.on_home);
    phone.send_nav_request(NavAction::Back).await.unwrap();
    nav_where(&mut rx, "back to initial", |n| !n.can_go_back && n.on_home).await;
    assert_eq!(r.state.active_page_id(), "home");
    settle(&mut rx).await;
    phone.send_nav_request(NavAction::Back).await.unwrap();
    quiet(&mut rx, "BACK on the initial page").await;

    // Unspecified and unknown actions are ignored.
    phone.send_nav_request(NavAction::Unspecified).await.unwrap();
    phone.send_message(Msg::NavRequest(NavRequest { action: 99, page_index: 0 })).await.unwrap();
    quiet(&mut rx, "unknown nav action").await;

    // Another profile: not home, name follows. HOME shows the main profile.
    r.state.activate_profile("other").unwrap();
    let n = nav_where(&mut rx, "other profile", |n| n.profile_name == "Other").await;
    assert!(!n.on_home && !n.can_go_back);
    phone.send_nav_request(NavAction::Home).await.unwrap();
    let n = nav_where(&mut rx, "home", |n| n.profile_name == "Main").await;
    assert!(n.on_home && !n.auto_switch_paused);

    // HOME_PAUSE: main profile and paused. HOME while paused only resumes.
    r.state.activate_profile("other").unwrap();
    nav_where(&mut rx, "other again", |n| n.profile_name == "Other").await;
    phone.send_nav_request(NavAction::HomePause).await.unwrap();
    let n = nav_where(&mut rx, "home+pause", |n| n.profile_name == "Main" && n.auto_switch_paused).await;
    assert!(n.on_home);
    assert!(r.svc.paused());
    phone.send_nav_request(NavAction::Home).await.unwrap();
    let n = nav_where(&mut rx, "home while paused", |n| !n.auto_switch_paused).await;
    assert!(n.on_home && !r.svc.paused());

    // RESUME.
    phone.send_nav_request(NavAction::HomePause).await.unwrap();
    nav_where(&mut rx, "paused again", |n| n.auto_switch_paused).await;
    phone.send_nav_request(NavAction::Resume).await.unwrap();
    nav_where(&mut rx, "resume", |n| !n.auto_switch_paused).await;
    assert!(!r.svc.paused());

    // The deck's own Main-menu key runs under the state lock and must not deadlock.
    r.state.activate_profile("other").unwrap();
    nav_where(&mut rx, "other for menu key", |n| n.profile_name == "Other").await;
    phone.send_key_down_on_page("o1", "menu").await.unwrap();
    let n = nav_where(&mut rx, "menu key", |n| n.profile_name == "Main" && n.auto_switch_paused).await;
    assert!(n.on_home);
}

async fn next_msg<T>(
    rx: &mut tokio::sync::broadcast::Receiver<nd_proto::Envelope>,
    what: &str,
    pick: impl Fn(Msg) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(env)) => {
                if let Some(t) = env.msg.and_then(&pick) {
                    return t;
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            _ => panic!("timed out waiting for {what}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn page_dots_go_to_page() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let r = rig().await;
        let mut rx = r.phone.subscribe();
        r.phone.send_nav_go_to_page(2).await.unwrap();
        let n = nav_where(&mut rx, "page 3", |n| n.page_index == 2).await;
        assert_eq!(n.page_count, 3);
        assert_eq!(r.state.active_page_id(), "p2");
        // Out of range is ignored. Let the trailing duplicate NavState of the previous change arrive first.
        settle(&mut rx).await;
        r.phone.send_nav_go_to_page(9).await.unwrap();
        quiet(&mut rx, "out-of-range page").await;
        assert_eq!(r.state.active_page_id(), "p2");
        // Back to the first page, open the folder: dots hidden (count 0); a dot closes the folder.
        r.phone.send_nav_go_to_page(0).await.unwrap();
        nav_where(&mut rx, "page 1", |n| n.page_index == 0 && n.page_count == 3).await;
        r.phone.send_key_down_on_page("home", "folder").await.unwrap();
        let n = nav_where(&mut rx, "folder", |n| n.page_count == 0).await;
        assert!(n.can_go_back);
        r.phone.send_nav_go_to_page(1).await.unwrap();
        let n = nav_where(&mut rx, "page via dot from folder", |n| n.page_count == 3).await;
        assert_eq!(n.page_index, 1);
        assert_eq!(r.state.active_page_id(), "f");
    })
    .await
    .expect("hung");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rail_keys_are_sent_pressed_and_patched() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let r = rig().await;
        let mut rx = r.phone.subscribe();

        // A rail saved before the phone connected arrives right after the first layout; here the
        // phone is already connected, so the empty rail was sent first and the new one follows.
        let keys = vec![
            Control::key("mute", 0, 0, "Mute").with_action(Action::ToggleState),
            Control::key("menu", 0, 0, "Menu").with_action(Action::GoHome { pause_auto_switch: false }),
            Control::key("next", 0, 0, "Next").with_action(Action::NextPage),
        ];
        r.state.set_rail(keys.clone()).unwrap();
        let rail = next_msg(&mut rx, "SetRail", |m| match m {
            Msg::SetRail(r) if !r.controls.is_empty() => Some(r),
            _ => None,
        })
        .await;
        assert_eq!(rail.controls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["mute", "menu", "next"]);
        assert_eq!(r.phone.latest_rail().await.unwrap(), rail);
        // Validation: too many / non-key rails are refused and nothing is sent.
        let mut four = keys.clone();
        four.push(Control::key("x", 0, 0, "X"));
        assert!(r.state.set_rail(four).is_err());
        assert_eq!(r.state.rail_keys().len(), 3);

        // ToggleState on a rail key patches page "__rail" (and again to toggle back).
        r.phone.simulate_rail_tap("mute").await.unwrap();
        let up = next_msg(&mut rx, "rail patch", |m| match m {
            Msg::UpdateControls(u) => Some(u),
            _ => None,
        })
        .await;
        assert_eq!(up.page_id, "__rail");
        assert_eq!(up.patches[0].id, "mute");
        assert_eq!(up.patches[0].control.as_ref().unwrap().state_index, 1);
        r.phone.simulate_rail_tap("mute").await.unwrap();
        let up = next_msg(&mut rx, "rail patch 2", |m| match m {
            Msg::UpdateControls(u) => Some(u),
            _ => None,
        })
        .await;
        assert_eq!(up.patches[0].control.as_ref().unwrap().state_index, 0);

        // A rail NextPage key navigates the active profile.
        r.phone.simulate_rail_tap("next").await.unwrap();
        nav_where(&mut rx, "next page", |n| n.page_index == 1).await;
        assert_eq!(r.state.active_page_id(), "f");

        // A rail GoHome key runs under the state lock without deadlocking.
        r.state.activate_profile("other").unwrap();
        nav_where(&mut rx, "other", |n| n.profile_name == "Other").await;
        r.phone.simulate_rail_tap("menu").await.unwrap();
        nav_where(&mut rx, "rail home", |n| n.profile_name == "Main").await;
        assert_eq!(r.state.active_profile_id(), "default");

        // Rail live data patches carry the rail page id.
        let s = nd_core::LiveSample { title: Some("hi".into()), ..Default::default() };
        let mut live = keys.clone();
        live[0].source = Some(nd_core::DataSource::Clock);
        r.state.set_rail(live).unwrap();
        next_msg(&mut rx, "SetRail 2", |m| matches!(m, Msg::SetRail(_)).then_some(())).await;
        let sources = r.state.live_sources_all();
        assert!(sources.iter().any(|(id, _)| id.ends_with("mute")));
        let id = sources.iter().find(|(id, _)| id.ends_with("mute")).unwrap().0.clone();
        let patches = r.state.apply_live_all(&[(id, s)]);
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].page_id, "__rail");
    })
    .await
    .expect("rail scenario hung (lock deadlock?)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rail_config_persists_and_backend_put_pushes_set_rail() {
    use nd_config::ConfigBackend;
    tokio::time::timeout(Duration::from_secs(60), async {
        let r = rig().await;
        let mut rx = r.phone.subscribe();
        // The session got SetRail (empty) after its first SetLayout.
        let t = tokio::time::Instant::now();
        while r.phone.latest_rail().await.is_none() {
            assert!(t.elapsed() < Duration::from_secs(5), "no initial SetRail");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let order: Vec<&'static str> = r
            .phone
            .all_received()
            .await
            .iter()
            .filter_map(|e| match e.msg {
                Some(Msg::SetLayout(_)) => Some("layout"),
                Some(Msg::SetRail(_)) => Some("rail"),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["layout", "rail"]);
        let backend = nd_agent::AgentConfigBackend::new(r.state.clone());
        assert!(backend.rail_get().is_empty());
        backend.rail_put(vec![Control::key("a", 0, 0, "A")]).unwrap();
        let rail = next_msg(&mut rx, "SetRail", |m| match m {
            Msg::SetRail(r) if !r.controls.is_empty() => Some(r),
            _ => None,
        })
        .await;
        assert_eq!(rail.controls[0].id, "a");
        assert_eq!(backend.rail_get().len(), 1);
        let bad = vec![Control::key("a", 0, 0, "A"), Control::key("a", 0, 0, "B")];
        assert!(matches!(backend.rail_put(bad), Err(nd_config::RailPutError::Invalid(_))));
        // Reopening the state from the same directory loads rail.json.
        let again = AgentState::open(ProfileStore::new(r.dir.join("profiles"))).unwrap();
        assert_eq!(again.rail_keys().len(), 1, "rail.json is reloaded from the config root");
        assert_eq!(again.rail_keys()[0].id, "a");
    })
    .await
    .expect("hung");
}
