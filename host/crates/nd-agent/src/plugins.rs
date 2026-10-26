// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin host wiring (round 19, M9): where bundled plugins live, starting the host, and carrying out the
//! requests plugins send back (feedback, open-url, setSettings). Protocol: `docs/PLUGINS.md`.

use crate::AgentState;
use nd_core::plugin::PluginControl;
use nd_plugin::{HostConfig, HostEvent, PluginHost};
use nd_transport::ServerHandle;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};
use tracing::{info, warn};

/// `NOTIDECK_NO_PLUGINS=1` turns the plugin host off (`available: false` in the API).
pub const NO_PLUGINS_ENV: &str = "NOTIDECK_NO_PLUGINS";
/// Overrides the bundled plugin folders (a `PATH`-style list).
pub const PLUGINS_DIR_ENV: &str = "NOTIDECK_PLUGINS_DIR";

/// Bundled plugin roots, searched in order: `NOTIDECK_PLUGINS_DIR` if set; otherwise `<exe dir>/plugins`,
/// `<exe dir>/../lib/notideck/plugins` (Linux tarball / AppImage layout), on Linux
/// `/usr/lib/notideck/plugins`, and the dev fallback `target/plugins-staged` found by walking up
/// from the executable.
pub fn bundled_dirs(exe: &Path) -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os(PLUGINS_DIR_ENV) {
        return std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect();
    }
    let mut dirs = Vec::new();
    if let Some(dir) = exe.parent() {
        dirs.push(dir.join("plugins"));
        dirs.push(dir.join("..").join("lib").join("notideck").join("plugins"));
    }
    if cfg!(target_os = "linux") {
        dirs.push(PathBuf::from("/usr/lib/notideck/plugins"));
    }
    // Dev builds: `<repo>/host/target/{debug,release}/nd-agent` finds `<repo>/host/target/plugins-staged`.
    let staged = exe
        .ancestors()
        .skip(1)
        .filter(|a| a.file_name().is_some_and(|n| n == "target"))
        .map(|a| a.join("plugins-staged"))
        .find(|p| p.is_dir());
    dirs.extend(staged);
    dirs
}

/// A running plugin host plus the channel its requests arrive on.
pub struct Plugins {
    pub host: Arc<PluginHost>,
    pub events: mpsc::UnboundedReceiver<HostEvent>,
}

/// Start the host. Must run inside the tokio runtime. `None` when disabled or when it cannot start
/// (logged); the agent then runs with `NullPlugins`.
pub fn start(
    exe: &Path,
    config_dir: &Path,
    icons: nd_core::mixer::IconImporter,
    poke: &Arc<Notify>,
) -> Option<Plugins> {
    if std::env::var_os(NO_PLUGINS_ENV).is_some_and(|v| v != "0" && !v.is_empty()) {
        info!("{NO_PLUGINS_ENV} is set: plugins are off");
        return None;
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let poke = poke.clone();
    let config = HostConfig {
        bundled_dirs: bundled_dirs(exe),
        user_dir: config_dir.join("plugins"),
        state_file: config_dir.join("plugins.json"),
        icons,
        poke: Arc::new(move || poke.notify_one()),
        events: tx,
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    info!(bundled = ?config.bundled_dirs, "Starting the plugin host");
    match PluginHost::start(config) {
        Ok(host) => Some(Plugins { host, events: rx }),
        Err(e) => {
            warn!("cannot start the plugin host: {e}; plugins are unavailable");
            None
        }
    }
}

/// Carry out plugin requests until the channel closes.
pub async fn event_loop(
    mut rx: mpsc::UnboundedReceiver<HostEvent>,
    state: AgentState,
    server: ServerHandle,
    launcher: Arc<dyn nd_platform::Launcher>,
) {
    while let Some(ev) = rx.recv().await {
        match ev {
            HostEvent::Feedback { context, ok } => {
                let Some(control_id) = state.control_for_context(&context) else { continue };
                let kind = if ok { nd_proto::v1::FeedbackKind::ShowOk } else { nd_proto::v1::FeedbackKind::ShowAlert };
                let fb = nd_proto::v1::Feedback { kind: kind as i32, control_id, text: String::new() };
                if let Err(e) = server.send_feedback(fb).await {
                    tracing::debug!("Plugin feedback skipped: {e}");
                }
            }
            HostEvent::OpenUrl { url } => {
                let launcher = launcher.clone();
                let res = tokio::task::spawn_blocking(move || launcher.open_url(&url)).await;
                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => warn!("plugin open-url failed: {e}"),
                    Err(e) => warn!("plugin open-url task failed: {e}"),
                }
            }
            HostEvent::PlayOnPhone {
                plugin,
                context,
                url,
                title,
                subtitle,
                max_height,
                muted,
                sync_delay_ms,
                session,
            } => {
                let session_id = format!("{plugin}/{session}");
                let play =
                    nd_proto::v1::PlayStream { url, title, subtitle, max_height, muted, sync_delay_ms, session_id };
                if let Err(e) = server.play_stream(play).await {
                    warn!("plugin phone.play: no phone with the built-in player is connected ({e})");
                    if let Some(control_id) = context.as_deref().and_then(|c| state.control_for_context(c)) {
                        let fb = nd_proto::v1::Feedback {
                            kind: nd_proto::v1::FeedbackKind::ShowAlert as i32,
                            control_id,
                            text: String::new(),
                        };
                        let _ = server.send_feedback(fb).await;
                    }
                }
            }
            HostEvent::StopOnPhone => server.stop_stream().await,
            HostEvent::SetSettings { context, plugin, settings } => {
                let st = state.clone();
                let ctx = context.clone();
                match tokio::task::spawn_blocking(move || st.plugin_set_settings(&ctx, &plugin, &settings)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => warn!(%context, "cannot save plugin settings: {e}"),
                    Err(e) => warn!("plugin settings task failed: {e}"),
                }
            }
        }
    }
}

/// Stop the host without blocking the reactor (it waits for plugin processes on a condvar).
pub fn shutdown(host: &Arc<PluginHost>) {
    host.shutdown();
}

/// Coerce to the trait object nd-core uses.
pub fn as_control(host: &Arc<PluginHost>) -> Arc<dyn PluginControl> {
    host.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_fallback_finds_staged_plugins_next_to_the_target_dir() {
        let root = std::env::temp_dir().join(format!("nd-plugdirs-{}", std::process::id()));
        let staged = root.join("host/target/plugins-staged");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::create_dir_all(root.join("host/target/debug")).unwrap();
        let exe = root.join("host/target/debug/nd-agent");
        if std::env::var_os(PLUGINS_DIR_ENV).is_none() {
            let dirs = bundled_dirs(&exe);
            assert_eq!(dirs[0], root.join("host/target/debug/plugins"));
            assert_eq!(dirs[1], root.join("host/target/debug/../lib/notideck/plugins"));
            assert!(dirs.contains(&staged), "{dirs:?}");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
