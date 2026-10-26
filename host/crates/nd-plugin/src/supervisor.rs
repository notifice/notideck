// SPDX-License-Identifier: GPL-3.0-or-later
//! Process supervision: spawn, log capture, restart backoff, crash limit, graceful stop.

use crate::host::PluginStatus;
use crate::state::{Done, Shared};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::{watch, Notify};

/// Exits within this window count towards the crash limit.
pub(crate) const CRASH_WINDOW: Duration = Duration::from_secs(120);
pub(crate) const MAX_EXITS: usize = 5;
pub(crate) const KILL_AFTER: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestartPolicy {
    Restart(Duration),
    GiveUp,
}

/// Record an exit at `now` and decide what happens next: 1, 2, 4 … 30 s backoff by the number of exits in the
/// last 2 minutes; the 5th exit in that window gives up (`Crashed`).
pub(crate) fn restart_policy(exits: &mut VecDeque<Instant>, now: Instant) -> RestartPolicy {
    exits.push_back(now);
    while exits.front().is_some_and(|t| now.duration_since(*t) > CRASH_WINDOW) {
        exits.pop_front();
    }
    let n = exits.len();
    if n >= MAX_EXITS {
        return RestartPolicy::GiveUp;
    }
    let secs = (1u64 << (n - 1)).min(30);
    RestartPolicy::Restart(Duration::from_secs(secs))
}

/// What to run.
#[derive(Debug, Clone)]
pub(crate) struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

/// Replaceable in tests.
pub(crate) type Spawner = Arc<dyn Fn(&SpawnSpec) -> std::io::Result<tokio::process::Child> + Send + Sync>;

pub(crate) fn real_spawner() -> Spawner {
    Arc::new(|spec: &SpawnSpec| {
        let mut cmd = tokio::process::Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd.spawn()
    })
}

/// Resolve a manifest program: relative to the plugin folder if it has a separator or exists there, else a
/// bare name for `PATH`.
pub(crate) fn resolve_program(dir: &std::path::Path, prog: &str) -> PathBuf {
    let has_sep = prog.contains('/') || prog.contains('\\');
    let local = dir.join(prog);
    if has_sep || local.exists() {
        local
    } else {
        PathBuf::from(prog)
    }
}

/// Does the program a spec would run exist? A path (or a bare name found in the plugin folder) must be a file; a
/// bare name resolved for `PATH` must be found there.
pub(crate) fn program_exists(dir: &std::path::Path, prog: &str) -> bool {
    let resolved = resolve_program(dir, prog);
    if resolved.components().count() > 1 || resolved.is_absolute() {
        return resolved.is_file();
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(&resolved).is_file()))
}

/// A spawn error that retrying can't fix (the executable isn't there): the message to log, else `None`.
pub(crate) fn permanent_spawn_error(spec: &SpawnSpec, e: &std::io::Error) -> Option<String> {
    if e.kind() != std::io::ErrorKind::NotFound {
        return None;
    }
    let what = if spec.cwd.is_dir() { "its program" } else { "its folder" };
    Some(format!(
        "cannot start plugin: {what} was not found ({}); not retrying (reinstall the plugin or restart it from the UI)",
        if spec.cwd.is_dir() { spec.program.display() } else { spec.cwd.display() }
    ))
}

fn pipe_lines<R: AsyncRead + Unpin + Send + 'static>(shared: Arc<Shared>, id: String, stream: &'static str, r: R) {
    shared.handle.clone().spawn(async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::info!(target: "plugin", plugin = %id, stream, "{line}");
            shared.push_log(&id, stream, &line);
        }
    });
}

async fn wait_stop(rx: &mut watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            return;
        }
    }
}

async fn stop_child(child: &mut tokio::process::Child) {
    if tokio::time::timeout(KILL_AFTER, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

/// One supervisor per started plugin; ends on stop or when the plugin is given up on.
pub(crate) async fn supervise(
    shared: Arc<Shared>,
    id: String,
    run_gen: u64,
    mut stop: watch::Receiver<bool>,
    done: Arc<Done>,
) {
    let mut exits = VecDeque::new();
    'outer: loop {
        if *stop.borrow() {
            break;
        }
        let Some((spec, disconnected)) = shared.prepare_launch(&id, run_gen) else { break };
        match shared.spawner.as_ref().map(|s| s(&spec)) {
            Some(Ok(mut child)) => {
                if let Some(o) = child.stdout.take() {
                    pipe_lines(shared.clone(), id.clone(), "stdout", o);
                }
                if let Some(e) = child.stderr.take() {
                    pipe_lines(shared.clone(), id.clone(), "stderr", e);
                }
                let wait_disc: &Notify = &disconnected;
                tokio::select! {
                    r = child.wait() => {
                        shared.push_log(&id, "agent", &format!("plugin exited: {r:?}"));
                    }
                    _ = wait_stop(&mut stop) => {
                        stop_child(&mut child).await;
                        break 'outer;
                    }
                    _ = wait_disc.notified() => {
                        shared.push_log(&id, "agent", "plugin disconnected; waiting for it to exit");
                        stop_child(&mut child).await;
                    }
                }
            }
            Some(Err(e)) => {
                if let Some(msg) = permanent_spawn_error(&spec, &e) {
                    tracing::warn!(target: "plugin", plugin = %id, "{msg}");
                    shared.push_log(&id, "agent", &msg);
                    shared.on_exit(&id, run_gen);
                    shared.set_status(&id, run_gen, PluginStatus::Crashed);
                    break;
                }
                tracing::warn!(target: "plugin", plugin = %id, error = %e, "cannot start plugin");
                shared.push_log(&id, "agent", &format!("cannot start plugin: {e}"));
            }
            None => break,
        }
        shared.on_exit(&id, run_gen);
        match restart_policy(&mut exits, Instant::now()) {
            RestartPolicy::GiveUp => {
                shared.set_status(&id, run_gen, PluginStatus::Crashed);
                shared.push_log(&id, "agent", "plugin keeps crashing; stopped (restart it from the UI)");
                break;
            }
            RestartPolicy::Restart(d) => {
                shared.push_log(&id, "agent", &format!("restarting in {}s", d.as_secs()));
                tokio::select! {
                    _ = tokio::time::sleep(d) => {}
                    _ = wait_stop(&mut stop) => break,
                }
            }
        }
    }
    done.set();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_then_crash() {
        let t0 = Instant::now();
        let mut exits = VecDeque::new();
        let got: Vec<_> = (0..5).map(|i| restart_policy(&mut exits, t0 + Duration::from_secs(i * 3))).collect();
        assert_eq!(
            got,
            vec![
                RestartPolicy::Restart(Duration::from_secs(1)),
                RestartPolicy::Restart(Duration::from_secs(2)),
                RestartPolicy::Restart(Duration::from_secs(4)),
                RestartPolicy::Restart(Duration::from_secs(8)),
                RestartPolicy::GiveUp,
            ]
        );
    }

    #[test]
    fn old_exits_expire() {
        let t0 = Instant::now();
        let mut exits = VecDeque::new();
        for i in 0..4 {
            restart_policy(&mut exits, t0 + Duration::from_secs(i));
        }
        // 3 minutes later the window is empty again.
        assert_eq!(
            restart_policy(&mut exits, t0 + Duration::from_secs(200)),
            RestartPolicy::Restart(Duration::from_secs(1))
        );
    }

    #[test]
    fn missing_program_is_permanent() {
        let spec = SpawnSpec {
            program: PathBuf::from("/nonexistent-dir/demo"),
            args: vec![],
            cwd: std::env::temp_dir(),
            env: vec![],
        };
        let nf = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(permanent_spawn_error(&spec, &nf).unwrap().contains("/nonexistent-dir/demo"));
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(permanent_spawn_error(&spec, &denied).is_none());
    }

    #[test]
    fn program_exists_checks() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("demo.exe"), b"x").unwrap();
        assert!(program_exists(d.path(), "demo.exe"));
        assert!(!program_exists(d.path(), "demo"));
        assert!(!program_exists(d.path(), "bin/demo"));
        assert!(program_exists(d.path(), "sh") || cfg!(windows));
    }

    #[test]
    fn program_resolution() {
        let d = std::path::Path::new("/p");
        assert_eq!(resolve_program(d, "bin/x"), PathBuf::from("/p/bin/x"));
        assert_eq!(resolve_program(d, "node"), PathBuf::from("node"));
    }
}
