// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin host: discovery, approval/enable state, supervision, JSON-RPC server, look cache.

use crate::discovery::{discover, Persisted};
use crate::manifest::{Manifest, ManifestAction, Permission};
use crate::state::{random_hex, Entry, Shared, State};
use nd_core::live::LiveSample;
use nd_core::mixer::IconImporter;
use nd_core::plugin::{PluginControl, PluginEvent, PluginInstance, PluginSettings};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How the agent configures the host.
pub struct HostConfig {
    /// Bundled (trusted) plugin roots, searched in order; each holds `<id>/manifest.json` folders.
    pub bundled_dirs: Vec<PathBuf>,
    /// User plugin root (`<config>/plugins`); created on demand. Overrides bundled plugins with the same id.
    pub user_dir: PathBuf,
    /// Persisted enable/approval state (`<config>/plugins.json`): `{"disabled": [ids], "approved": {id: [perms]}}`.
    /// A user plugin whose current permissions are not a subset of its approved ones needs approval again.
    pub state_file: PathBuf,
    /// Imports PNG bytes into the agent icon store and returns the hash (used by `setImage`).
    pub icons: IconImporter,
    /// Called whenever a plugin changed a look, so the agent runs a live tick at once.
    pub poke: Arc<dyn Fn() + Send + Sync>,
    /// Requests from plugins that the agent must carry out.
    pub events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    /// Agent version for the `register` reply.
    pub agent_version: String,
}

/// Plugin → agent requests the host can't handle itself.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// `showOk` (`ok: true`) / `showAlert`: flash the control on every phone that shows it.
    Feedback { context: String, ok: bool },
    /// `openUrl` (already checked to be http/https).
    OpenUrl { url: String },
    /// `setSettings`: persist into every plugin binding of this plugin on the context's control and save the
    /// profile (or rail). The host already updated its own copy and does not echo `settings.didChange`.
    SetSettings { context: String, plugin: String, settings: PluginSettings },
    /// Round 20, `phone.play`: play an HLS stream in the phone's built-in player (url already checked to be
    /// https). The agent sends `PlayStream` to the active phone if it has `CAPABILITY_PLAYER`, else to the
    /// first connected phone that has it; with none, it flashes an alert on `context` (if any) and logs.
    /// Round 21: `muted`, `sync_delay_ms` (see proto `PlayStream`), and `session` = the plugin's opaque
    /// session string; the agent sends `PlayStream.session_id = "{plugin}/{session}"` and routes a phone
    /// `PlayerClosed` back to that plugin with [`PluginHost::player_closed`].
    PlayOnPhone {
        plugin: String,
        context: Option<String>,
        url: String,
        title: String,
        subtitle: String,
        max_height: u32,
        muted: bool,
        sync_delay_ms: u32,
        session: String,
    },
    /// Round 20, `phone.stop`: close the player on every phone that has `CAPABILITY_PLAYER`.
    StopOnPhone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStatus {
    Starting,
    Running,
    Disabled,
    /// User plugin not yet approved (or its permissions grew since).
    NeedsApproval,
    /// Gave up after repeated crashes; `restart` tries again.
    Crashed,
    /// Manifest missing or invalid (`error` says why).
    Invalid,
    /// No entry for this OS.
    Unsupported,
}

/// One row of `GET /api/plugins`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Manifest icon path (fetch via `GET /api/plugins/{id}/file?path=…`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub permissions: Vec<Permission>,
    pub bundled: bool,
    pub status: PluginStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub actions: Vec<ManifestAction>,
}

/// UI commands on one plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCommand {
    Enable,
    Disable,
    /// Approve the current permissions (user plugins) and start it.
    Approve,
    /// Stop and start again; also clears `Crashed`.
    Restart,
    /// Stop and delete the folder (user plugins only).
    Uninstall,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum HostError {
    #[error("no plugin {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(String),
}

/// The running host. Cheap to share (`Arc`).
pub struct PluginHost {
    shared: Arc<Shared>,
    server: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

fn io_err(e: impl std::fmt::Display) -> HostError {
    HostError::Io(e.to_string())
}

impl PluginHost {
    /// Scan the plugin dirs, bind the WebSocket server on `127.0.0.1:0`, and start every enabled + approved
    /// supported plugin. Must be called inside a tokio runtime.
    pub fn start(config: HostConfig) -> std::io::Result<Arc<Self>> {
        Self::start_inner(config, Some(crate::supervisor::real_spawner()))
    }

    /// `spawner: None` never launches processes (tests connect to the socket themselves).
    pub(crate) fn start_inner(
        config: HostConfig,
        spawner: Option<crate::supervisor::Spawner>,
    ) -> std::io::Result<Arc<Self>> {
        let handle = tokio::runtime::Handle::current();
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        std_listener.set_nonblocking(true)?;
        let port = std_listener.local_addr()?.port();
        let persisted = Persisted::load(&config.state_file);
        let state = State {
            plugins: BTreeMap::new(),
            visible: HashMap::new(),
            looks: HashMap::new(),
            persisted,
            images: HashMap::new(),
            next_id: 0,
            shutting_down: false,
        };
        let shared = Arc::new(Shared {
            bundled_dirs: config.bundled_dirs,
            user_dir: config.user_dir,
            state_file: config.state_file,
            icons: config.icons,
            poke: config.poke,
            events: config.events,
            agent_version: config.agent_version,
            port,
            handle: handle.clone(),
            spawner,
            state: Mutex::new(state),
        });
        let listener = {
            let _guard = handle.enter();
            tokio::net::TcpListener::from_std(std_listener)?
        };
        let task = handle.spawn(crate::server::serve(shared.clone(), listener));
        let host = Arc::new(PluginHost { shared, server: Mutex::new(Some(task)) });
        host.reload();
        Ok(host)
    }

    #[cfg(test)]
    pub(crate) fn port(&self) -> u16 {
        self.shared.port
    }

    #[cfg(test)]
    pub(crate) fn token(&self, id: &str) -> Option<String> {
        self.shared.lock().plugins.get(id).and_then(|e| e.token.clone())
    }

    /// All discovered plugins (bundled and user), sorted by name.
    pub fn list(&self) -> Vec<PluginInfo> {
        let st = self.shared.lock();
        let mut v: Vec<PluginInfo> = st.plugins.iter().map(|(id, e)| e.info(id)).collect();
        v.sort_by_key(|p| (p.name.to_lowercase(), p.id.clone()));
        v
    }

    /// Manifest of an installed plugin (any status except `Invalid`).
    pub fn manifest(&self, id: &str) -> Option<Manifest> {
        self.shared.lock().plugins.get(id).and_then(|e| e.manifest.as_ref().ok().cloned())
    }

    pub fn command(&self, id: &str, cmd: PluginCommand) -> Result<(), HostError> {
        let sh = &self.shared;
        let mut waits = Vec::new();
        let mut uninstall_dir = None;
        let result = {
            let mut guard = sh.lock();
            let st = &mut *guard;
            let (bundled, dir) = match st.plugins.get(id) {
                Some(e) => (e.bundled, e.dir.clone()),
                None => return Err(HostError::NotFound(id.to_string())),
            };
            match cmd {
                PluginCommand::Enable => {
                    st.persisted.disabled.remove(id);
                    sh.save_persisted(st)?;
                    self.start_if_eligible(st, id);
                    Ok(())
                }
                PluginCommand::Disable => {
                    st.persisted.disabled.insert(id.to_string());
                    sh.save_persisted(st)?;
                    waits.extend(sh.stop_plugin(st, id));
                    if let Some(e) = st.plugins.get_mut(id) {
                        e.status = PluginStatus::Disabled;
                    }
                    Ok(())
                }
                PluginCommand::Approve => {
                    let perms = match st.plugins.get(id).map(|e| e.manifest.clone()) {
                        Some(Ok(m)) => m.permissions,
                        _ => return Err(HostError::Invalid("plugin has an invalid manifest".into())),
                    };
                    if !bundled {
                        st.persisted.approved.insert(id.to_string(), perms);
                    }
                    st.persisted.disabled.remove(id);
                    sh.save_persisted(st)?;
                    waits.extend(sh.stop_plugin(st, id));
                    self.start_if_eligible(st, id);
                    Ok(())
                }
                PluginCommand::Restart => {
                    waits.extend(sh.stop_plugin(st, id));
                    match st.plugins.get(id).and_then(|e| e.idle_status(&st.persisted, id)) {
                        None => {
                            self.start_if_eligible(st, id);
                            Ok(())
                        }
                        Some(PluginStatus::Disabled) => Err(HostError::Invalid("plugin is disabled".into())),
                        Some(PluginStatus::NeedsApproval) => Err(HostError::Invalid("plugin needs approval".into())),
                        Some(PluginStatus::Unsupported) => {
                            Err(HostError::Invalid("plugin has no entry for this OS".into()))
                        }
                        Some(_) => Err(HostError::Invalid("plugin has an invalid manifest".into())),
                    }
                }
                PluginCommand::Uninstall => {
                    if bundled {
                        return Err(HostError::Invalid("bundled plugins cannot be uninstalled".into()));
                    }
                    waits.extend(sh.stop_plugin(st, id));
                    uninstall_dir = Some(dir);
                    Ok(())
                }
            }
        };
        for d in &waits {
            d.wait(Duration::from_secs(3));
        }
        match uninstall_dir {
            Some(dir) if result.is_ok() => self.finish_uninstall(id, dir),
            _ => result,
        }
    }

    fn finish_uninstall(&self, id: &str, dir: PathBuf) -> Result<(), HostError> {
        let sh = &self.shared;
        let inside = match (dir.canonicalize(), sh.user_dir.canonicalize()) {
            (Ok(d), Ok(u)) => d.starts_with(u) && d != sh.user_dir,
            _ => false,
        };
        if !inside {
            return Err(HostError::Invalid("plugin folder is not inside the user plugin folder".into()));
        }
        std::fs::remove_dir_all(&dir).map_err(io_err)?;
        {
            let mut st = sh.lock();
            st.persisted.approved.remove(id);
            st.persisted.disabled.remove(id);
            sh.save_persisted(&st)?;
        }
        self.reload();
        Ok(())
    }

    fn start_if_eligible(&self, st: &mut State, id: &str) {
        let eligible =
            st.plugins.get(id).is_some_and(|e| e.run.is_none() && e.idle_status(&st.persisted, id).is_none());
        if eligible {
            self.shared.start_plugin(st, id);
        } else if let Some(e) = st.plugins.get_mut(id) {
            if e.run.is_none() {
                if let Some(s) = e.idle_status(&st.persisted, id) {
                    e.status = s;
                }
            }
        }
    }

    /// Rescan the dirs: start new approved plugins, stop removed ones, restart ones whose manifest changed.
    pub fn reload(&self) {
        let sh = &self.shared;
        let found = discover(&sh.bundled_dirs, &sh.user_dir);
        let mut waits = Vec::new();
        {
            let mut guard = sh.lock();
            let st = &mut *guard;
            if st.shutting_down {
                return;
            }
            let ids: Vec<String> = st.plugins.keys().cloned().collect();
            for id in ids {
                let same = match (st.plugins.get(&id), found.get(&id)) {
                    (Some(e), Some(d)) => e.dir == d.dir && e.bundled == d.bundled && e.manifest == d.manifest,
                    _ => false,
                };
                if !same {
                    waits.extend(sh.stop_plugin(st, &id));
                    st.plugins.remove(&id);
                }
            }
            for (id, d) in &found {
                st.plugins.entry(id.clone()).or_insert_with(|| Entry::from_discovered(d));
            }
            let ids: Vec<String> = st.plugins.keys().cloned().collect();
            for id in ids {
                self.start_if_eligible(st, &id);
            }
        }
        for d in &waits {
            d.wait(Duration::from_secs(3));
        }
    }

    /// Install a plugin from `.zip` bytes (≤ 50 MB unpacked, no path escapes; the manifest may sit at the root
    /// or in a single top folder) into `<user_dir>/<id>`, replacing an older copy. Returns its info; a new
    /// user plugin is `NeedsApproval`.
    pub fn install_zip(&self, zip: &[u8]) -> Result<PluginInfo, HostError> {
        let sh = &self.shared;
        std::fs::create_dir_all(&sh.user_dir).map_err(io_err)?;
        let tmp = sh.user_dir.join(format!(".install-{}", random_hex(6)));
        std::fs::create_dir(&tmp).map_err(io_err)?;
        let unpacked = crate::install::extract(zip, &tmp).and_then(|_| {
            let text = std::fs::read_to_string(tmp.join("manifest.json")).map_err(io_err)?;
            let m = Manifest::parse(&text).map_err(|e| HostError::Invalid(e.to_string()))?;
            crate::install::check_entry(&m, &tmp)?;
            Ok(m)
        });
        let manifest = match unpacked {
            Ok(m) => m,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
        };
        let id = manifest.id.clone();
        let target = sh.user_dir.join(&id);
        let mut waits = Vec::new();
        {
            let mut guard = sh.lock();
            let st = &mut *guard;
            if st.plugins.get(&id).is_some_and(|e| e.dir == target) {
                waits.extend(sh.stop_plugin(st, &id));
            }
            if !target.exists() {
                // A fresh install never inherits an old approval.
                st.persisted.approved.remove(&id);
                if let Err(e) = sh.save_persisted(st) {
                    tracing::warn!(target: "plugin", error = %e, "cannot save plugins.json");
                }
            }
        }
        for d in &waits {
            d.wait(Duration::from_secs(3));
        }
        let replaced = (|| {
            if target.exists() {
                std::fs::remove_dir_all(&target)?;
            }
            std::fs::rename(&tmp, &target)
        })();
        if let Err(e) = replaced {
            let _ = std::fs::remove_dir_all(&tmp);
            self.reload();
            return Err(io_err(e));
        }
        self.reload();
        self.list().into_iter().find(|p| p.id == id).ok_or(HostError::NotFound(id))
    }

    /// Last ≤ 200 log lines (stdout/stderr and `log` calls), oldest first.
    pub fn log(&self, id: &str) -> Option<Vec<String>> {
        self.shared.lock().plugins.get(id).map(|e| e.log.iter().cloned().collect())
    }

    /// A file inside the plugin folder (icons for the UI): canonical path must stay inside the folder.
    pub fn file(&self, id: &str, rel: &str) -> Option<PathBuf> {
        let dir = self.shared.lock().plugins.get(id)?.dir.clone();
        let root = dir.canonicalize().ok()?;
        let path = root.join(rel).canonicalize().ok()?;
        (path.starts_with(&root) && path.is_file()).then_some(path)
    }

    /// Foreground app changed: send `system.focusChanged` to every running plugin.
    pub fn focus_changed(&self, app: &str, title: &str) {
        self.shared.broadcast("system.focusChanged", serde_json::json!({"app": app, "title": title}));
    }

    /// Round 21: the phone player of a stream this plugin started was closed by the user: send the plugin
    /// `phone.playerClosed {session}`. Unknown / stopped plugins: dropped.
    pub fn player_closed(&self, plugin: &str, session: &str) {
        self.shared.send_to(plugin, "phone.playerClosed", serde_json::json!({"session": session}));
    }

    /// Close every connection, then kill what still runs after 2 s.
    pub fn shutdown(&self) {
        if let Some(t) = self.server.lock().unwrap_or_else(|e| e.into_inner()).take() {
            t.abort();
        }
        let mut waits = Vec::new();
        {
            let mut guard = self.shared.lock();
            let st = &mut *guard;
            st.shutting_down = true;
            let ids: Vec<String> = st.plugins.keys().cloned().collect();
            for id in ids {
                waits.extend(self.shared.stop_plugin(st, &id));
            }
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        for d in waits {
            d.wait(deadline.saturating_duration_since(std::time::Instant::now()));
        }
    }
}

impl PluginControl for PluginHost {
    fn event(&self, event: PluginEvent) {
        self.shared.event(event);
    }

    fn set_visible(&self, instances: Vec<PluginInstance>) {
        self.shared.set_visible(instances);
    }

    fn sample(&self, context: &str) -> Option<LiveSample> {
        self.shared.sample(context)
    }
}
