// SPDX-License-Identifier: GPL-3.0-or-later
//! obs-websocket v5 client (implemented in R14-1).
//!
//! # Behaviour
//! - [`ObsClient::start`] spawns one tokio task (call it inside a runtime). The task loops:
//!   resolve the endpoint → connect `ws://host:port` (subprotocol `obswebsocket.json`) → Hello/Identify
//!   (rpcVersion 1, auth = base64(sha256(base64(sha256(password + salt)) + challenge)) when Hello has
//!   `authentication`) → initial state requests → event loop. On any failure/close: status updated, wait
//!   (1 s, doubling to 5 s max; reset after a successful connect), retry. `enabled = false` → status
//!   `Disabled`, no connection, wakes up on [`ObsClient::update_settings`].
//! - Endpoint: with `auto_detect` and a detected OBS config ([`crate::detect::read`]) → `127.0.0.1` + its
//!   `server_port` + its `server_password` (if `auth_required`); a detected config with
//!   `server_enabled = false` → status `ServerDisabled` (keep re-reading it every retry). Otherwise
//!   `host`/`port`/`password` from the settings.
//! - Status: refused/unreachable → `Offline`; Identify rejected (close code 4009) → `AuthFailed` (retry at 5 s);
//!   identified → `Connected`; dropping back to retrying → `Connecting` only for the first attempt after a
//!   settings change, else the specific failure status.
//! - Initial state (after Identified, as one RequestBatch or sequential requests): GetVersion,
//!   GetStudioModeEnabled, GetSceneList (OBS returns `scenes` bottom-to-top: reverse it so the snapshot is
//!   top-to-bottom), GetCurrentProgramScene, GetCurrentPreviewScene (studio mode only), GetStreamStatus,
//!   GetRecordStatus, GetReplayBufferStatus (error 604/501 when not configured = inactive), GetVirtualCamStatus,
//!   GetInputList, GetSpecialInputs, and per audio input GetInputMute + GetInputVolume (an input is audio when
//!   GetInputMute succeeds), per scene GetSceneItemList (items top-to-bottom = reverse of the returned order,
//!   OBS lists bottom-to-top by `sceneItemIndex`).
//! - Events (subscribe General|Config|Scenes|Inputs|Outputs|SceneItems|Ui = default `All` minus high-volume):
//!   CurrentProgramSceneChanged, CurrentPreviewSceneChanged, SceneListChanged, SceneCreated/Removed/NameChanged
//!   (refresh scene list + items), StudioModeStateChanged, StreamStateChanged / RecordStateChanged /
//!   ReplayBufferStateChanged / VirtualcamStateChanged (`outputActive`; record also `OBS_WEBSOCKET_OUTPUT_PAUSED`
//!   / `RESUMED` states), InputCreated/Removed/NameChanged (refresh inputs), InputMuteStateChanged,
//!   InputVolumeChanged, SceneItemEnableStateChanged, SceneItemCreated/Removed/ListReindexed (refresh that
//!   scene's items), ExitStarted (→ `Offline` and reconnect loop).
//! - Durations: on connect take `outputDuration` from GetStreamStatus/GetRecordStatus and remember
//!   `Instant::now() - duration`; on state events start/stop the clock; pause freezes it. `snapshot()` computes
//!   `duration_ms` at call time.
//! - `on_change` is called (from the client task, must be cheap: the agent passes a `Notify::notify_one`)
//!   after every state change that alters the snapshot, and on status changes.
//! - [`ObsControl::send`] errs `"OBS is not connected"` unless connected; otherwise queues the request on an
//!   unbounded channel to the task (fire and forget; OBS errors are logged at debug and the next event/refresh
//!   corrects the state). `SetInputVolumeMul` clamps `mul` to 0..=20.
//! - All OBS request names/fields per obs-websocket 5.x protocol.md.

use crate::detect;
use crate::util::{auth_string, out_mut, Clock, OUT_COUNT, SPECIALS};
use futures_util::{FutureExt, SinkExt, StreamExt};
use nd_core::obs::{ObsCommand, ObsControl, ObsInputInfo, ObsSceneItem, ObsSnapshot, ObsStatus};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// General | Config | Scenes | Inputs | Outputs | SceneItems | Ui.
const EVENT_MASK: u32 = 1 | 2 | 4 | 8 | 64 | 128 | 1024;
const AUTH_FAILED_CLOSE: u16 = 4009;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Notideck's OBS settings (`obs.json` in the config dir).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObsSettings {
    pub enabled: bool,
    /// Use OBS's own websocket settings file when it exists (port + password), ignoring host/port/password.
    pub auto_detect: bool,
    pub host: String,
    pub port: u16,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub password: String,
}

impl Default for ObsSettings {
    fn default() -> Self {
        Self { enabled: true, auto_detect: true, host: "127.0.0.1".into(), port: 4455, password: String::new() }
    }
}

/// Connection details for the config UI.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct ObsInfo {
    pub status: ObsStatus,
    /// `host:port` of the current / last attempt, empty when disabled.
    pub endpoint: String,
    /// True when the endpoint came from OBS's own settings file.
    pub from_detected: bool,
    /// OBS Studio version (`GetVersion.obsVersion`) once connected.
    pub obs_version: Option<String>,
    /// obs-websocket version once connected.
    pub ws_version: Option<String>,
    /// Last connection error, human readable (`"connection refused"`, `"authentication failed"`).
    pub last_error: Option<String>,
}

struct St {
    snap: ObsSnapshot,
    clocks: [Clock; OUT_COUNT],
    info: ObsInfo,
    cmd_tx: Option<mpsc::UnboundedSender<ObsCommand>>,
    /// The next attempt follows a settings change (or the start): report `Connecting`.
    first_attempt: bool,
}

struct Inner {
    settings: Mutex<ObsSettings>,
    st: Mutex<St>,
    on_change: Arc<dyn Fn() + Send + Sync>,
    /// Interrupts backoff sleeps.
    wake: Notify,
    /// Drops a live (or connecting) session.
    reset: Notify,
}

impl Inner {
    fn st(&self) -> MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Edit the state; fires `on_change` when the snapshot or the info changed.
    fn update(&self, f: impl FnOnce(&mut St)) {
        let changed = {
            let mut st = self.st();
            let (snap, info) = (st.snap.clone(), st.info.clone());
            f(&mut st);
            st.snap != snap || st.info != info
        };
        if changed {
            (self.on_change)();
        }
    }

    /// Leave the connected state: empty snapshot with `status`.
    fn set_down(&self, status: ObsStatus, error: Option<String>) {
        self.update(|st| {
            st.snap = ObsSnapshot { status, ..Default::default() };
            st.clocks = Default::default();
            st.cmd_tx = None;
            st.info.status = status;
            st.info.obs_version = None;
            st.info.ws_version = None;
            if error.is_some() || status == ObsStatus::Disabled {
                st.info.last_error = error;
            }
        });
    }
}

struct Guard {
    inner: Arc<Inner>,
    task: JoinHandle<()>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Handle to the background client task. Cheap to clone; the task ends when the last handle is dropped.
#[derive(Clone)]
pub struct ObsClient {
    g: Arc<Guard>,
}

impl ObsClient {
    /// Spawn the client task with `settings`. `on_change` fires after every snapshot/status change.
    pub fn start(settings: ObsSettings, on_change: Arc<dyn Fn() + Send + Sync>) -> Self {
        let status = if settings.enabled { ObsStatus::Connecting } else { ObsStatus::Disabled };
        let inner = Arc::new(Inner {
            settings: Mutex::new(settings),
            st: Mutex::new(St {
                snap: ObsSnapshot { status, ..Default::default() },
                clocks: Default::default(),
                info: ObsInfo { status, ..Default::default() },
                cmd_tx: None,
                first_attempt: true,
            }),
            on_change,
            wake: Notify::new(),
            reset: Notify::new(),
        });
        let task = tokio::spawn(run(inner.clone()));
        Self { g: Arc::new(Guard { inner, task }) }
    }

    /// Replace the settings and reconnect now (drops a live connection when the endpoint/password changed
    /// or `enabled` flipped; otherwise keeps it).
    pub fn update_settings(&self, settings: ObsSettings) {
        let inner = &self.g.inner;
        {
            let mut cur = inner.settings.lock().unwrap_or_else(|e| e.into_inner());
            if *cur == settings {
                return;
            }
            *cur = settings;
        }
        inner.st().first_attempt = true;
        inner.reset.notify_one();
        inner.wake.notify_one();
    }

    pub fn settings(&self) -> ObsSettings {
        self.g.inner.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn info(&self) -> ObsInfo {
        self.g.inner.st().info.clone()
    }

    /// Ask the task to retry now (e.g. after the user enabled OBS's server or started OBS).
    pub fn reconnect_now(&self) {
        self.g.inner.wake.notify_one();
    }
}

impl ObsControl for ObsClient {
    fn snapshot(&self) -> ObsSnapshot {
        let st = self.g.inner.st();
        let mut s = st.snap.clone();
        for i in 0..OUT_COUNT {
            let o = out_mut(&mut s, i);
            o.duration_ms = if o.active { st.clocks[i].ms() } else { 0 };
        }
        s
    }

    fn send(&self, cmd: ObsCommand) -> Result<(), String> {
        let st = self.g.inner.st();
        match (&st.cmd_tx, st.snap.status) {
            (Some(tx), ObsStatus::Connected) => tx.send(cmd).map_err(|_| "OBS is not connected".to_string()),
            _ => Err("OBS is not connected".into()),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Connection loop

enum Fail {
    /// Could not connect.
    Offline(String),
    /// Identify rejected.
    Auth,
    /// Connection lost / protocol failure after connecting.
    Closed(String),
    /// Settings changed: reconnect right away.
    Reset,
}

struct Endpoint {
    host: String,
    port: u16,
    password: String,
    from_detected: bool,
}

/// `None` = OBS's own settings say the server is off.
fn resolve(s: &ObsSettings) -> Option<Endpoint> {
    if s.auto_detect {
        if let Some(c) = detect::read() {
            if !c.server_enabled {
                return None;
            }
            let password = if c.auth_required { c.server_password } else { String::new() };
            return Some(Endpoint { host: "127.0.0.1".into(), port: c.server_port, password, from_detected: true });
        }
    }
    Some(Endpoint { host: s.host.clone(), port: s.port, password: s.password.clone(), from_detected: false })
}

async fn sleep_or_wake(inner: &Inner, d: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(d) => {}
        _ = inner.wake.notified() => {}
        _ = inner.reset.notified() => {}
    }
}

async fn run(inner: Arc<Inner>) {
    let mut delay = Duration::from_secs(1);
    loop {
        let settings = inner.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !settings.enabled {
            inner.update(|st| st.info.endpoint.clear());
            inner.set_down(ObsStatus::Disabled, None);
            inner.wake.notified().await;
            continue;
        }
        // Drop permits left over from notifications that arrived before this attempt.
        let _ = inner.reset.notified().now_or_never();
        let _ = inner.wake.notified().now_or_never();
        let first = std::mem::take(&mut inner.st().first_attempt);
        let Some(ep) = resolve(&settings) else {
            inner.set_down(ObsStatus::ServerDisabled, Some("OBS's WebSocket server is switched off".into()));
            sleep_or_wake(&inner, Duration::from_secs(5)).await;
            continue;
        };
        inner.update(|st| {
            st.info.endpoint = format!("{}:{}", ep.host, ep.port);
            st.info.from_detected = ep.from_detected;
            if first {
                st.snap.status = ObsStatus::Connecting;
                st.info.status = ObsStatus::Connecting;
            }
        });
        let mut connected = false;
        let fail = tokio::select! {
            f = session(&inner, &ep, &mut connected) => f,
            _ = inner.reset.notified() => Fail::Reset,
        };
        if connected {
            delay = Duration::from_secs(1);
        }
        let wait = match fail {
            Fail::Reset => {
                delay = Duration::from_secs(1);
                continue;
            }
            Fail::Offline(e) | Fail::Closed(e) => {
                tracing::debug!("OBS: {e}");
                inner.set_down(ObsStatus::Offline, Some(e));
                delay
            }
            Fail::Auth => {
                inner.set_down(ObsStatus::AuthFailed, Some("authentication failed".into()));
                Duration::from_secs(5)
            }
        };
        delay = (delay * 2).min(Duration::from_secs(5));
        sleep_or_wake(&inner, wait).await;
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Conn {
    ws: Ws,
    /// Events that arrived while waiting for a response.
    queue: VecDeque<Value>,
    next_id: u64,
}

impl Conn {
    /// Next JSON message (`{op, d}`).
    async fn next_msg(&mut self) -> Result<Value, Fail> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        return Ok(v);
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    return Err(match frame {
                        Some(f) if u16::from(f.code) == AUTH_FAILED_CLOSE => Fail::Auth,
                        Some(f) => Fail::Closed(format!("closed by OBS ({}) {}", u16::from(f.code), f.reason)),
                        None => Fail::Closed("closed by OBS".into()),
                    })
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(Fail::Closed(e.to_string())),
                None => return Err(Fail::Closed("connection closed".into())),
            }
        }
    }

    async fn send_json(&mut self, v: Value) -> Result<(), Fail> {
        self.ws.send(Message::Text(v.to_string())).await.map_err(|e| Fail::Closed(e.to_string()))
    }

    /// Send a request and wait for its response. `Ok(Err((code, comment)))` = OBS refused the request.
    async fn raw(&mut self, ty: &str, data: Value) -> Result<Result<Value, (i64, String)>, Fail> {
        self.next_id += 1;
        let id = format!("req-{}", self.next_id);
        self.send_json(json!({"op": 6, "d": {"requestType": ty, "requestId": id, "requestData": data}})).await?;
        let wait = async {
            loop {
                let m = self.next_msg().await?;
                let d = &m["d"];
                match m["op"].as_u64() {
                    Some(5) => self.queue.push_back(d.clone()),
                    Some(7) if d["requestId"] == id => {
                        let st = &d["requestStatus"];
                        return Ok(if st["result"].as_bool() == Some(true) {
                            Ok(d["responseData"].clone())
                        } else {
                            Err((st["code"].as_i64().unwrap_or(0), st["comment"].as_str().unwrap_or("").to_string()))
                        });
                    }
                    Some(7) => log_response(d),
                    _ => {}
                }
            }
        };
        tokio::time::timeout(REQUEST_TIMEOUT, wait).await.map_err(|_| Fail::Closed(format!("{ty} timed out")))?
    }

    /// Request that must succeed.
    async fn hard(&mut self, ty: &str, data: Value) -> Result<Value, Fail> {
        self.raw(ty, data).await?.map_err(|(c, m)| Fail::Closed(format!("{ty} failed: {c} {m}")))
    }

    /// Request that may legitimately fail (e.g. replay buffer not configured, input without audio).
    async fn soft(&mut self, ty: &str, data: Value) -> Result<Option<Value>, Fail> {
        Ok(self.raw(ty, data).await?.ok())
    }

    /// Fire and forget.
    async fn fire(&mut self, ty: &str, data: Value) -> Result<(), Fail> {
        self.next_id += 1;
        let id = format!("ff-{}", self.next_id);
        self.send_json(json!({"op": 6, "d": {"requestType": ty, "requestId": id, "requestData": data}})).await
    }
}

fn log_response(d: &Value) {
    let st = &d["requestStatus"];
    if st["result"].as_bool() != Some(true) {
        tracing::debug!("OBS refused {}: {} {}", d["requestType"], st["code"], st["comment"]);
    }
}

async fn session(inner: &Arc<Inner>, ep: &Endpoint, connected: &mut bool) -> Fail {
    match session_inner(inner, ep, connected).await {
        Err(f) => f,
        Ok(never) => match never {},
    }
}

async fn session_inner(
    inner: &Arc<Inner>,
    ep: &Endpoint,
    connected: &mut bool,
) -> Result<std::convert::Infallible, Fail> {
    let mut req = format!("ws://{}:{}", ep.host, ep.port)
        .into_client_request()
        .map_err(|e| Fail::Offline(format!("bad address: {e}")))?;
    req.headers_mut().insert("Sec-WebSocket-Protocol", "obswebsocket.json".parse().expect("static header"));
    let ws = match tokio::time::timeout(Duration::from_secs(3), tokio_tungstenite::connect_async(req)).await {
        Err(_) => return Err(Fail::Offline("connection timed out".into())),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Io(e)))
            if matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset) =>
        {
            return Err(Fail::Offline("connection refused".into()))
        }
        Ok(Err(e)) => return Err(Fail::Offline(e.to_string())),
        Ok(Ok((ws, _))) => ws,
    };
    let mut conn = Conn { ws, queue: VecDeque::new(), next_id: 0 };

    // Hello -> Identify -> Identified.
    let handshake = async {
        let hello = conn.next_msg().await?;
        let mut identify = json!({"rpcVersion": 1, "eventSubscriptions": EVENT_MASK});
        let auth = &hello["d"]["authentication"];
        if let (Some(ch), Some(salt)) = (auth["challenge"].as_str(), auth["salt"].as_str()) {
            identify["authentication"] = json!(auth_string(&ep.password, salt, ch));
        }
        conn.send_json(json!({"op": 1, "d": identify})).await?;
        loop {
            if conn.next_msg().await?["op"].as_u64() == Some(2) {
                return Ok::<_, Fail>(());
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), handshake)
        .await
        .map_err(|_| Fail::Closed("handshake timed out".to_string()))??;

    let (tx, mut rx) = mpsc::unbounded_channel();
    refresh_all(inner, &mut conn).await?;
    *connected = true;
    inner.update(|st| {
        st.cmd_tx = Some(tx);
        st.snap.status = ObsStatus::Connected;
        st.info.status = ObsStatus::Connected;
        st.info.last_error = None;
    });

    loop {
        while let Some(ev) = conn.queue.pop_front() {
            on_event(inner, &mut conn, &ev).await?;
        }
        tokio::select! {
            m = conn.next_msg() => {
                let m = m?;
                match m["op"].as_u64() {
                    Some(5) => on_event(inner, &mut conn, &m["d"]).await?,
                    Some(7) => log_response(&m["d"]),
                    _ => {}
                }
            }
            c = rx.recv() => {
                let Some(c) = c else { return Err(Fail::Closed("command channel closed".into())) };
                let (ty, data) = command_request(&c);
                conn.fire(ty, data).await?;
            }
        }
    }
}

/// OBS request for a command.
fn command_request(cmd: &ObsCommand) -> (&'static str, Value) {
    fn on_off(b: bool, on: &'static str, off: &'static str) -> &'static str {
        if b {
            on
        } else {
            off
        }
    }
    match cmd {
        ObsCommand::SetProgramScene(n) => ("SetCurrentProgramScene", json!({"sceneName": n})),
        ObsCommand::SetPreviewScene(n) => ("SetCurrentPreviewScene", json!({"sceneName": n})),
        ObsCommand::SetSceneItemEnabled { scene, item_id, enabled } => {
            ("SetSceneItemEnabled", json!({"sceneName": scene, "sceneItemId": item_id, "sceneItemEnabled": enabled}))
        }
        ObsCommand::Stream(b) => (on_off(*b, "StartStream", "StopStream"), json!({})),
        ObsCommand::Record(b) => (on_off(*b, "StartRecord", "StopRecord"), json!({})),
        ObsCommand::RecordPause(b) => (on_off(*b, "PauseRecord", "ResumeRecord"), json!({})),
        ObsCommand::ReplayBuffer(b) => (on_off(*b, "StartReplayBuffer", "StopReplayBuffer"), json!({})),
        ObsCommand::SaveReplayBuffer => ("SaveReplayBuffer", json!({})),
        ObsCommand::VirtualCam(b) => (on_off(*b, "StartVirtualCam", "StopVirtualCam"), json!({})),
        ObsCommand::SetStudioMode(b) => ("SetStudioModeEnabled", json!({"studioModeEnabled": b})),
        ObsCommand::StudioTransition => ("TriggerStudioModeTransition", json!({})),
        ObsCommand::SetInputMute { input, muted } => ("SetInputMute", json!({"inputName": input, "inputMuted": muted})),
        ObsCommand::SetInputVolumeMul { input, mul } => {
            ("SetInputVolume", json!({"inputName": input, "inputVolumeMul": mul.clamp(0.0, 20.0)}))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// State fetching

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

struct SceneList {
    names: Vec<String>,
    program: Option<String>,
    preview: Option<String>,
}

/// `GetSceneList` → names top to bottom (OBS reports `sceneIndex` 0 at the bottom).
async fn fetch_scenes(conn: &mut Conn) -> Result<SceneList, Fail> {
    let v = conn.hard("GetSceneList", json!({})).await?;
    let mut scenes: Vec<(i64, String)> = v["scenes"]
        .as_array()
        .map(|a| a.iter().map(|x| (x["sceneIndex"].as_i64().unwrap_or(0), s(x, "sceneName"))).collect())
        .unwrap_or_default();
    scenes.sort_by_key(|(i, _)| std::cmp::Reverse(*i));
    Ok(SceneList {
        names: scenes.into_iter().map(|(_, n)| n).collect(),
        program: v["currentProgramSceneName"].as_str().map(String::from),
        preview: v["currentPreviewSceneName"].as_str().map(String::from),
    })
}

/// `GetSceneItemList` → items top to bottom (highest `sceneItemIndex` first).
async fn fetch_items(conn: &mut Conn, scene: &str) -> Result<Vec<ObsSceneItem>, Fail> {
    let Some(v) = conn.soft("GetSceneItemList", json!({"sceneName": scene})).await? else { return Ok(vec![]) };
    let mut items: Vec<(i64, ObsSceneItem)> = v["sceneItems"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    let item = ObsSceneItem {
                        source: s(x, "sourceName"),
                        item_id: x["sceneItemId"].as_i64().unwrap_or(0),
                        enabled: x["sceneItemEnabled"].as_bool().unwrap_or(true),
                    };
                    (x["sceneItemIndex"].as_i64().unwrap_or(0), item)
                })
                .collect()
        })
        .unwrap_or_default();
    items.sort_by_key(|(i, _)| std::cmp::Reverse(*i));
    Ok(items.into_iter().map(|(_, i)| i).collect())
}

async fn fetch_all_items(conn: &mut Conn, scenes: &[String]) -> Result<BTreeMap<String, Vec<ObsSceneItem>>, Fail> {
    let mut map = BTreeMap::new();
    for n in scenes {
        map.insert(n.clone(), fetch_items(conn, n).await?);
    }
    Ok(map)
}

async fn fetch_inputs(
    conn: &mut Conn,
) -> Result<(Vec<ObsInputInfo>, BTreeMap<nd_core::obs::SpecialInput, String>), Fail> {
    let list = conn.hard("GetInputList", json!({})).await?;
    let mut inputs = Vec::new();
    for x in list["inputs"].as_array().into_iter().flatten() {
        let name = s(x, "inputName");
        let mut info = ObsInputInfo { name: name.clone(), kind: s(x, "inputKind"), ..Default::default() };
        if let Some(m) = conn.soft("GetInputMute", json!({"inputName": name})).await? {
            info.muted = m["inputMuted"].as_bool();
            if let Some(vol) = conn.soft("GetInputVolume", json!({"inputName": name})).await? {
                info.volume_mul = vol["inputVolumeMul"].as_f64().map(|f| f as f32);
            }
        }
        inputs.push(info);
    }
    let sp = conn.hard("GetSpecialInputs", json!({})).await?;
    let special = SPECIALS.into_iter().filter_map(|k| Some((k, sp[k.ws_key()].as_str()?.to_string()))).collect();
    Ok((inputs, special))
}

/// (active, paused, duration ms)
fn output_status(v: &Value) -> (bool, bool, u64) {
    (
        v["outputActive"].as_bool().unwrap_or(false),
        v["outputPaused"].as_bool().unwrap_or(false),
        v["outputDuration"].as_u64().unwrap_or(0),
    )
}

async fn refresh_all(inner: &Arc<Inner>, conn: &mut Conn) -> Result<(), Fail> {
    let version = conn.hard("GetVersion", json!({})).await?;
    let studio = conn.hard("GetStudioModeEnabled", json!({})).await?["studioModeEnabled"].as_bool().unwrap_or(false);
    let scenes = fetch_scenes(conn).await?;
    let scene_items = fetch_all_items(conn, &scenes.names).await?;
    let (inputs, special_inputs) = fetch_inputs(conn).await?;
    let mut snap = ObsSnapshot {
        status: ObsStatus::Connecting,
        studio_mode: studio,
        program_scene: scenes.program,
        preview_scene: scenes.preview.filter(|_| studio),
        scenes: scenes.names,
        inputs,
        special_inputs,
        scene_items,
        ..Default::default()
    };
    let mut clocks: [Clock; OUT_COUNT] = Default::default();
    for (i, ty) in
        ["GetStreamStatus", "GetRecordStatus", "GetReplayBufferStatus", "GetVirtualCamStatus"].iter().enumerate()
    {
        let (active, paused, ms) = conn.soft(ty, json!({})).await?.map(|v| output_status(&v)).unwrap_or_default();
        let o = out_mut(&mut snap, i);
        o.active = active;
        o.paused = active && paused;
        if active {
            clocks[i].start_from(ms);
            if paused {
                clocks[i].pause();
            }
        }
    }
    inner.update(|st| {
        snap.status = st.snap.status;
        st.snap = snap;
        st.clocks = clocks;
        st.info.obs_version = version["obsVersion"].as_str().map(String::from);
        st.info.ws_version = version["obsWebSocketVersion"].as_str().map(String::from);
    });
    Ok(())
}

async fn refresh_scenes(inner: &Arc<Inner>, conn: &mut Conn) -> Result<(), Fail> {
    let scenes = fetch_scenes(conn).await?;
    let items = fetch_all_items(conn, &scenes.names).await?;
    inner.update(|st| {
        st.snap.preview_scene = scenes.preview.filter(|_| st.snap.studio_mode);
        st.snap.program_scene = scenes.program;
        st.snap.scenes = scenes.names;
        st.snap.scene_items = items;
    });
    Ok(())
}

async fn refresh_inputs(inner: &Arc<Inner>, conn: &mut Conn) -> Result<(), Fail> {
    let (inputs, special) = fetch_inputs(conn).await?;
    inner.update(|st| {
        st.snap.inputs = inputs;
        st.snap.special_inputs = special;
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// Events

async fn on_event(inner: &Arc<Inner>, conn: &mut Conn, ev: &Value) -> Result<(), Fail> {
    let ty = ev["eventType"].as_str().unwrap_or_default();
    let d = &ev["eventData"];
    match ty {
        "ExitStarted" => return Err(Fail::Closed("OBS is closing".into())),
        "CurrentProgramSceneChanged" => {
            inner.update(|st| st.snap.program_scene = d["sceneName"].as_str().map(Into::into))
        }
        "CurrentPreviewSceneChanged" => {
            inner.update(|st| st.snap.preview_scene = d["sceneName"].as_str().map(Into::into))
        }
        "SceneListChanged" | "SceneCreated" | "SceneRemoved" | "SceneNameChanged" => {
            refresh_scenes(inner, conn).await?
        }
        "StudioModeStateChanged" => {
            let on = d["studioModeEnabled"].as_bool().unwrap_or(false);
            inner.update(|st| {
                st.snap.studio_mode = on;
                if !on {
                    st.snap.preview_scene = None;
                }
            });
            if on {
                refresh_scenes(inner, conn).await?;
            }
        }
        "StreamStateChanged" => output_event(inner, 0, d),
        "RecordStateChanged" => output_event(inner, 1, d),
        "ReplayBufferStateChanged" => output_event(inner, 2, d),
        "VirtualcamStateChanged" => output_event(inner, 3, d),
        "InputCreated" | "InputRemoved" | "InputNameChanged" => {
            refresh_inputs(inner, conn).await?;
            refresh_scenes(inner, conn).await?;
        }
        "InputMuteStateChanged" => {
            let (n, m) = (s(d, "inputName"), d["inputMuted"].as_bool());
            inner.update(|st| st.snap.inputs.iter_mut().filter(|i| i.name == n).for_each(|i| i.muted = m));
        }
        "InputVolumeChanged" => {
            let (n, v) = (s(d, "inputName"), d["inputVolumeMul"].as_f64().map(|f| f as f32));
            inner.update(|st| st.snap.inputs.iter_mut().filter(|i| i.name == n).for_each(|i| i.volume_mul = v));
        }
        "SceneItemEnableStateChanged" => {
            let (scene, id, en) = (s(d, "sceneName"), d["sceneItemId"].as_i64(), d["sceneItemEnabled"].as_bool());
            inner.update(|st| {
                if let (Some(items), Some(en)) = (st.snap.scene_items.get_mut(&scene), en) {
                    items.iter_mut().filter(|i| Some(i.item_id) == id).for_each(|i| i.enabled = en);
                }
            });
        }
        "SceneItemCreated" | "SceneItemRemoved" | "SceneItemListReindexed" => {
            let scene = s(d, "sceneName");
            let items = fetch_items(conn, &scene).await?;
            inner.update(|st| {
                st.snap.scene_items.insert(scene, items);
            });
        }
        "CurrentSceneCollectionChanged" => refresh_all(inner, conn).await?,
        _ => {}
    }
    Ok(())
}

/// Apply an output state event (only the settled states; STARTING/STOPPING/... are ignored).
fn output_event(inner: &Inner, i: usize, d: &Value) {
    let state = d["outputState"].as_str().unwrap_or_default();
    inner.update(|st| {
        let o = out_mut(&mut st.snap, i);
        match state {
            "OBS_WEBSOCKET_OUTPUT_STARTED" => {
                *o = nd_core::obs::OutputState { active: true, ..Default::default() };
                st.clocks[i].start_from(0);
            }
            "OBS_WEBSOCKET_OUTPUT_STOPPED" => {
                *o = Default::default();
                st.clocks[i].stop();
            }
            "OBS_WEBSOCKET_OUTPUT_PAUSED" if o.active => {
                o.paused = true;
                st.clocks[i].pause();
            }
            "OBS_WEBSOCKET_OUTPUT_RESUMED" if o.active => {
                o.paused = false;
                st.clocks[i].resume();
            }
            _ => {}
        }
    });
}
