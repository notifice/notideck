// SPDX-License-Identifier: GPL-3.0-or-later
//! In-process fake obs-websocket v5 server (implemented in R14-1) for the client tests, the agent e2e tests and
//! `NOTIDECK_FAKE_OBS=1` (a demo OBS with three scenes, Mic/Aux + Desktop Audio, a hideable "Webcam" source).
//!
//! Supports: Hello/Identify (optional password auth), Request + RequestBatch for every request the client
//! sends (see `client.rs`), and emits the matching events after state-changing requests, so a client sees
//! the same flow as with real OBS. State is inspectable and mutable from tests (to simulate OBS-side changes,
//! which also emit events).

use crate::util::{auth_string, out_mut, random_alnum, Clock, OUT_COUNT, SPECIALS};
use futures_util::{SinkExt, StreamExt};
use nd_core::obs::{ObsInputInfo, ObsSceneItem, ObsSnapshot, ObsStatus, SpecialInput};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Notify};
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};
use tokio_tungstenite::tungstenite::Message;

/// (event type, event data)
type Ev = (&'static str, Value);
/// (obs-websocket result code, comment)
type ReqErr = (i64, String);

const OUT_EVENTS: [&str; OUT_COUNT] =
    ["StreamStateChanged", "RecordStateChanged", "ReplayBufferStateChanged", "VirtualcamStateChanged"];
const STARTED: &str = "OBS_WEBSOCKET_OUTPUT_STARTED";
const STOPPED: &str = "OBS_WEBSOCKET_OUTPUT_STOPPED";
const PAUSED: &str = "OBS_WEBSOCKET_OUTPUT_PAUSED";
const RESUMED: &str = "OBS_WEBSOCKET_OUTPUT_RESUMED";

fn err<T>(code: i64, msg: impl Into<String>) -> Result<T, ReqErr> {
    Err((code, msg.into()))
}

fn field<'a>(d: &'a Value, k: &str) -> Result<&'a Value, ReqErr> {
    match d.get(k) {
        Some(v) if !v.is_null() => Ok(v),
        _ => err(300, format!("missing request field {k}")),
    }
}

fn str_field<'a>(d: &'a Value, k: &str) -> Result<&'a str, ReqErr> {
    field(d, k)?.as_str().map_or_else(|| err(400, format!("{k} must be a string")), Ok)
}

fn bool_field(d: &Value, k: &str) -> Result<bool, ReqErr> {
    field(d, k)?.as_bool().map_or_else(|| err(400, format!("{k} must be a boolean")), Ok)
}

fn db(mul: f32) -> f64 {
    if mul <= 0.0 {
        -100.0
    } else {
        20.0 * (mul as f64).log10()
    }
}

/// The fake OBS: a snapshot plus the output clocks.
struct Fake {
    snap: ObsSnapshot,
    clocks: [Clock; OUT_COUNT],
}

impl Fake {
    fn demo() -> Fake {
        let audio = |name: &str, kind: &str| ObsInputInfo {
            name: name.into(),
            kind: kind.into(),
            muted: Some(false),
            volume_mul: Some(1.0),
        };
        let item = |source: &str, item_id: i64| ObsSceneItem { source: source.into(), item_id, enabled: true };
        let snap = ObsSnapshot {
            status: ObsStatus::Connected,
            program_scene: Some("Main".into()),
            scenes: vec!["Main".into(), "BRB".into(), "Starting Soon".into()],
            inputs: vec![
                audio("Mic/Aux", "wasapi_input_capture"),
                audio("Desktop Audio", "wasapi_output_capture"),
                ObsInputInfo { name: "Webcam".into(), kind: "dshow_input".into(), ..Default::default() },
            ],
            special_inputs: [
                (SpecialInput::Desktop1, "Desktop Audio".to_string()),
                (SpecialInput::Mic1, "Mic/Aux".to_string()),
            ]
            .into(),
            scene_items: [
                ("Main".to_string(), vec![item("Webcam", 1), item("Display Capture", 2)]),
                ("BRB".to_string(), vec![item("BRB Text", 3)]),
                ("Starting Soon".to_string(), vec![item("Starting Text", 4)]),
            ]
            .into(),
            ..Default::default()
        };
        Fake { snap, clocks: Default::default() }
    }

    fn state(&self) -> ObsSnapshot {
        let mut s = self.snap.clone();
        for i in 0..OUT_COUNT {
            let o = out_mut(&mut s, i);
            o.duration_ms = if o.active { self.clocks[i].ms() } else { 0 };
        }
        s
    }

    /// Move output `i` to (`active`, `paused`), updating its clock and emitting the events.
    fn transition(&mut self, i: usize, active: bool, paused: bool, ev: &mut Vec<Ev>) {
        let o = out_mut(&mut self.snap, i);
        let out = |state: &str, active: bool| json!({"outputActive": active, "outputState": state});
        if active && !o.active {
            o.active = true;
            self.clocks[i].start_from(0);
            ev.push((OUT_EVENTS[i], out(STARTED, true)));
        } else if !active && o.active {
            *o = Default::default();
            self.clocks[i].stop();
            ev.push((OUT_EVENTS[i], out(STOPPED, false)));
        }
        if i == 1 && o.active && paused != o.paused {
            o.paused = paused;
            if paused {
                self.clocks[i].pause();
            } else {
                self.clocks[i].resume();
            }
            ev.push((OUT_EVENTS[i], out(if paused { PAUSED } else { RESUMED }, true)));
        }
    }

    fn scene_exists(&self, name: &str) -> Result<(), ReqErr> {
        if self.snap.scenes.iter().any(|s| s == name) {
            Ok(())
        } else {
            err(600, format!("No scene was found by the name of `{name}`."))
        }
    }

    fn input_mut(&mut self, name: &str) -> Result<&mut ObsInputInfo, ReqErr> {
        match self.snap.inputs.iter_mut().find(|i| i.name == name) {
            Some(i) => Ok(i),
            None => err(600, format!("No input was found by the name of `{name}`.")),
        }
    }

    fn require_studio(&self) -> Result<(), ReqErr> {
        if self.snap.studio_mode {
            Ok(())
        } else {
            err(506, "Studio mode is not active.")
        }
    }

    fn set_program(&mut self, name: &str, ev: &mut Vec<Ev>) {
        self.snap.program_scene = Some(name.into());
        ev.push(("CurrentProgramSceneChanged", json!({"sceneName": name})));
    }

    fn set_preview(&mut self, name: &str, ev: &mut Vec<Ev>) {
        self.snap.preview_scene = Some(name.into());
        ev.push(("CurrentPreviewSceneChanged", json!({"sceneName": name})));
    }

    fn output_request(&mut self, ty: &str, ev: &mut Vec<Ev>) -> Option<Result<Value, ReqErr>> {
        let (i, start) = match ty {
            "StartStream" => (0, true),
            "StopStream" => (0, false),
            "StartRecord" => (1, true),
            "StopRecord" => (1, false),
            "StartReplayBuffer" => (2, true),
            "StopReplayBuffer" => (2, false),
            "StartVirtualCam" => (3, true),
            "StopVirtualCam" => (3, false),
            _ => return None,
        };
        let active = out_mut(&mut self.snap, i).active;
        if start == active {
            return Some(if start {
                err(500, "The output is already running.")
            } else {
                err(501, "The output is not running.")
            });
        }
        self.transition(i, start, false, ev);
        Some(Ok(json!({})))
    }

    fn apply(&mut self, ty: &str, d: &Value, ev: &mut Vec<Ev>) -> Result<Value, ReqErr> {
        if let Some(r) = self.output_request(ty, ev) {
            return r;
        }
        let status = |v: &ObsSnapshot, i: usize| {
            let mut v = v.clone();
            let o = *out_mut(&mut v, i);
            let ms = if o.active { self.clocks[i].ms() } else { 0 };
            json!({"outputActive": o.active, "outputPaused": o.paused, "outputDuration": ms,
                   "outputTimecode": "00:00:00.000", "outputBytes": 0})
        };
        match ty {
            "GetVersion" => {
                Ok(json!({"obsVersion": "30.2.0-fake", "obsWebSocketVersion": "5.5.0-fake", "rpcVersion": 1}))
            }
            "GetStudioModeEnabled" => Ok(json!({"studioModeEnabled": self.snap.studio_mode})),
            "GetSceneList" => {
                let n = self.snap.scenes.len();
                // OBS lists scenes bottom to top: index 0 is the bottom one.
                let scenes: Vec<Value> = self
                    .snap
                    .scenes
                    .iter()
                    .enumerate()
                    .rev()
                    .map(|(pos, name)| json!({"sceneName": name, "sceneIndex": n - 1 - pos}))
                    .collect();
                let preview = self.snap.preview_scene.clone().filter(|_| self.snap.studio_mode);
                Ok(json!({"currentProgramSceneName": self.snap.program_scene,
                          "currentPreviewSceneName": preview, "scenes": scenes}))
            }
            "SetCurrentProgramScene" => {
                let name = str_field(d, "sceneName")?;
                self.scene_exists(name)?;
                self.set_program(name, ev);
                Ok(json!({}))
            }
            "SetCurrentPreviewScene" => {
                let name = str_field(d, "sceneName")?;
                self.require_studio()?;
                self.scene_exists(name)?;
                self.set_preview(name, ev);
                Ok(json!({}))
            }
            "SetStudioModeEnabled" => {
                let on = bool_field(d, "studioModeEnabled")?;
                if on != self.snap.studio_mode {
                    self.snap.studio_mode = on;
                    ev.push(("StudioModeStateChanged", json!({"studioModeEnabled": on})));
                    if on {
                        let p = self.snap.program_scene.clone().unwrap_or_default();
                        self.set_preview(&p, ev);
                    } else {
                        self.snap.preview_scene = None;
                    }
                }
                Ok(json!({}))
            }
            "TriggerStudioModeTransition" => {
                self.require_studio()?;
                let (prog, prev) = (self.snap.program_scene.clone(), self.snap.preview_scene.clone());
                if let (Some(a), Some(b)) = (prog, prev) {
                    self.set_program(&b, ev);
                    self.set_preview(&a, ev);
                }
                Ok(json!({}))
            }
            "GetStreamStatus" => Ok(status(&self.snap, 0)),
            "GetRecordStatus" => Ok(status(&self.snap, 1)),
            "GetReplayBufferStatus" => Ok(status(&self.snap, 2)),
            "GetVirtualCamStatus" => Ok(status(&self.snap, 3)),
            "PauseRecord" | "ResumeRecord" => {
                if !self.snap.record.active {
                    return err(501, "The record output is not running.");
                }
                self.transition(1, true, ty == "PauseRecord", ev);
                Ok(json!({}))
            }
            "SaveReplayBuffer" => {
                if !self.snap.replay_buffer.active {
                    return err(501, "The replay buffer is not running.");
                }
                ev.push(("ReplayBufferSaved", json!({"savedReplayPath": "/tmp/fake-replay.mkv"})));
                Ok(json!({}))
            }
            "GetInputList" => {
                let inputs: Vec<Value> = self
                    .snap
                    .inputs
                    .iter()
                    .map(|i| json!({"inputName": i.name, "inputKind": i.kind, "unversionedInputKind": i.kind}))
                    .collect();
                Ok(json!({"inputs": inputs}))
            }
            "GetSpecialInputs" => {
                let m: serde_json::Map<String, Value> =
                    SPECIALS.iter().map(|k| (k.ws_key().to_string(), json!(self.snap.special_inputs.get(k)))).collect();
                Ok(Value::Object(m))
            }
            "GetInputMute" => {
                let i = self.input_mut(str_field(d, "inputName")?)?;
                // OBS answers with an error for inputs without audio.
                i.muted.map_or_else(|| err(604, "The input has no audio."), |m| Ok(json!({"inputMuted": m})))
            }
            "GetInputVolume" => {
                let i = self.input_mut(str_field(d, "inputName")?)?;
                let m = i.volume_mul.ok_or((604, "The input has no audio.".to_string()))?;
                Ok(json!({"inputVolumeMul": m, "inputVolumeDb": db(m)}))
            }
            "SetInputMute" => {
                let (name, muted) = (str_field(d, "inputName")?, bool_field(d, "inputMuted")?);
                let i = self.input_mut(name)?;
                if i.muted.is_none() {
                    return err(604, "The input has no audio.");
                }
                i.muted = Some(muted);
                ev.push(("InputMuteStateChanged", json!({"inputName": name, "inputMuted": muted})));
                Ok(json!({}))
            }
            "SetInputVolume" => {
                let name = str_field(d, "inputName")?;
                let mul = match (
                    d.get("inputVolumeMul").and_then(Value::as_f64),
                    d.get("inputVolumeDb").and_then(Value::as_f64),
                ) {
                    (Some(m), _) => m as f32,
                    (None, Some(db)) => 10f32.powf(db as f32 / 20.0),
                    _ => return err(300, "missing request field inputVolumeMul"),
                };
                let i = self.input_mut(name)?;
                if i.volume_mul.is_none() {
                    return err(604, "The input has no audio.");
                }
                i.volume_mul = Some(mul);
                ev.push((
                    "InputVolumeChanged",
                    json!({"inputName": name, "inputVolumeMul": mul, "inputVolumeDb": db(mul)}),
                ));
                Ok(json!({}))
            }
            "GetSceneItemList" => {
                let scene = str_field(d, "sceneName")?;
                self.scene_exists(scene)?;
                let items = self.snap.scene_items.get(scene).map(Vec::as_slice).unwrap_or_default();
                // Bottom to top, like OBS: the first item of our list (top) has the highest index.
                let list: Vec<Value> = items
                    .iter()
                    .enumerate()
                    .rev()
                    .map(|(pos, it)| {
                        json!({"sceneItemId": it.item_id, "sourceName": it.source, "sceneItemEnabled": it.enabled,
                               "sceneItemIndex": items.len() - 1 - pos})
                    })
                    .collect();
                Ok(json!({"sceneItems": list}))
            }
            "SetSceneItemEnabled" => {
                let scene = str_field(d, "sceneName")?;
                let id = field(d, "sceneItemId")?.as_i64().unwrap_or(-1);
                let enabled = bool_field(d, "sceneItemEnabled")?;
                self.scene_exists(scene)?;
                let it = self.snap.scene_items.get_mut(scene).and_then(|v| v.iter_mut().find(|i| i.item_id == id));
                let Some(it) = it else { return err(600, "No scene item was found.") };
                it.enabled = enabled;
                ev.push((
                    "SceneItemEnableStateChanged",
                    json!({"sceneName": scene, "sceneItemId": id, "sceneItemEnabled": enabled}),
                ));
                Ok(json!({}))
            }
            _ => err(204, format!("Unknown request type `{ty}`.")),
        }
    }

    /// Replace the state with an edited copy, emitting events for every difference.
    fn replace(&mut self, mut new: ObsSnapshot, ev: &mut Vec<Ev>) {
        new.status = ObsStatus::Connected;
        let mut old = std::mem::replace(&mut self.snap, new);
        let wanted: Vec<_> = (0..OUT_COUNT).map(|i| *out_mut(&mut self.snap, i)).collect();
        for (i, w) in wanted.iter().enumerate() {
            *out_mut(&mut self.snap, i) = *out_mut(&mut old, i);
            self.transition(i, w.active, w.paused, ev);
        }
        let new = &self.snap;
        if old.studio_mode != new.studio_mode {
            ev.push(("StudioModeStateChanged", json!({"studioModeEnabled": new.studio_mode})));
        }
        if old.program_scene != new.program_scene {
            ev.push(("CurrentProgramSceneChanged", json!({"sceneName": new.program_scene})));
        }
        if old.preview_scene != new.preview_scene && new.preview_scene.is_some() {
            ev.push(("CurrentPreviewSceneChanged", json!({"sceneName": new.preview_scene})));
        }
        if old.scenes != new.scenes {
            let n = new.scenes.len();
            let scenes: Vec<Value> = new
                .scenes
                .iter()
                .enumerate()
                .rev()
                .map(|(pos, name)| json!({"sceneName": name, "sceneIndex": n - 1 - pos}))
                .collect();
            ev.push(("SceneListChanged", json!({"scenes": scenes})));
        }
        for i in &new.inputs {
            match old.inputs.iter().find(|o| o.name == i.name) {
                None => ev.push(("InputCreated", json!({"inputName": i.name, "inputKind": i.kind}))),
                Some(o) => {
                    if o.muted != i.muted {
                        ev.push(("InputMuteStateChanged", json!({"inputName": i.name, "inputMuted": i.muted})));
                    }
                    if o.volume_mul != i.volume_mul {
                        let m = i.volume_mul.unwrap_or(0.0);
                        let d = json!({"inputName": i.name, "inputVolumeMul": m, "inputVolumeDb": db(m)});
                        ev.push(("InputVolumeChanged", d));
                    }
                }
            }
        }
        for o in old.inputs.iter().filter(|o| !new.inputs.iter().any(|i| i.name == o.name)) {
            ev.push(("InputRemoved", json!({"inputName": o.name})));
        }
        for (scene, items) in &new.scene_items {
            let Some(before) = old.scene_items.get(scene).filter(|b| *b != items) else { continue };
            let same_shape = before.len() == items.len()
                && before.iter().zip(items).all(|(a, b)| a.item_id == b.item_id && a.source == b.source);
            if same_shape {
                for (a, b) in before.iter().zip(items).filter(|(a, b)| a.enabled != b.enabled) {
                    let d = json!({"sceneName": scene, "sceneItemId": a.item_id, "sceneItemEnabled": b.enabled});
                    ev.push(("SceneItemEnableStateChanged", d));
                }
            } else {
                ev.push(("SceneItemListReindexed", json!({"sceneName": scene})));
            }
        }
    }
}

struct Shared {
    fake: Mutex<Fake>,
    requests: Mutex<Vec<String>>,
    /// Full `{op: 5, d}` messages for identified clients.
    events: broadcast::Sender<Value>,
    password: Option<String>,
}

impl Shared {
    fn fake(&self) -> MutexGuard<'_, Fake> {
        self.fake.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, evs: Vec<Ev>) {
        for (ty, data) in evs {
            self.emit_one(ty, data);
        }
    }

    fn emit_one(&self, ty: &str, data: Value) {
        let _ = self.events.send(json!({"op": 5, "d": {"eventType": ty, "eventIntent": 1, "eventData": data}}));
    }

    fn handle(&self, ty: &str, data: &Value) -> Result<Value, ReqErr> {
        self.requests.lock().unwrap_or_else(|e| e.into_inner()).push(ty.to_string());
        let mut ev = Vec::new();
        let mut fake = self.fake();
        let r = fake.apply(ty, data, &mut ev);
        self.emit(ev); // under the state lock, so events keep the order of the changes
        drop(fake);
        r
    }
}

fn response(ty: &str, id: &Value, r: Result<Value, ReqErr>) -> Value {
    let (status, data) = match r {
        Ok(d) => (json!({"result": true, "code": 100}), d),
        Err((code, comment)) => (json!({"result": false, "code": code, "comment": comment}), Value::Null),
    };
    json!({"requestType": ty, "requestId": id, "requestStatus": status, "responseData": data})
}

/// An in-process fake OBS (see the module doc).
pub struct FakeObs {
    sh: Arc<Shared>,
    addr: SocketAddr,
    stop: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Drop for FakeObs {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeObs {
    /// Listen on 127.0.0.1:`port` (0 = any free port) with the demo state. `password` None = no auth.
    pub async fn start(port: u16, password: Option<&str>) -> std::io::Result<FakeObs> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let addr = listener.local_addr()?;
        let sh = Arc::new(Shared {
            fake: Mutex::new(Fake::demo()),
            requests: Mutex::new(Vec::new()),
            events: broadcast::channel(256).0,
            password: password.map(String::from),
        });
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(accept_loop(listener, sh.clone(), stop.clone()));
        Ok(FakeObs { sh, addr, stop, task })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The fake's current state, in snapshot form (status `Connected`; durations 0 except while active).
    pub fn state(&self) -> ObsSnapshot {
        self.sh.fake().state()
    }

    /// Change state as if the user did it in OBS (emits the events). `f` edits the snapshot; the fake diffs it.
    pub fn mutate(&self, f: impl FnOnce(&mut ObsSnapshot)) {
        let mut ev = Vec::new();
        let mut fake = self.sh.fake();
        let mut snap = fake.state();
        f(&mut snap);
        fake.replace(snap, &mut ev);
        self.sh.emit(ev);
    }

    /// Names of all requests received so far, in order (`"SetCurrentProgramScene"`, …).
    pub fn requests(&self) -> Vec<String> {
        self.sh.requests.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Close all client connections (simulates OBS quitting: sends ExitStarted first) and stop listening.
    pub async fn shutdown(mut self) {
        self.sh.emit_one("ExitStarted", json!({}));
        self.stop.notify_one();
        let _ = (&mut self.task).await;
    }
}

async fn accept_loop(listener: TcpListener, sh: Arc<Shared>, stop: Arc<Notify>) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            a = listener.accept() => match a {
                Ok((stream, _)) => { conns.spawn(serve(stream, sh.clone())); }
                Err(_) => break,
            },
            _ = stop.notified() => break,
        }
    }
    drop(listener); // release the port now
    let drain = async { while conns.join_next().await.is_some() {} };
    let _ = tokio::time::timeout(Duration::from_secs(1), drain).await;
}

async fn serve(stream: TcpStream, sh: Arc<Shared>) {
    #[allow(clippy::result_large_err)] // the signature is dictated by tungstenite's Callback
    let cb = |req: &Request, mut resp: Response| -> Result<Response, ErrorResponse> {
        if let Some(p) = req.headers().get("Sec-WebSocket-Protocol") {
            resp.headers_mut().insert("Sec-WebSocket-Protocol", p.clone());
        }
        Ok(resp)
    };
    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, cb).await else { return };
    let mut rx = sh.events.subscribe();

    // Hello / Identify.
    let (salt, challenge) = (random_alnum(24), random_alnum(24));
    let mut hello = json!({"obsWebSocketVersion": "5.5.0-fake", "rpcVersion": 1});
    if sh.password.is_some() {
        hello["authentication"] = json!({"challenge": challenge, "salt": salt});
    }
    if ws.send(Message::Text(json!({"op": 0, "d": hello}).to_string())).await.is_err() {
        return;
    }
    let identify = async {
        loop {
            match ws.next().await? {
                Ok(Message::Text(t)) => {
                    let v: Value = serde_json::from_str(&t).ok()?;
                    if v["op"] == 1 {
                        return Some(v["d"].clone());
                    }
                }
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    };
    let Ok(Some(d)) = tokio::time::timeout(Duration::from_secs(5), identify).await else { return };
    let ok = match &sh.password {
        None => true,
        Some(pw) => d["authentication"].as_str() == Some(auth_string(pw, &salt, &challenge).as_str()),
    };
    if !ok {
        let frame = CloseFrame { code: CloseCode::Library(4009), reason: "Authentication failed.".into() };
        let _ = ws.send(Message::Close(Some(frame))).await;
        // Let the client read the close frame before the socket goes away.
        let _ = tokio::time::timeout(Duration::from_millis(500), ws.next()).await;
        return;
    }
    let identified = json!({"op": 2, "d": {"negotiatedRpcVersion": 1}});
    if ws.send(Message::Text(identified.to_string())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            m = ws.next() => {
                let Some(Ok(m)) = m else { return };
                let reply = match m {
                    Message::Text(t) => serde_json::from_str::<Value>(&t).ok().and_then(|v| handle_message(&sh, &v)),
                    Message::Close(_) => return,
                    _ => None,
                };
                if let Some(r) = reply {
                    if ws.send(Message::Text(r.to_string())).await.is_err() {
                        return;
                    }
                }
            }
            e = rx.recv() => match e {
                Ok(v) => {
                    let exit = v["d"]["eventType"] == "ExitStarted";
                    if ws.send(Message::Text(v.to_string())).await.is_err() {
                        return;
                    }
                    if exit {
                        let _ = ws.close(None).await;
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

/// Answer a Request (6) or RequestBatch (8).
fn handle_message(sh: &Shared, v: &Value) -> Option<Value> {
    let d = &v["d"];
    match v["op"].as_u64()? {
        6 => {
            let ty = d["requestType"].as_str().unwrap_or_default();
            let r = sh.handle(ty, &d["requestData"]);
            Some(json!({"op": 7, "d": response(ty, &d["requestId"], r)}))
        }
        8 => {
            let results: Vec<Value> = d["requests"]
                .as_array()?
                .iter()
                .map(|q| {
                    let ty = q["requestType"].as_str().unwrap_or_default();
                    response(ty, &q["requestId"], sh.handle(ty, &q["requestData"]))
                })
                .collect();
            Some(json!({"op": 9, "d": {"requestId": d["requestId"], "results": results}}))
        }
        _ => None,
    }
}
