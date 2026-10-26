// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests for the round 17 media session actions, sources and the mixer media target.

use crate::error::CoreError;
use crate::live::{sample_all, LiveProviders, LiveSample};
use crate::mixer::{Mixer, MEDIA_APP_MAX_AGE};
use crate::mock::{MockAppAudio, MockAudio, MockInputInjector, MockLauncher};
use crate::model::{Action, AudioApp, DataSource, MediaTransportCmd, RepeatAction};
use crate::obs::ObsToggle;
use crate::router::{ActionOutcome, ActionRouter, InputContext};
use nd_platform::{Key, KeyCombo, MediaCommand, MediaInfo, NowPlaying, PlatformError, RepeatMode, Result as PResult};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How the fake answers `control`.
#[derive(Clone, Copy)]
enum Reply {
    Ok,
    Unsupported,
    NoPlayer,
}

struct FakeMedia {
    np: Mutex<Option<NowPlaying>>,
    reply: Mutex<Reply>,
    cmds: Mutex<Vec<MediaCommand>>,
    polls: AtomicUsize,
}

impl FakeMedia {
    fn new(np: Option<NowPlaying>, reply: Reply) -> Arc<Self> {
        Arc::new(Self {
            np: Mutex::new(np),
            reply: Mutex::new(reply),
            cmds: Mutex::new(vec![]),
            polls: AtomicUsize::new(0),
        })
    }
    fn cmds(&self) -> Vec<MediaCommand> {
        self.cmds.lock().unwrap().clone()
    }
}

impl MediaInfo for FakeMedia {
    fn now_playing(&self) -> PResult<Option<NowPlaying>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        Ok(self.np.lock().unwrap().clone())
    }
    fn control(&self, cmd: MediaCommand) -> PResult<()> {
        self.cmds.lock().unwrap().push(cmd);
        match *self.reply.lock().unwrap() {
            Reply::Ok => Ok(()),
            Reply::Unsupported => Err(PlatformError::Unsupported("media session control")),
            Reply::NoPlayer => Err(PlatformError::Other("no media player".into())),
        }
    }
}

fn player(f: impl FnOnce(&mut NowPlaying)) -> NowPlaying {
    let mut np = NowPlaying { title: "T".into(), playing: true, ..Default::default() };
    f(&mut np);
    np
}

fn router(media: Arc<FakeMedia>) -> (ActionRouter, MockInputInjector) {
    let inj = MockInputInjector::new();
    let r = ActionRouter::new(Arc::new(inj.clone()), Arc::new(MockLauncher::new())).with_media(media);
    (r, inj)
}

fn run(r: &ActionRouter, a: Action, ctx: InputContext) -> crate::Result<ActionOutcome> {
    r.execute(&a, &ctx)
}

fn ctx() -> InputContext {
    InputContext::default()
}

fn key(k: Key) -> KeyCombo {
    KeyCombo { modifiers: vec![], key: k }
}

#[test]
fn transport_success_sends_command_only() {
    let m = FakeMedia::new(Some(player(|_| {})), Reply::Ok);
    let (r, inj) = router(m.clone());
    let out = run(&r, Action::MediaTransport { command: MediaTransportCmd::Next }, ctx()).unwrap();
    assert_eq!(out, ActionOutcome::Executed);
    assert_eq!(m.cmds(), vec![MediaCommand::Next]);
    assert!(inj.key_combos().is_empty());
}

#[test]
fn transport_unsupported_falls_back_to_hotkeys() {
    use MediaTransportCmd as C;
    let cases = [
        (C::PlayPause, Key::MediaPlayPause),
        (C::Play, Key::MediaPlayPause),
        (C::Pause, Key::MediaPlayPause),
        (C::Next, Key::MediaNext),
        (C::Prev, Key::MediaPrev),
        (C::Stop, Key::MediaStop),
    ];
    for (cmd, k) in cases {
        let m = FakeMedia::new(None, Reply::Unsupported);
        let (r, inj) = router(m);
        assert_eq!(run(&r, Action::MediaTransport { command: cmd }, ctx()).unwrap(), ActionOutcome::Executed);
        assert_eq!(inj.key_combos(), vec![key(k)], "{cmd:?}");
    }
}

#[test]
fn transport_no_player_fallback_rules() {
    let m = FakeMedia::new(None, Reply::NoPlayer);
    let (r, inj) = router(m);
    // Play / Pause surface the error and press nothing.
    for cmd in [MediaTransportCmd::Play, MediaTransportCmd::Pause] {
        let err = run(&r, Action::MediaTransport { command: cmd }, ctx()).unwrap_err();
        assert!(matches!(err, CoreError::Platform(PlatformError::Other(_))));
    }
    assert!(inj.key_combos().is_empty());
    // The others try the hotkey.
    run(&r, Action::MediaTransport { command: MediaTransportCmd::PlayPause }, ctx()).unwrap();
    run(&r, Action::MediaTransport { command: MediaTransportCmd::Next }, ctx()).unwrap();
    assert_eq!(inj.key_combos(), vec![key(Key::MediaPlayPause), key(Key::MediaNext)]);
}

#[test]
fn seek_fraction_with_and_without_length() {
    let m = FakeMedia::new(Some(player(|n| n.length_s = Some(200.0))), Reply::Ok);
    let (r, _) = router(m.clone());
    // Explicit position wins over the event value.
    let out = run(
        &r,
        Action::MediaSeek { position: Some(0.25) },
        InputContext { delta: 0, value: 0.9, ..Default::default() },
    )
    .unwrap();
    assert_eq!(out, ActionOutcome::SetValue(0.25));
    // Absent position uses ctx.value, clamped.
    let out =
        run(&r, Action::MediaSeek { position: None }, InputContext { delta: 0, value: 1.5, ..Default::default() })
            .unwrap();
    assert_eq!(out, ActionOutcome::SetValue(1.0));
    assert_eq!(m.cmds(), vec![MediaCommand::Seek { position_s: 50.0 }, MediaCommand::Seek { position_s: 200.0 }]);

    let m = FakeMedia::new(Some(player(|_| {})), Reply::Ok);
    let (r, _) = router(m.clone());
    let err = run(&r, Action::MediaSeek { position: Some(0.5) }, ctx()).unwrap_err();
    assert!(matches!(err, CoreError::UnsupportedAction(ref s) if s.contains("length")));
    assert!(m.cmds().is_empty());
    let (r, _) = router(FakeMedia::new(None, Reply::Ok));
    assert!(run(&r, Action::MediaSeek { position: Some(0.5) }, ctx()).is_err());
}

#[test]
fn seek_by_scales_with_dial_delta() {
    let m = FakeMedia::new(Some(player(|_| {})), Reply::Ok);
    let (r, _) = router(m.clone());
    run(&r, Action::MediaSeekBy { seconds: 5.0 }, ctx()).unwrap();
    run(&r, Action::MediaSeekBy { seconds: 5.0 }, InputContext { delta: 3, value: 0.0, ..Default::default() }).unwrap();
    run(&r, Action::MediaSeekBy { seconds: 5.0 }, InputContext { delta: -2, value: 0.0, ..Default::default() })
        .unwrap();
    assert_eq!(
        m.cmds(),
        vec![
            MediaCommand::SeekBy { delta_s: 5.0 },
            MediaCommand::SeekBy { delta_s: 15.0 },
            MediaCommand::SeekBy { delta_s: -10.0 },
        ]
    );
}

#[test]
fn shuffle_toggle_on_off() {
    let m = FakeMedia::new(Some(player(|n| n.shuffle = Some(true))), Reply::Ok);
    let (r, _) = router(m.clone());
    let a = |mode| Action::MediaShuffle { mode };
    assert_eq!(run(&r, a(ObsToggle::Toggle), ctx()).unwrap(), ActionOutcome::SetState(0));
    assert_eq!(run(&r, a(ObsToggle::On), ctx()).unwrap(), ActionOutcome::SetState(1));
    assert_eq!(run(&r, a(ObsToggle::Off), ctx()).unwrap(), ActionOutcome::SetState(0));
    // Unknown shuffle counts as off, so toggle turns it on.
    *m.np.lock().unwrap() = Some(player(|_| {}));
    assert_eq!(run(&r, a(ObsToggle::Toggle), ctx()).unwrap(), ActionOutcome::SetState(1));
    assert_eq!(
        m.cmds(),
        vec![
            MediaCommand::Shuffle { on: false },
            MediaCommand::Shuffle { on: true },
            MediaCommand::Shuffle { on: false },
            MediaCommand::Shuffle { on: true },
        ]
    );
}

#[test]
fn repeat_cycle_and_set() {
    let m = FakeMedia::new(Some(player(|n| n.repeat = Some(RepeatMode::Off))), Reply::Ok);
    let (r, _) = router(m.clone());
    let cycle = Action::MediaRepeat { mode: RepeatAction::Cycle };
    let mut states = vec![];
    for cur in [Some(RepeatMode::Off), Some(RepeatMode::List), Some(RepeatMode::Track), None] {
        *m.np.lock().unwrap() = Some(player(|n| n.repeat = cur));
        match run(&r, cycle.clone(), ctx()).unwrap() {
            ActionOutcome::SetState(s) => states.push(s),
            o => panic!("{o:?}"),
        }
    }
    assert_eq!(states, vec![1, 2, 0, 1]);
    assert_eq!(
        m.cmds(),
        vec![
            MediaCommand::Repeat { mode: RepeatMode::List },
            MediaCommand::Repeat { mode: RepeatMode::Track },
            MediaCommand::Repeat { mode: RepeatMode::Off },
            MediaCommand::Repeat { mode: RepeatMode::List },
        ]
    );
    assert_eq!(run(&r, Action::MediaRepeat { mode: RepeatAction::Track }, ctx()).unwrap(), ActionOutcome::SetState(2));
    assert_eq!(run(&r, Action::MediaRepeat { mode: RepeatAction::Off }, ctx()).unwrap(), ActionOutcome::SetState(0));
}

// ---- live ----

fn providers(media: Arc<dyn MediaInfo>) -> LiveProviders {
    LiveProviders {
        audio: Arc::new(nd_platform::NullAudio),
        media,
        stats: Arc::new(nd_platform::NullStats),
        camera: Arc::new(crate::NullCamera),
        mic: Arc::new(crate::NullMic),
        profile_actions: Arc::new(crate::autoswitch::NullProfileActions),
        mixer: Arc::new(Mixer::new(Arc::new(nd_platform::NullAppAudio), Arc::new(nd_platform::NullAudio))),
        obs: Arc::new(crate::obs::NullObs),
        icons: None,
    }
}

fn sources() -> Vec<(String, DataSource)> {
    vec![
        ("np".into(), DataSource::NowPlaying { art: false }),
        ("sh".into(), DataSource::MediaShuffle),
        ("rp".into(), DataSource::MediaRepeat),
        ("pl".into(), DataSource::MediaPlaying),
    ]
}

fn state(state_index: u32, dimmed: bool) -> LiveSample {
    LiveSample { state_index: Some(state_index), dimmed: Some(dimmed), ..Default::default() }
}

#[test]
fn live_shuffle_repeat_single_poll() {
    let m = FakeMedia::new(
        Some(player(|n| {
            n.shuffle = Some(true);
            n.repeat = Some(RepeatMode::Track);
        })),
        Reply::Ok,
    );
    let out = sample_all(&sources(), &providers(m.clone()));
    assert_eq!(m.polls.load(Ordering::SeqCst), 1);
    assert_eq!(out[1].1, state(1, false));
    assert_eq!(out[2].1, state(2, false));

    for (rep, want) in [(RepeatMode::Off, 0), (RepeatMode::List, 1)] {
        *m.np.lock().unwrap() = Some(player(|n| n.repeat = Some(rep)));
        assert_eq!(sample_all(&sources(), &providers(m.clone()))[2].1, state(want, false));
    }
}

#[test]
fn live_shuffle_repeat_dimmed() {
    // Player without the fields.
    let m = FakeMedia::new(Some(player(|_| {})), Reply::Ok);
    let out = sample_all(&sources(), &providers(m));
    assert_eq!(out[1].1, state(0, true));
    assert_eq!(out[2].1, state(0, true));
    // No player.
    let m = FakeMedia::new(None, Reply::Ok);
    let out = sample_all(&sources(), &providers(m));
    assert_eq!(out[1].1, state(0, true));
    assert_eq!(out[2].1, state(0, true));
}

// ---- mixer media target ----

struct Rig {
    mixer: Mixer,
    ms: Arc<AtomicU64>,
    provider_calls: Arc<AtomicUsize>,
    player: Arc<Mutex<Option<String>>>,
}

fn rig(sessions: Vec<nd_platform::AudioSession>, player_id: Option<&str>) -> Rig {
    let b = MockAppAudio::new();
    b.set_sessions(sessions);
    let base = Instant::now();
    let ms = Arc::new(AtomicU64::new(0));
    let m2 = ms.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = calls.clone();
    let player = Arc::new(Mutex::new(player_id.map(String::from)));
    let p2 = player.clone();
    let mixer = Mixer::new(Arc::new(b), Arc::new(MockAudio::new()))
        .with_clock(Arc::new(move || base + Duration::from_millis(m2.load(Ordering::SeqCst))))
        .with_media_app(Arc::new(move || {
            c2.fetch_add(1, Ordering::SeqCst);
            p2.lock().unwrap().clone()
        }));
    Rig { mixer, ms, provider_calls: calls, player }
}

fn sess(id: &str, process: &str, name: &str, active: bool) -> nd_platform::AudioSession {
    let mut s = MockAppAudio::session(id, process, name, 0.5);
    s.active = active;
    s
}

#[test]
fn media_target_matches_key_and_display_name() {
    let r = rig(
        vec![sess("1", "C:\\x\\Spotify.exe", "Spotify", true), sess("2", "chrome.exe", "Chrome", true)],
        Some("Spotify.exe"),
    );
    assert_eq!(r.mixer.resolve(&AudioApp::Media).unwrap().key, "spotify");

    // AUMID: `!App` part first; display-name match when the process key differs.
    let r = rig(
        vec![sess("1", "player.exe", "Spotify", true), sess("2", "chrome.exe", "Chrome", true)],
        Some("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"),
    );
    assert_eq!(r.mixer.resolve(&AudioApp::Media).unwrap().key, "player");
}

#[test]
fn media_target_includes_hidden_apps() {
    let r =
        rig(vec![sess("1", "spotify.exe", "Spotify", true), sess("2", "chrome.exe", "Chrome", true)], Some("spotify"));
    r.mixer.set_config(crate::mixer::MixerConfig { hidden: vec!["spotify".into()], ..Default::default() });
    let a = r.mixer.resolve(&AudioApp::Media).unwrap();
    assert_eq!((a.key.as_str(), a.hidden), ("spotify", true));
}

#[test]
fn media_target_falls_back_to_only_active_app() {
    let r = rig(vec![sess("1", "foo.exe", "Foo", false), sess("2", "bar.exe", "Bar", true)], Some("unknown.player"));
    assert_eq!(r.mixer.resolve(&AudioApp::Media).unwrap().key, "bar");
    // Two active apps: ambiguous.
    let r = rig(vec![sess("1", "foo.exe", "Foo", true), sess("2", "bar.exe", "Bar", true)], Some("unknown.player"));
    assert!(r.mixer.resolve(&AudioApp::Media).is_none());
    // No player at all: none, even with one active app.
    let r = rig(vec![sess("2", "bar.exe", "Bar", true)], None);
    assert!(r.mixer.resolve(&AudioApp::Media).is_none());
    // No provider wired: none.
    let b = MockAppAudio::new();
    b.set_sessions(vec![sess("2", "bar.exe", "Bar", true)]);
    let m = Mixer::new(Arc::new(b), Arc::new(MockAudio::new()));
    assert!(m.resolve(&AudioApp::Media).is_none());
}

#[test]
fn media_provider_is_cached_for_max_age() {
    let r = rig(vec![sess("1", "spotify.exe", "Spotify", true)], Some("spotify"));
    for _ in 0..5 {
        r.mixer.resolve(&AudioApp::Media);
    }
    assert_eq!(r.provider_calls.load(Ordering::SeqCst), 1);
    *r.player.lock().unwrap() = None;
    r.ms.store(MEDIA_APP_MAX_AGE.as_millis() as u64 - 1, Ordering::SeqCst);
    assert!(r.mixer.resolve(&AudioApp::Media).is_some());
    assert_eq!(r.provider_calls.load(Ordering::SeqCst), 1);
    r.ms.store(MEDIA_APP_MAX_AGE.as_millis() as u64 + 1, Ordering::SeqCst);
    // Cache expired: the provider is asked again and reports no player.
    assert!(r.mixer.resolve(&AudioApp::Media).is_none());
    assert_eq!(r.provider_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn media_target_routes_through_router() {
    let r = rig(vec![sess("1", "spotify.exe", "Spotify", true)], Some("spotify"));
    let router = ActionRouter::new(Arc::new(MockInputInjector::new()), Arc::new(MockLauncher::new()))
        .with_mixer(Arc::new(r.mixer));
    let out = router
        .execute(&Action::SetAppVolume { app: AudioApp::Media, level: Some(0.3) }, &InputContext::default())
        .unwrap();
    assert_eq!(out, ActionOutcome::SetValue(0.3));
}

// ---- serde ----

#[test]
fn action_json_shapes() {
    let a: Action = serde_json::from_str(r#"{"type":"media_transport","command":"next"}"#).unwrap();
    assert_eq!(a, Action::MediaTransport { command: MediaTransportCmd::Next });
    let a: Action = serde_json::from_str(r#"{"type":"media_seek"}"#).unwrap();
    assert_eq!(a, Action::MediaSeek { position: None });
    let a: Action = serde_json::from_str(r#"{"type":"media_seek","position":0.5}"#).unwrap();
    assert_eq!(a, Action::MediaSeek { position: Some(0.5) });
    let a: Action = serde_json::from_str(r#"{"type":"media_seek_by","seconds":-10}"#).unwrap();
    assert_eq!(a, Action::MediaSeekBy { seconds: -10.0 });
    let a: Action = serde_json::from_str(r#"{"type":"media_shuffle"}"#).unwrap();
    assert_eq!(a, Action::MediaShuffle { mode: ObsToggle::Toggle });
    let a: Action = serde_json::from_str(r#"{"type":"media_repeat"}"#).unwrap();
    assert_eq!(a, Action::MediaRepeat { mode: RepeatAction::Cycle });
    let a: Action = serde_json::from_str(r#"{"type":"media_repeat","mode":"track"}"#).unwrap();
    assert_eq!(a, Action::MediaRepeat { mode: RepeatAction::Track });

    for a in [
        Action::MediaTransport { command: MediaTransportCmd::PlayPause },
        Action::MediaSeek { position: None },
        Action::MediaSeek { position: Some(0.25) },
        Action::MediaSeekBy { seconds: 5.0 },
        Action::MediaShuffle { mode: ObsToggle::On },
        Action::MediaRepeat { mode: RepeatAction::List },
    ] {
        let back: Action = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(back, a);
    }

    let t: AudioApp = serde_json::from_str(r#"{"by":"media"}"#).unwrap();
    assert_eq!(t, AudioApp::Media);
    assert_eq!(serde_json::to_string(&AudioApp::Media).unwrap(), r#"{"by":"media"}"#);
    let s: DataSource = serde_json::from_str(r#"{"type":"media_shuffle"}"#).unwrap();
    assert_eq!(s, DataSource::MediaShuffle);
}
