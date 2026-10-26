// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared host state. One short `std::sync::Mutex` guards everything; it is never held across an `.await`.

use crate::discovery::{Discovered, Persisted};
use crate::host::{HostEvent, PluginInfo, PluginStatus};
use crate::manifest::{Manifest, PROTOCOL_VERSION};
use crate::supervisor::{resolve_program, SpawnSpec, Spawner};
use nd_core::live::LiveSample;
use nd_core::mixer::IconImporter;
use nd_core::plugin::{PluginInstance, PluginSettings};
use rand::RngCore;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};

pub(crate) const LOG_LINES: usize = 200;

/// Frames queued to a plugin's socket writer.
pub(crate) enum Out {
    Text(String),
    Close,
}

#[derive(Default)]
pub(crate) struct Done {
    m: Mutex<bool>,
    cv: Condvar,
}

impl Done {
    pub fn set(&self) {
        *self.m.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_all();
    }

    /// Wait until set; `false` on timeout.
    pub fn wait(&self, timeout: Duration) -> bool {
        let g = self.m.lock().unwrap_or_else(|e| e.into_inner());
        let (g, _) = self.cv.wait_timeout_while(g, timeout, |d| !*d).unwrap_or_else(|e| e.into_inner());
        *g
    }
}

pub(crate) struct Conn {
    pub id: u64,
    pub tx: mpsc::UnboundedSender<Out>,
}

pub(crate) struct Run {
    pub gen: u64,
    pub stop: watch::Sender<bool>,
    pub done: Arc<Done>,
}

pub(crate) struct Entry {
    pub dir: PathBuf,
    pub bundled: bool,
    pub manifest: Result<Manifest, String>,
    /// Folder name (used as id / name when the manifest is invalid).
    pub folder: String,
    pub status: PluginStatus,
    pub log: VecDeque<String>,
    pub token: Option<String>,
    pub conn: Option<Conn>,
    /// Contexts this plugin was told about on the current connection.
    pub seen: HashSet<String>,
    pub run: Option<Run>,
    pub disconnected: Arc<Notify>,
}

impl Entry {
    pub fn from_discovered(d: &Discovered) -> Self {
        Entry {
            dir: d.dir.clone(),
            bundled: d.bundled,
            manifest: d.manifest.clone(),
            folder: d.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            status: PluginStatus::Invalid,
            log: VecDeque::new(),
            token: None,
            conn: None,
            seen: HashSet::new(),
            run: None,
            disconnected: Arc::new(Notify::new()),
        }
    }

    /// Status when nothing is running: `None` = eligible to start.
    pub fn idle_status(&self, persisted: &Persisted, id: &str) -> Option<PluginStatus> {
        let m = match &self.manifest {
            Ok(m) => m,
            Err(_) => return Some(PluginStatus::Invalid),
        };
        if m.command_for_os().is_none() {
            return Some(PluginStatus::Unsupported);
        }
        if persisted.disabled.contains(id) {
            return Some(PluginStatus::Disabled);
        }
        if persisted.needs_approval(id, self.bundled, &m.permissions) {
            return Some(PluginStatus::NeedsApproval);
        }
        None
    }

    pub fn push_log(&mut self, stream: &str, line: &str) {
        if self.log.len() >= LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(format!("{} [{stream}] {line}", clock()));
    }

    pub fn send(&self, text: String) {
        if let Some(c) = &self.conn {
            let _ = c.tx.send(Out::Text(text));
        }
    }

    pub fn info(&self, id: &str) -> PluginInfo {
        match &self.manifest {
            Ok(m) => PluginInfo {
                id: m.id.clone(),
                name: m.name.clone(),
                version: m.version.clone(),
                author: m.author.clone(),
                description: m.description.clone(),
                icon: m.icon.clone(),
                permissions: m.permissions.clone(),
                bundled: self.bundled,
                status: self.status,
                error: None,
                actions: m.actions.clone(),
            },
            Err(e) => PluginInfo {
                id: id.to_string(),
                name: self.folder.clone(),
                version: String::new(),
                author: None,
                description: None,
                icon: None,
                permissions: Vec::new(),
                bundled: self.bundled,
                status: PluginStatus::Invalid,
                error: Some(e.clone()),
                actions: Vec::new(),
            },
        }
    }
}

/// An on-screen instance. `inst.settings` is the host's current copy; `reported` is what the agent last said.
pub(crate) struct Visible {
    pub inst: PluginInstance,
    pub reported: PluginSettings,
}

#[derive(Default)]
pub(crate) struct Look {
    pub plugin: String,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub icon_hash: Option<String>,
    pub state: Option<u32>,
    pub value: Option<f32>,
    /// Round 20: `setDetail` (third text line, `Control.detail`).
    pub detail: Option<String>,
}

pub(crate) struct State {
    pub plugins: BTreeMap<String, Entry>,
    pub visible: HashMap<String, Visible>,
    pub looks: HashMap<String, Look>,
    pub persisted: Persisted,
    pub images: HashMap<u64, String>,
    pub next_id: u64,
    pub shutting_down: bool,
}

pub(crate) struct Shared {
    pub bundled_dirs: Vec<PathBuf>,
    pub user_dir: PathBuf,
    pub state_file: PathBuf,
    pub icons: IconImporter,
    pub poke: Arc<dyn Fn() + Send + Sync>,
    pub events: mpsc::UnboundedSender<HostEvent>,
    pub agent_version: String,
    pub port: u16,
    pub handle: tokio::runtime::Handle,
    /// `None` = never spawn processes (tests drive the socket directly).
    pub spawner: Option<Spawner>,
    pub state: Mutex<State>,
}

pub(crate) fn clock() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{:02}:{:02}:{:02}", secs / 3600 % 24, secs / 60 % 60, secs % 60)
}

pub(crate) fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

fn control_name(c: nd_core::model::ControlKind) -> Value {
    serde_json::to_value(c).unwrap_or(Value::Null)
}

pub(crate) fn appear_message(inst: &PluginInstance) -> String {
    notification(
        "action.willAppear",
        json!({
            "context": inst.context,
            "action": inst.action,
            "settings": inst.settings,
            "controller": control_name(inst.control),
            "state": inst.state,
            "value": inst.value,
        }),
    )
}

fn disappear_message(inst: &PluginInstance) -> String {
    notification(
        "action.willDisappear",
        json!({"context": inst.context, "action": inst.action, "settings": inst.settings}),
    )
}

fn changed_message(inst: &PluginInstance) -> String {
    notification(
        "settings.didChange",
        json!({"context": inst.context, "action": inst.action, "settings": inst.settings}),
    )
}

/// Send `action.willAppear` to the plugin that owns `inst`, if it is connected.
fn send_appear(plugins: &mut BTreeMap<String, Entry>, inst: &PluginInstance) {
    if let Some(e) = plugins.get_mut(&inst.plugin) {
        if e.conn.is_some() {
            e.seen.insert(inst.context.clone());
            e.send(appear_message(inst));
        }
    }
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push_log(&self, id: &str, stream: &str, line: &str) {
        if let Some(e) = self.lock().plugins.get_mut(id) {
            e.push_log(stream, line);
        }
    }

    pub fn save_persisted(&self, st: &State) -> Result<(), crate::host::HostError> {
        st.persisted
            .save(&self.state_file)
            .map_err(|e| crate::host::HostError::Io(format!("cannot save plugins.json: {e}")))
    }

    /// Build the launch for generation `gen`: new token, status `Starting`.
    pub fn prepare_launch(&self, id: &str, gen: u64) -> Option<(SpawnSpec, Arc<Notify>)> {
        let mut st = self.lock();
        self.prepare_launch_locked(&mut st, id, gen)
    }

    pub fn prepare_launch_locked(&self, st: &mut State, id: &str, gen: u64) -> Option<(SpawnSpec, Arc<Notify>)> {
        if st.shutting_down {
            return None;
        }
        let e = st.plugins.get_mut(id)?;
        if e.run.as_ref().map(|r| r.gen) != Some(gen) {
            return None;
        }
        let argv = e.manifest.as_ref().ok()?.command_for_os()?;
        let token = random_hex(16);
        let mut args: Vec<String> = argv[1..].to_vec();
        args.extend([
            "--port".into(),
            self.port.to_string(),
            "--token".into(),
            token.clone(),
            "--plugin-id".into(),
            id.to_string(),
        ]);
        let spec = SpawnSpec {
            program: resolve_program(&e.dir, &argv[0]),
            args,
            cwd: e.dir.clone(),
            env: vec![
                ("NOTIDECK_PORT".into(), self.port.to_string()),
                ("NOTIDECK_TOKEN".into(), token.clone()),
                ("NOTIDECK_PLUGIN_ID".into(), id.to_string()),
            ],
        };
        e.token = Some(token);
        e.conn = None;
        e.seen.clear();
        e.status = PluginStatus::Starting;
        e.disconnected = Arc::new(Notify::new());
        Some((spec, e.disconnected.clone()))
    }

    /// The process (or its connection) is gone.
    pub fn on_exit(&self, id: &str, gen: u64) {
        let mut st = self.lock();
        if let Some(e) = st.plugins.get_mut(id) {
            if e.run.as_ref().map(|r| r.gen) == Some(gen) {
                e.token = None;
                if let Some(c) = e.conn.take() {
                    let _ = c.tx.send(Out::Close);
                }
                e.seen.clear();
                e.status = PluginStatus::Starting;
            }
        }
        drop(st);
        (self.poke)();
    }

    pub fn set_status(&self, id: &str, gen: u64, status: PluginStatus) {
        let mut st = self.lock();
        if let Some(e) = st.plugins.get_mut(id) {
            if e.run.as_ref().map(|r| r.gen) == Some(gen) {
                e.status = status;
            }
        }
        drop(st);
        (self.poke)();
    }

    /// Start the supervisor for `id` (must have no run). Caller holds the lock.
    pub fn start_plugin(self: &Arc<Self>, st: &mut State, id: &str) {
        st.next_id += 1;
        let gen = st.next_id;
        let (stop_tx, stop_rx) = watch::channel(false);
        let done = Arc::new(Done::default());
        let Some(e) = st.plugins.get_mut(id) else { return };
        e.run = Some(Run { gen, stop: stop_tx, done: done.clone() });
        e.status = PluginStatus::Starting;
        if self.spawner.is_some() {
            let shared = self.clone();
            let id = id.to_string();
            self.handle.spawn(crate::supervisor::supervise(shared, id, gen, stop_rx, done));
        } else {
            // Tests: no process, just issue a token so a client can register.
            let id = id.to_string();
            self.prepare_launch_locked(st, &id, gen);
            done.set();
        }
    }

    /// Ask the supervisor to close the socket and stop the process. Returns the handle to wait on.
    pub fn stop_plugin(&self, st: &mut State, id: &str) -> Option<Arc<Done>> {
        let e = st.plugins.get_mut(id)?;
        let run = e.run.take()?;
        if let Some(c) = e.conn.take() {
            let _ = c.tx.send(Out::Close);
        }
        e.token = None;
        e.seen.clear();
        let _ = run.stop.send(true);
        Some(run.done)
    }

    // ---- PluginControl internals -------------------------------------------------------------

    pub fn set_visible(&self, instances: Vec<PluginInstance>) {
        let mut st = self.lock();
        let st = &mut *st;
        let mut old = std::mem::take(&mut st.visible);
        let mut new: HashMap<String, Visible> = HashMap::with_capacity(instances.len());
        for inst in instances {
            let reported = inst.settings.clone();
            match old.remove(&inst.context).or_else(|| new.remove(&inst.context)) {
                Some(mut v) if v.inst.plugin == inst.plugin && v.inst.action == inst.action => {
                    if inst.settings != v.reported {
                        v.reported = inst.settings.clone();
                        if inst.settings != v.inst.settings {
                            v.inst.settings = inst.settings.clone();
                            if let Some(e) = st.plugins.get(&inst.plugin) {
                                e.send(changed_message(&v.inst));
                            }
                        }
                    }
                    v.inst.control = inst.control;
                    v.inst.state = inst.state;
                    v.inst.value = inst.value;
                    new.insert(inst.context.clone(), v);
                }
                prev => {
                    if let Some(p) = prev {
                        if let Some(e) = st.plugins.get(&p.inst.plugin) {
                            e.send(disappear_message(&p.inst));
                        }
                    }
                    send_appear(&mut st.plugins, &inst);
                    new.insert(inst.context.clone(), Visible { inst, reported });
                }
            }
        }
        for (_, gone) in old {
            if let Some(e) = st.plugins.get(&gone.inst.plugin) {
                e.send(disappear_message(&gone.inst));
            }
        }
        st.visible = new;
    }

    pub fn sample(&self, context: &str) -> Option<LiveSample> {
        let st = self.lock();
        let plugin = st
            .visible
            .get(context)
            .map(|v| v.inst.plugin.as_str())
            .or_else(|| st.looks.get(context).map(|l| l.plugin.as_str()))?;
        let running = st.plugins.get(plugin).is_some_and(|e| e.status == PluginStatus::Running);
        let mut s = LiveSample { dimmed: Some(!running), ..Default::default() };
        if let Some(l) = st.looks.get(context).filter(|l| l.plugin == plugin) {
            s.title = l.title.clone();
            s.subtitle = l.subtitle.clone();
            s.icon_hash = l.icon_hash.clone();
            s.state_index = l.state;
            s.value = l.value;
            s.detail = l.detail.clone();
        }
        Some(s)
    }

    pub fn event(&self, ev: nd_core::plugin::PluginEvent) {
        use nd_core::plugin::PluginEventKind as K;
        let mut params = json!({"context": ev.context, "action": ev.action, "settings": ev.settings});
        match ev.kind {
            K::DialRotate { ticks } => params["ticks"] = json!(ticks),
            K::SliderChange { value } => params["value"] = json!(value),
            _ => {}
        }
        let st = self.lock();
        if let Some(e) = st.plugins.get(&ev.plugin) {
            e.send(notification(ev.kind.method(), params));
        }
    }

    pub fn send_to(&self, plugin: &str, method: &str, params: Value) {
        if let Some(e) = self.lock().plugins.get(plugin) {
            e.send(notification(method, params));
        }
    }

    pub fn broadcast(&self, method: &str, params: Value) {
        let text = notification(method, params);
        for e in self.lock().plugins.values() {
            e.send(text.clone());
        }
    }

    /// Register a connection for `id` (token already checked). Returns the writer queue and connection id.
    /// The reply (if any) goes out first, then `willAppear` for each visible instance.
    pub fn attach(&self, id: &str, reply: Option<String>) -> Option<(u64, mpsc::UnboundedReceiver<Out>)> {
        let mut g = self.lock();
        let st = &mut *g;
        st.next_id += 1;
        let conn_id = st.next_id;
        let (tx, rx) = mpsc::unbounded_channel();
        let e = st.plugins.get_mut(id)?;
        if let Some(old) = e.conn.replace(Conn { id: conn_id, tx: tx.clone() }) {
            let _ = old.tx.send(Out::Close);
        }
        e.seen.clear();
        e.status = PluginStatus::Running;
        if let Some(r) = reply {
            let _ = tx.send(Out::Text(r));
        }
        let mut appear: Vec<&PluginInstance> =
            st.visible.values().map(|v| &v.inst).filter(|i| i.plugin == id).collect();
        appear.sort_by(|a, b| a.context.cmp(&b.context));
        for inst in appear {
            e.seen.insert(inst.context.clone());
            let _ = tx.send(Out::Text(appear_message(inst)));
        }
        drop(g);
        (self.poke)();
        Some((conn_id, rx))
    }

    pub fn detach(&self, id: &str, conn_id: u64) {
        let mut st = self.lock();
        if let Some(e) = st.plugins.get_mut(id) {
            if e.conn.as_ref().map(|c| c.id) == Some(conn_id) {
                e.conn = None;
                e.token = None;
                e.seen.clear();
                if e.run.is_some() && e.status == PluginStatus::Running {
                    e.status = PluginStatus::Starting;
                }
                e.disconnected.notify_one();
            }
        }
        drop(st);
        (self.poke)();
    }

    pub fn protocol_reply(&self) -> Value {
        json!({
            "protocol": PROTOCOL_VERSION,
            "agent": {"version": self.agent_version, "os": if cfg!(windows) { "windows" } else { "linux" }},
        })
    }
}
