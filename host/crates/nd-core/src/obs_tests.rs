// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests for the OBS actions and sources (round 14, R14-2) against a mock [`ObsControl`].

use crate::error::CoreError;
use crate::live::{sample_all, LiveProviders};
use crate::mock::{MockInputInjector, MockLauncher};
use crate::model::{Action, DataSource};
use crate::obs::*;
use crate::router::{ActionOutcome, ActionRouter, InputContext};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct MockObs {
    snap: Mutex<ObsSnapshot>,
    sent: Mutex<Vec<ObsCommand>>,
}

impl MockObs {
    fn new(snap: ObsSnapshot) -> Arc<Self> {
        Arc::new(Self { snap: Mutex::new(snap), sent: Mutex::default() })
    }
    fn sent(&self) -> Vec<ObsCommand> {
        self.sent.lock().unwrap().clone()
    }
}

impl ObsControl for MockObs {
    fn snapshot(&self) -> ObsSnapshot {
        self.snap.lock().unwrap().clone()
    }
    fn send(&self, cmd: ObsCommand) -> Result<(), String> {
        self.sent.lock().unwrap().push(cmd);
        Ok(())
    }
}

fn audio(name: &str, muted: bool, mul: f32) -> ObsInputInfo {
    ObsInputInfo { name: name.into(), kind: "audio".into(), muted: Some(muted), volume_mul: Some(mul) }
}

fn item(source: &str, id: i64, enabled: bool) -> ObsSceneItem {
    ObsSceneItem { source: source.into(), item_id: id, enabled }
}

fn snap() -> ObsSnapshot {
    let mut s = ObsSnapshot {
        status: ObsStatus::Connected,
        program_scene: Some("Main".into()),
        scenes: vec!["Main".into(), "Chat".into(), "BRB".into()],
        inputs: vec![
            audio("Mic/Aux", false, 0.125),
            audio("Desktop Audio", true, 1.0),
            ObsInputInfo { name: "Cam".into(), kind: "v4l2".into(), muted: None, volume_mul: None },
        ],
        ..Default::default()
    };
    s.special_inputs.insert(SpecialInput::Mic1, "Mic/Aux".into());
    s.special_inputs.insert(SpecialInput::Desktop1, "Desktop Audio".into());
    s.special_inputs.insert(SpecialInput::Mic2, "Gone".into());
    s.scene_items.insert("Main".into(), vec![item("Cam", 7, true), item("Logo", 8, false)]);
    s.scene_items.insert("Chat".into(), vec![item("Cam", 3, false)]);
    s
}

fn run(m: &Arc<MockObs>, a: Action) -> Result<ActionOutcome, String> {
    execute(&**m, &a, &InputContext::default())
}

fn name(n: &str) -> ObsScene {
    ObsScene::Name { name: n.into() }
}
fn slot(i: u32) -> ObsScene {
    ObsScene::Slot { index: i }
}
fn inp(n: &str) -> ObsInput {
    ObsInput::Name { name: n.into() }
}
fn special(w: SpecialInput) -> ObsInput {
    ObsInput::Special { which: w }
}
fn smp(s: &ObsSnapshot, src: DataSource) -> crate::live::LiveSample {
    sample(s, &src)
}

#[test]
fn helpers_roundtrip_and_format() {
    assert_eq!(fader_to_mul(0.0), 0.0);
    assert_eq!(fader_to_mul(1.0), 1.0);
    assert_eq!(fader_to_mul(2.0), 1.0);
    assert!((fader_to_mul(0.5) - 0.125).abs() < 1e-6);
    for v in [0.0, 0.1, 0.5, 0.9, 1.0] {
        assert!((mul_to_fader(fader_to_mul(v)) - v).abs() < 1e-4);
    }
    assert_eq!(mul_to_fader(4.0), 1.0);
    assert_eq!(mul_to_fader(-1.0), 0.0);
    assert_eq!(format_db(0.0), "-inf dB");
    assert_eq!(format_db(1.0), "0.0 dB");
    assert_eq!(format_db(0.5), "-6.0 dB");
    assert_eq!(format_duration(0), "0:00");
    assert_eq!(format_duration(65_000), "1:05");
    assert_eq!(format_duration(3_600_000 + 61_000), "1:01:01");
}

#[test]
fn resolvers() {
    let s = snap();
    assert_eq!(resolve_scene(&s, &name("Chat")).as_deref(), Some("Chat"));
    assert_eq!(resolve_scene(&s, &name("Nope")), None);
    assert_eq!(resolve_scene(&s, &slot(2)).as_deref(), Some("BRB"));
    assert_eq!(resolve_scene(&s, &slot(3)), None);
    assert_eq!(resolve_input(&s, &special(SpecialInput::Mic1)).unwrap().name, "Mic/Aux");
    assert!(resolve_input(&s, &special(SpecialInput::Mic3)).is_none(), "absent special");
    assert!(resolve_input(&s, &special(SpecialInput::Mic2)).is_none(), "special names a missing input");
    assert!(resolve_input(&s, &inp("Cam")).is_some());
    assert!(resolve_input(&s, &inp("x")).is_none());
}

#[test]
fn not_connected_fails_every_action() {
    let mut s = snap();
    s.status = ObsStatus::Offline;
    let m = MockObs::new(s);
    let actions = vec![
        Action::ObsScene { scene: slot(0), preview: false },
        Action::ObsStream { mode: ObsToggle::Toggle },
        Action::ObsSaveReplay,
        Action::ObsStudioTransition,
        Action::ObsInputMute { input: inp("Mic/Aux"), mode: ObsToggle::Toggle },
        Action::ObsSetInputVolume { input: inp("Mic/Aux"), level: Some(0.5) },
    ];
    for a in actions {
        assert_eq!(run(&m, a), Err("OBS is not connected".into()));
    }
    assert!(m.sent().is_empty());
    assert!(is_obs_action(&Action::ObsSaveReplay));
    assert!(execute(&*m, &Action::ToggleState, &InputContext::default()).is_err());
}

#[test]
fn scene_actions() {
    let m = MockObs::new(snap());
    assert_eq!(run(&m, Action::ObsScene { scene: slot(1), preview: false }), Ok(ActionOutcome::SetState(1)));
    assert_eq!(run(&m, Action::ObsScene { scene: name("BRB"), preview: false }), Ok(ActionOutcome::SetState(1)));
    // preview outside studio mode = program
    assert_eq!(run(&m, Action::ObsScene { scene: slot(0), preview: true }), Ok(ActionOutcome::SetState(1)));
    assert_eq!(
        m.sent(),
        vec![
            ObsCommand::SetProgramScene("Chat".into()),
            ObsCommand::SetProgramScene("BRB".into()),
            ObsCommand::SetProgramScene("Main".into()),
        ]
    );
    assert_eq!(run(&m, Action::ObsScene { scene: slot(4), preview: false }), Err("OBS scene slot 4 is empty".into()));
    assert!(run(&m, Action::ObsScene { scene: name("Nope"), preview: false }).is_err());
    assert_eq!(m.sent().len(), 3);

    m.snap.lock().unwrap().studio_mode = true;
    run(&m, Action::ObsScene { scene: slot(1), preview: true }).unwrap();
    run(&m, Action::ObsScene { scene: slot(2), preview: false }).unwrap();
    assert_eq!(m.sent()[3], ObsCommand::SetPreviewScene("Chat".into()));
    assert_eq!(m.sent()[4], ObsCommand::SetProgramScene("BRB".into()));
}

#[test]
fn output_toggles_and_noops() {
    let mut s = snap();
    s.record = OutputState { active: true, paused: false, duration_ms: 5000 };
    let m = MockObs::new(s);
    use ObsToggle::*;
    assert_eq!(run(&m, Action::ObsStream { mode: Toggle }), Ok(ActionOutcome::SetState(1)));
    assert_eq!(run(&m, Action::ObsStream { mode: Off }), Ok(ActionOutcome::SetState(0)), "off when inactive");
    assert_eq!(run(&m, Action::ObsRecord { mode: On }), Ok(ActionOutcome::SetState(1)), "on when active");
    assert_eq!(run(&m, Action::ObsRecord { mode: Toggle }), Ok(ActionOutcome::SetState(0)));
    assert_eq!(run(&m, Action::ObsReplayBuffer { mode: On }), Ok(ActionOutcome::SetState(1)));
    assert_eq!(run(&m, Action::ObsVirtualCam { mode: On }), Ok(ActionOutcome::SetState(1)));
    assert_eq!(run(&m, Action::ObsStudioMode { mode: Toggle }), Ok(ActionOutcome::SetState(1)));
    assert_eq!(
        m.sent(),
        vec![
            ObsCommand::Stream(true),
            ObsCommand::Record(false),
            ObsCommand::ReplayBuffer(true),
            ObsCommand::VirtualCam(true),
            ObsCommand::SetStudioMode(true),
        ]
    );
}

#[test]
fn record_pause_and_replay_and_transition() {
    let m = MockObs::new(snap());
    assert!(run(&m, Action::ObsRecordPause { mode: ObsToggle::Toggle }).is_err(), "not recording");
    assert_eq!(run(&m, Action::ObsSaveReplay), Err("the replay buffer is off".into()));
    assert_eq!(run(&m, Action::ObsStudioTransition), Err("studio mode is off".into()));
    assert!(m.sent().is_empty());

    {
        let mut s = m.snap.lock().unwrap();
        s.record.active = true;
        s.replay_buffer.active = true;
        s.studio_mode = true;
    }
    assert_eq!(run(&m, Action::ObsRecordPause { mode: ObsToggle::Toggle }), Ok(ActionOutcome::SetState(2)));
    assert_eq!(run(&m, Action::ObsRecordPause { mode: ObsToggle::Off }), Ok(ActionOutcome::SetState(1)));
    m.snap.lock().unwrap().record.paused = true;
    assert_eq!(run(&m, Action::ObsRecordPause { mode: ObsToggle::Toggle }), Ok(ActionOutcome::SetState(1)));
    assert_eq!(run(&m, Action::ObsRecordPause { mode: ObsToggle::On }), Ok(ActionOutcome::SetState(2)));
    assert_eq!(run(&m, Action::ObsSaveReplay), Ok(ActionOutcome::Executed));
    assert_eq!(run(&m, Action::ObsStudioTransition), Ok(ActionOutcome::Executed));
    assert_eq!(
        m.sent(),
        vec![
            ObsCommand::RecordPause(true),
            ObsCommand::RecordPause(false),
            ObsCommand::SaveReplayBuffer,
            ObsCommand::StudioTransition,
        ]
    );
}

#[test]
fn visibility_action() {
    let m = MockObs::new(snap());
    let vis = |scene, source: &str, mode| Action::ObsSourceVisibility { scene, source: source.into(), mode };
    // program scene is Main: Cam enabled -> toggle hides
    assert_eq!(run(&m, vis(None, "Cam", ObsToggle::Toggle)), Ok(ActionOutcome::SetState(0)));
    assert_eq!(run(&m, vis(None, "Logo", ObsToggle::On)), Ok(ActionOutcome::SetState(1)));
    assert_eq!(run(&m, vis(Some(slot(1)), "Cam", ObsToggle::On)), Ok(ActionOutcome::SetState(1)));
    assert_eq!(run(&m, vis(Some(name("Main")), "Cam", ObsToggle::On)), Ok(ActionOutcome::SetState(1)), "no-op");
    assert_eq!(
        m.sent(),
        vec![
            ObsCommand::SetSceneItemEnabled { scene: "Main".into(), item_id: 7, enabled: false },
            ObsCommand::SetSceneItemEnabled { scene: "Main".into(), item_id: 8, enabled: true },
            ObsCommand::SetSceneItemEnabled { scene: "Chat".into(), item_id: 3, enabled: true },
        ]
    );
    assert!(run(&m, vis(None, "Missing", ObsToggle::Toggle)).is_err());
    assert!(run(&m, vis(Some(slot(9)), "Cam", ObsToggle::Toggle)).is_err());
    assert!(run(&m, vis(Some(slot(2)), "Cam", ObsToggle::Toggle)).is_err(), "scene with no items");
}

#[test]
fn mute_action() {
    let m = MockObs::new(snap());
    let mute = |input, mode| Action::ObsInputMute { input, mode };
    assert_eq!(run(&m, mute(special(SpecialInput::Mic1), ObsToggle::Toggle)), Ok(ActionOutcome::SetState(1)));
    assert_eq!(run(&m, mute(inp("Desktop Audio"), ObsToggle::Toggle)), Ok(ActionOutcome::SetState(0)));
    assert_eq!(run(&m, mute(inp("Desktop Audio"), ObsToggle::On)), Ok(ActionOutcome::SetState(1)), "no-op");
    assert_eq!(run(&m, mute(inp("Mic/Aux"), ObsToggle::Off)), Ok(ActionOutcome::SetState(0)), "no-op");
    assert_eq!(
        m.sent(),
        vec![
            ObsCommand::SetInputMute { input: "Mic/Aux".into(), muted: true },
            ObsCommand::SetInputMute { input: "Desktop Audio".into(), muted: false },
        ]
    );
    assert_eq!(run(&m, mute(inp("Nope"), ObsToggle::Toggle)), Err("OBS input 'Nope' not found".into()));
    assert!(run(&m, mute(special(SpecialInput::Mic3), ObsToggle::Toggle)).is_err(), "absent special");
    assert!(run(&m, mute(inp("Cam"), ObsToggle::Toggle)).is_err(), "not an audio input");
}

fn mul_sent(m: &MockObs, i: usize) -> f32 {
    match &m.sent()[i] {
        ObsCommand::SetInputVolumeMul { mul, .. } => *mul,
        c => panic!("unexpected {c:?}"),
    }
}

#[test]
fn volume_actions() {
    let m = MockObs::new(snap());
    let set = |level| Action::ObsSetInputVolume { input: inp("Mic/Aux"), level };
    assert_eq!(run(&m, set(Some(0.5))), Ok(ActionOutcome::SetValue(0.5)));
    assert!((mul_sent(&m, 0) - 0.125).abs() < 1e-6);
    assert_eq!(run(&m, set(Some(3.0))), Ok(ActionOutcome::SetValue(1.0)), "clamped");
    // slider value from ctx when level is None
    let r = execute(&*m, &set(None), &InputContext { delta: 0, value: 0.25, ..Default::default() });
    assert_eq!(r, Ok(ActionOutcome::SetValue(0.25)));
    assert!((mul_sent(&m, 2) - fader_to_mul(0.25)).abs() < 1e-6);
    assert!(run(&m, Action::ObsSetInputVolume { input: inp("Cam"), level: Some(0.5) }).is_err());
    assert!(run(&m, Action::ObsSetInputVolume { input: inp("Nope"), level: Some(0.5) }).is_err());
}

#[test]
fn adjust_volume() {
    // Mic/Aux mul 0.125 -> fader 0.5
    let m = MockObs::new(snap());
    let adj = |step| Action::ObsAdjustInputVolume { input: special(SpecialInput::Mic1), step };
    let ctx = |delta| InputContext { delta, value: 0.0, ..Default::default() };
    // key press (delta 0 = once)
    let Ok(ActionOutcome::SetValue(v)) = execute(&*m, &adj(0.1), &ctx(0)) else { panic!() };
    assert!((v - 0.6).abs() < 1e-4);
    assert!((mul_sent(&m, 0) - fader_to_mul(v)).abs() < 1e-6);
    // dial, 3 detents
    let Ok(ActionOutcome::SetValue(v)) = execute(&*m, &adj(0.1), &ctx(3)) else { panic!() };
    assert!((v - 0.8).abs() < 1e-4);
    // negative delta
    let Ok(ActionOutcome::SetValue(v)) = execute(&*m, &adj(0.1), &ctx(-2)) else { panic!() };
    assert!((v - 0.3).abs() < 1e-4);
    // clamps
    assert_eq!(execute(&*m, &adj(0.1), &ctx(-50)), Ok(ActionOutcome::SetValue(0.0)));
    assert_eq!(execute(&*m, &adj(0.1), &ctx(50)), Ok(ActionOutcome::SetValue(1.0)));
    assert!(execute(&*m, &Action::ObsAdjustInputVolume { input: inp("Cam"), step: 0.1 }, &ctx(1)).is_err());
}

#[test]
fn scene_sources() {
    let mut s = snap();
    s.studio_mode = true;
    s.preview_scene = Some("Chat".into());
    let sc = |scene| DataSource::ObsScene { scene };
    let r = smp(&s, sc(slot(0)));
    assert_eq!((r.state_index, r.title.as_deref(), r.dimmed), (Some(1), Some("Main"), Some(false)));
    let r = smp(&s, sc(slot(1)));
    assert_eq!((r.state_index, r.title.as_deref()), (Some(2), Some("Chat")));
    let r = smp(&s, sc(name("BRB")));
    assert_eq!((r.state_index, r.title, r.dimmed), (Some(0), None, Some(false)));
    // preview == program counts as program (1)
    s.preview_scene = Some("Main".into());
    assert_eq!(smp(&s, sc(name("Main"))).state_index, Some(1));
    // preview ignored outside studio mode
    s.studio_mode = false;
    s.preview_scene = Some("Chat".into());
    assert_eq!(smp(&s, sc(name("Chat"))).state_index, Some(0));
    // unresolved
    let r = smp(&s, sc(slot(5)));
    assert_eq!(r.dimmed, Some(true));
    assert_eq!(r.title.as_deref(), Some(""));
    assert_eq!(r.state_index, Some(0));
    let r = smp(&s, sc(name("Nope")));
    assert_eq!((r.dimmed, r.title), (Some(true), None));
}

#[test]
fn output_sources() {
    let mut s = snap();
    s.stream = OutputState { active: true, paused: false, duration_ms: 125_000 };
    s.record = OutputState { active: true, paused: true, duration_ms: 3_725_000 };
    s.virtual_cam.active = true;
    s.studio_mode = true;
    let r = smp(&s, DataSource::ObsStream);
    assert_eq!((r.state_index, r.subtitle.as_deref(), r.dimmed), (Some(1), Some("2:05"), Some(false)));
    let r = smp(&s, DataSource::ObsRecord);
    assert_eq!((r.state_index, r.subtitle.as_deref()), (Some(2), Some("1:02:05")));
    s.record.paused = false;
    assert_eq!(smp(&s, DataSource::ObsRecord).state_index, Some(1));
    s.record = OutputState::default();
    let r = smp(&s, DataSource::ObsRecord);
    assert_eq!((r.state_index, r.subtitle.as_deref()), (Some(0), Some("")));
    assert_eq!(smp(&s, DataSource::ObsReplayBuffer).state_index, Some(0));
    assert_eq!(smp(&s, DataSource::ObsVirtualCam).state_index, Some(1));
    assert_eq!(smp(&s, DataSource::ObsStudioMode).state_index, Some(1));
    s.replay_buffer.active = true;
    assert_eq!(smp(&s, DataSource::ObsReplayBuffer).state_index, Some(1));
}

#[test]
fn visibility_and_input_sources() {
    let s = snap();
    let vis = |scene, source: &str| DataSource::ObsSourceVisibility { scene, source: source.into() };
    assert_eq!(smp(&s, vis(None, "Cam")).state_index, Some(1));
    assert_eq!(smp(&s, vis(None, "Logo")).state_index, Some(0));
    assert_eq!(smp(&s, vis(None, "Logo")).dimmed, Some(false));
    assert_eq!(smp(&s, vis(Some(slot(1)), "Cam")).state_index, Some(0));
    assert_eq!(smp(&s, vis(None, "Nope")).dimmed, Some(true));
    assert_eq!(smp(&s, vis(Some(slot(8)), "Cam")).dimmed, Some(true));

    let r = smp(&s, DataSource::ObsInput { input: special(SpecialInput::Mic1) });
    assert!((r.value.unwrap() - 0.5).abs() < 1e-4);
    assert_eq!((r.subtitle.as_deref(), r.state_index, r.dimmed), (Some("-18.1 dB"), Some(0), Some(false)));
    let r = smp(&s, DataSource::ObsInput { input: inp("Desktop Audio") });
    assert_eq!((r.value, r.subtitle.as_deref(), r.state_index), (Some(1.0), Some("0.0 dB"), Some(1)));
    for i in [special(SpecialInput::Mic4), inp("Cam"), inp("zzz")] {
        let r = smp(&s, DataSource::ObsInput { input: i });
        assert_eq!((r.dimmed, r.value, r.state_index), (Some(true), Some(0.0), Some(0)));
    }
}

#[test]
fn offline_sources_dim_but_status_does_not() {
    let mut s = snap();
    s.status = ObsStatus::Offline;
    let all = [
        DataSource::ObsScene { scene: name("Main") },
        DataSource::ObsStream,
        DataSource::ObsRecord,
        DataSource::ObsReplayBuffer,
        DataSource::ObsVirtualCam,
        DataSource::ObsStudioMode,
        DataSource::ObsSourceVisibility { scene: None, source: "Cam".into() },
        DataSource::ObsInput { input: inp("Mic/Aux") },
    ];
    for src in all {
        assert!(is_obs_source(&src));
        let r = smp(&s, src);
        assert_eq!((r.dimmed, r.state_index, r.value), (Some(true), Some(0), Some(0.0)));
        assert_eq!(r.subtitle.as_deref(), Some(""));
        assert_eq!(r.title, None);
    }
    assert_eq!(smp(&s, DataSource::ObsScene { scene: slot(0) }).title.as_deref(), Some(""));
    assert!(!is_obs_source(&DataSource::Cpu));
    assert_eq!(smp(&s, DataSource::Cpu), crate::live::LiveSample::default());
}

#[test]
fn status_source() {
    let mut s = snap();
    let cases = [
        (ObsStatus::Disabled, 0, "disabled"),
        (ObsStatus::Connecting, 0, "connecting"),
        (ObsStatus::AuthFailed, 0, "wrong password"),
        (ObsStatus::ServerDisabled, 0, "server off"),
        (ObsStatus::Offline, 0, "offline"),
        (ObsStatus::Connected, 1, ""),
    ];
    for (st, state, sub) in cases {
        s.status = st;
        let r = smp(&s, DataSource::ObsStatus);
        assert_eq!((r.state_index, r.subtitle.as_deref(), r.dimmed), (Some(state), Some(sub), Some(false)));
    }
}

#[test]
fn router_integration() {
    let m = MockObs::new(snap());
    let router =
        ActionRouter::new(Arc::new(MockInputInjector::new()), Arc::new(MockLauncher::new())).with_obs(m.clone());
    let out = router.execute(&Action::ObsStream { mode: ObsToggle::Toggle }, &InputContext::default()).unwrap();
    assert_eq!(out, ActionOutcome::SetState(1));
    assert_eq!(m.sent(), vec![ObsCommand::Stream(true)]);
    let err =
        router.execute(&Action::ObsScene { scene: slot(9), preview: false }, &InputContext::default()).unwrap_err();
    assert!(matches!(err, CoreError::Obs(ref e) if e == "OBS scene slot 9 is empty"));
    // default router: NullObs fails
    let plain = ActionRouter::new(Arc::new(MockInputInjector::new()), Arc::new(MockLauncher::new()));
    let err = plain.execute(&Action::ObsSaveReplay, &InputContext::default()).unwrap_err();
    assert!(matches!(err, CoreError::Obs(_)));
}

#[test]
fn sample_all_uses_obs_provider() {
    let m = MockObs::new(snap());
    let p = LiveProviders {
        audio: Arc::new(nd_platform::NullAudio),
        media: Arc::new(nd_platform::NullMedia),
        stats: Arc::new(nd_platform::NullStats),
        camera: Arc::new(crate::NullCamera),
        mic: Arc::new(crate::NullMic),
        profile_actions: Arc::new(crate::autoswitch::NullProfileActions),
        mixer: Arc::new(crate::mixer::Mixer::new(
            Arc::new(nd_platform::NullAppAudio),
            Arc::new(nd_platform::NullAudio),
        )),
        obs: m,
        icons: None,
    };
    let src = vec![
        ("a".to_string(), DataSource::ObsScene { scene: slot(1) }),
        ("b".to_string(), DataSource::ObsStatus),
        ("c".to_string(), DataSource::ObsInput { input: special(SpecialInput::Mic1) }),
    ];
    let out = sample_all(&src, &p);
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].1.title.as_deref(), Some("Chat"));
    assert_eq!(out[1].1.state_index, Some(1));
    assert_eq!(out[2].1.subtitle.as_deref(), Some("-18.1 dB"));

    let null = LiveProviders { obs: Arc::new(NullObs), ..p };
    let out = sample_all(&src, &null);
    assert_eq!(out[0].1.dimmed, Some(true));
    assert_eq!(out[1].1.subtitle.as_deref(), Some("disabled"));
}

#[test]
fn serde_actions() {
    let cases: Vec<(&str, Action)> = vec![
        (
            r#"{"type":"obs_scene","scene":{"by":"slot","index":0}}"#,
            Action::ObsScene { scene: slot(0), preview: false },
        ),
        (
            r#"{"type":"obs_scene","scene":{"by":"name","name":"Main"},"preview":true}"#,
            Action::ObsScene { scene: name("Main"), preview: true },
        ),
        (
            r#"{"type":"obs_source_visibility","scene":{"by":"slot","index":1},"source":"Cam","mode":"on"}"#,
            Action::ObsSourceVisibility { scene: Some(slot(1)), source: "Cam".into(), mode: ObsToggle::On },
        ),
        (
            r#"{"type":"obs_source_visibility","source":"Cam"}"#,
            Action::ObsSourceVisibility { scene: None, source: "Cam".into(), mode: ObsToggle::Toggle },
        ),
        (r#"{"type":"obs_stream"}"#, Action::ObsStream { mode: ObsToggle::Toggle }),
        (r#"{"type":"obs_record","mode":"off"}"#, Action::ObsRecord { mode: ObsToggle::Off }),
        (r#"{"type":"obs_record_pause"}"#, Action::ObsRecordPause { mode: ObsToggle::Toggle }),
        (r#"{"type":"obs_replay_buffer","mode":"on"}"#, Action::ObsReplayBuffer { mode: ObsToggle::On }),
        (r#"{"type":"obs_save_replay"}"#, Action::ObsSaveReplay),
        (r#"{"type":"obs_virtual_cam"}"#, Action::ObsVirtualCam { mode: ObsToggle::Toggle }),
        (r#"{"type":"obs_studio_mode"}"#, Action::ObsStudioMode { mode: ObsToggle::Toggle }),
        (r#"{"type":"obs_studio_transition"}"#, Action::ObsStudioTransition),
        (
            r#"{"type":"obs_input_mute","input":{"by":"special","which":"mic1"}}"#,
            Action::ObsInputMute { input: special(SpecialInput::Mic1), mode: ObsToggle::Toggle },
        ),
        (
            r#"{"type":"obs_set_input_volume","input":{"by":"name","name":"Mic/Aux"},"level":0.5}"#,
            Action::ObsSetInputVolume { input: inp("Mic/Aux"), level: Some(0.5) },
        ),
        (
            r#"{"type":"obs_set_input_volume","input":{"by":"special","which":"desktop1"}}"#,
            Action::ObsSetInputVolume { input: special(SpecialInput::Desktop1), level: None },
        ),
        (
            r#"{"type":"obs_adjust_input_volume","input":{"by":"special","which":"mic2"},"step":-0.05}"#,
            Action::ObsAdjustInputVolume { input: special(SpecialInput::Mic2), step: -0.05 },
        ),
    ];
    for (json, expected) in cases {
        let parsed: Action = serde_json::from_str(json).unwrap_or_else(|e| panic!("{json}: {e}"));
        assert_eq!(parsed, expected, "{json}");
        // Round trip through the serializer.
        let back: Action = serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(back, expected);
    }
    // defaults are omitted when serialising
    let j = serde_json::to_value(Action::ObsScene { scene: slot(0), preview: false }).unwrap();
    assert!(j.get("preview").is_none());
    assert_eq!(j["type"], "obs_scene");
}

#[test]
fn serde_sources() {
    let cases: Vec<(&str, DataSource)> = vec![
        (r#"{"type":"obs_scene","scene":{"by":"slot","index":2}}"#, DataSource::ObsScene { scene: slot(2) }),
        (r#"{"type":"obs_stream"}"#, DataSource::ObsStream),
        (r#"{"type":"obs_record"}"#, DataSource::ObsRecord),
        (r#"{"type":"obs_replay_buffer"}"#, DataSource::ObsReplayBuffer),
        (r#"{"type":"obs_virtual_cam"}"#, DataSource::ObsVirtualCam),
        (r#"{"type":"obs_studio_mode"}"#, DataSource::ObsStudioMode),
        (
            r#"{"type":"obs_source_visibility","source":"Cam"}"#,
            DataSource::ObsSourceVisibility { scene: None, source: "Cam".into() },
        ),
        (
            r#"{"type":"obs_input","input":{"by":"name","name":"Mic/Aux"}}"#,
            DataSource::ObsInput { input: inp("Mic/Aux") },
        ),
        (r#"{"type":"obs_status"}"#, DataSource::ObsStatus),
    ];
    for (json, expected) in cases {
        let parsed: DataSource = serde_json::from_str(json).unwrap_or_else(|e| panic!("{json}: {e}"));
        assert_eq!(parsed, expected, "{json}");
        let back: DataSource = serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(back, expected);
    }
}
