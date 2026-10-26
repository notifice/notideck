// SPDX-License-Identifier: GPL-3.0-or-later
//! USB / adb device tracking and port reversal (`adb reverse tcp:47800 tcp:47800`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::{broadcast, watch};

use crate::error::{Result, TransportError};

/// State of an adb-connected device as reported by `adb devices -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdbDeviceState {
    Device,
    Offline,
    Unauthorized,
    Authorizing,
    Recovery,
    Sideload,
    Other(String),
}

impl AdbDeviceState {
    #[allow(clippy::should_implement_trait)] // infallible parse, public API: not a FromStr impl
    pub fn from_str(s: &str) -> Self {
        match s {
            "device" => Self::Device,
            "offline" => Self::Offline,
            "unauthorized" => Self::Unauthorized,
            "authorizing" => Self::Authorizing,
            "recovery" => Self::Recovery,
            "sideload" => Self::Sideload,
            other => Self::Other(other.to_string()),
        }
    }
}

/// An attached Android device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDevice {
    pub serial: String,
    pub state: AdbDeviceState,
    pub model: Option<String>,
    pub product: Option<String>,
    pub device: Option<String>,
    pub transport_id: Option<String>,
}

/// Events emitted when devices attach or detach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdbDeviceEvent {
    Attached(AdbDevice),
    Detached(String),
}

/// Parse the stdout from `adb devices -l`.
pub fn parse_devices_output(output: &str) -> Vec<AdbDevice> {
    let mut devices = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("List of devices attached") || trimmed.starts_with("* daemon") {
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let serial = parts[0].to_string();
        let state = AdbDeviceState::from_str(parts[1]);

        let mut model = None;
        let mut product = None;
        let mut device = None;
        let mut transport_id = None;

        for part in &parts[2..] {
            if let Some(val) = part.strip_prefix("model:") {
                model = Some(val.to_string());
            } else if let Some(val) = part.strip_prefix("product:") {
                product = Some(val.to_string());
            } else if let Some(val) = part.strip_prefix("device:") {
                device = Some(val.to_string());
            } else if let Some(val) = part.strip_prefix("transport_id:") {
                transport_id = Some(val.to_string());
            }
        }

        devices.push(AdbDevice { serial, state, model, product, device, transport_id });
    }

    devices
}

/// Pure candidate selection for the adb binary. `exists` reports whether a path is a file.
///
/// Order: `ADB_PATH` -> next to the running exe -> `%LOCALAPPDATA%\Android\Sdk\platform-tools`
/// -> `ANDROID_HOME` / `ANDROID_SDK_ROOT` `platform-tools` -> plain `adb` (PATH lookup).
pub fn resolve_adb_path_from(
    adb_path_env: Option<&str>,
    exe_dir: Option<&Path>,
    local_app_data: Option<&str>,
    android_home: Option<&str>,
    android_sdk_root: Option<&str>,
    exe_suffix: &str,
    exists: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    fn non_empty(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    if let Some(p) = non_empty(adb_path_env) {
        return PathBuf::from(p);
    }
    let name = format!("adb{exe_suffix}");
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe_dir {
        candidates.push(dir.join(&name));
    }
    if !exe_suffix.is_empty() {
        if let Some(l) = non_empty(local_app_data) {
            candidates.push(Path::new(l).join("Android").join("Sdk").join("platform-tools").join(&name));
        }
    }
    for root in [android_home, android_sdk_root] {
        if let Some(r) = non_empty(root) {
            candidates.push(Path::new(r).join("platform-tools").join(&name));
        }
    }
    candidates.into_iter().find(|p| exists(p)).unwrap_or_else(|| PathBuf::from("adb"))
}

/// Resolves the path to the adb binary.
pub fn resolve_adb_path() -> PathBuf {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let var = |k: &str| std::env::var(k).ok();
    resolve_adb_path_from(
        var("ADB_PATH").as_deref(),
        exe_dir.as_deref(),
        var("LOCALAPPDATA").as_deref(),
        var("ANDROID_HOME").as_deref(),
        var("ANDROID_SDK_ROOT").as_deref(),
        std::env::consts::EXE_SUFFIX,
        &|p| p.is_file(),
    )
}

/// adb server port Notideck talks to. 0 = adb's default (5037, or `ANDROID_ADB_SERVER_PORT`).
static SERVER_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// Private adb server port used when the default one is unusable.
pub const FALLBACK_SERVER_PORT: u16 = 5039;

/// Is this adb failure the default server port being held by something that isn't a working
/// adb server? Typical case: WSL forwards its own (Linux) adb server's 127.0.0.1:5037 to Windows,
/// so Windows adb either talks to a server that can't see USB or gets a reset connection.
fn is_broken_server(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("protocol fault") || s.contains("connection reset") || s.contains("server version")
}

/// Upper bound for one adb invocation. A wedged adb server (e.g. a stale forwarded port) must not
/// stall the device tracker forever.
const ADB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Run an adb command with [`ADB_TIMEOUT`]; the child is killed on timeout.
async fn run_with_timeout(mut cmd: Command) -> std::io::Result<std::process::Output> {
    cmd.kill_on_drop(true);
    match tokio::time::timeout(ADB_TIMEOUT, cmd.output()).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "adb timed out")),
    }
}

/// Build a tokio command for adb that never flashes a console window on Windows.
fn adb_command(path: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(path);
    let port = SERVER_PORT.load(std::sync::atomic::Ordering::Relaxed);
    if port != 0 {
        cmd.env("ANDROID_ADB_SERVER_PORT", port.to_string());
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

/// Client for running adb commands.
#[derive(Debug, Clone)]
pub struct AdbClient {
    adb_path: PathBuf,
}

impl Default for AdbClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AdbClient {
    pub fn new() -> Self {
        Self { adb_path: resolve_adb_path() }
    }

    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { adb_path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.adb_path
    }

    /// `adb version` first line, e.g. "Android Debug Bridge version 1.0.41".
    /// Errors when adb can't be executed (used for the "adb missing" problem).
    pub async fn version(&self) -> Result<String> {
        let mut cmd = adb_command(&self.adb_path);
        cmd.arg("version");
        let output = run_with_timeout(cmd)
            .await
            .map_err(|e| TransportError::Adb(format!("cannot run '{}': {e}", self.adb_path.display())))?;
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text.lines().next().unwrap_or_default().trim().to_string())
    }

    /// List connected devices via `adb devices -l`.
    pub async fn list_devices(&self) -> Result<Vec<AdbDevice>> {
        let mut cmd = adb_command(&self.adb_path);
        cmd.args(["devices", "-l"]);

        let output = run_with_timeout(cmd).await.map_err(|e| {
            TransportError::Adb(format!("Failed to execute '{} devices -l': {e}", self.adb_path.display()))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if is_broken_server(&stderr)
                && SERVER_PORT
                    .compare_exchange(
                        0,
                        FALLBACK_SERVER_PORT,
                        std::sync::atomic::Ordering::Relaxed,
                        std::sync::atomic::Ordering::Relaxed,
                    )
                    .is_ok()
            {
                tracing::warn!(
                    "adb server on the default port is unusable ({}); switching to a private adb server on port {FALLBACK_SERVER_PORT}",
                    stderr.trim()
                );
                return Box::pin(self.list_devices()).await;
            }
            return Err(TransportError::Adb(format!("adb devices failed: {stderr}")));
        }

        let text = String::from_utf8_lossy(&output.stdout);
        Ok(parse_devices_output(&text))
    }

    /// Run `adb <args...>` with its own `timeout` (the child is killed when it expires). Returns
    /// `(success, stdout, stderr)`; `Err` when adb cannot run or times out.
    pub async fn run(&self, args: &[&str], timeout: Duration) -> Result<(bool, String, String)> {
        let mut cmd = adb_command(&self.adb_path);
        cmd.args(args).kill_on_drop(true);
        let output = match tokio::time::timeout(timeout, cmd.output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Err(TransportError::Adb(format!("cannot run '{}': {e}", self.adb_path.display())));
            }
            Err(_) => return Err(TransportError::Adb(format!("adb {} timed out", args.join(" ")))),
        };
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }

    /// Run `adb [-s <serial>] reverse tcp:<remote_port> tcp:<local_port>`.
    pub async fn reverse(&self, serial: Option<&str>, remote_port: u16, local_port: u16) -> Result<()> {
        let mut cmd = adb_command(&self.adb_path);
        if let Some(s) = serial {
            cmd.args(["-s", s]);
        }
        let forward_arg = format!("tcp:{remote_port}");
        let target_arg = format!("tcp:{local_port}");
        cmd.args(["reverse", &forward_arg, &target_arg]);

        let output = run_with_timeout(cmd)
            .await
            .map_err(|e| TransportError::Adb(format!("Failed to execute adb reverse: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TransportError::Adb(format!("adb reverse failed: {stderr}")));
        }

        tracing::info!(
            serial = serial.unwrap_or("default"),
            remote_port,
            local_port,
            "Set up adb reverse port forwarding"
        );
        Ok(())
    }

    /// Remove port reversal via `adb [-s <serial>] reverse --remove tcp:<remote_port>`.
    pub async fn remove_reverse(&self, serial: Option<&str>, remote_port: u16) -> Result<()> {
        let mut cmd = adb_command(&self.adb_path);
        if let Some(s) = serial {
            cmd.args(["-s", s]);
        }
        let forward_arg = format!("tcp:{remote_port}");
        cmd.args(["reverse", "--remove", &forward_arg]);

        let output = run_with_timeout(cmd)
            .await
            .map_err(|e| TransportError::Adb(format!("Failed to execute adb reverse --remove: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TransportError::Adb(format!("adb reverse --remove failed: {stderr}")));
        }

        Ok(())
    }
}

/// Background tracker that polls adb devices and configures `adb reverse`.
pub struct AdbTracker {
    client: AdbClient,
    remote_port: u16,
    local_port: u16,
    poll_interval: Duration,
    reversed_serials: Arc<Mutex<HashSet<String>>>,
    event_tx: broadcast::Sender<AdbDeviceEvent>,
}

impl AdbTracker {
    pub fn new(remote_port: u16, local_port: u16) -> Self {
        Self::with_client(AdbClient::new(), remote_port, local_port)
    }

    pub fn with_client(client: AdbClient, remote_port: u16, local_port: u16) -> Self {
        let (event_tx, _) = broadcast::channel(32);
        Self {
            client,
            remote_port,
            local_port,
            poll_interval: Duration::from_millis(1000),
            reversed_serials: Arc::new(Mutex::new(HashSet::new())),
            event_tx,
        }
    }

    pub fn set_poll_interval(&mut self, interval: Duration) {
        self.poll_interval = interval;
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AdbDeviceEvent> {
        self.event_tx.subscribe()
    }

    /// Returns the serials of all currently reversed devices.
    pub fn reversed_devices(&self) -> Vec<String> {
        self.reversed_serials.lock().map(|set| set.iter().cloned().collect()).unwrap_or_default()
    }

    /// Spawns the tracker background task.
    ///
    /// Returns a [`watch::Sender<bool>`] that, when set to `true`, stops the background task.
    pub fn spawn(self) -> (tokio::task::JoinHandle<()>, watch::Sender<bool>) {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.poll_interval);

            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            tracing::info!("AdbTracker received shutdown signal");
                            break;
                        }
                    }
                    _ = interval.tick() => {
                        self.poll_once().await;
                    }
                }
            }
        });

        (handle, shutdown_tx)
    }

    /// Run a single poll pass.
    pub async fn poll_once(&self) {
        match self.client.list_devices().await {
            Ok(devices) => {
                let current_serials: HashSet<String> =
                    devices.iter().filter(|d| d.state == AdbDeviceState::Device).map(|d| d.serial.clone()).collect();

                let known_snapshot = {
                    match self.reversed_serials.lock() {
                        Ok(guard) => guard.clone(),
                        Err(e) => {
                            tracing::error!("AdbTracker mutex poisoned: {e}");
                            return;
                        }
                    }
                };

                // Detect detached devices
                let mut detached = Vec::new();
                for serial in &known_snapshot {
                    if !current_serials.contains(serial) {
                        detached.push(serial.clone());
                    }
                }

                if !detached.is_empty() {
                    if let Ok(mut guard) = self.reversed_serials.lock() {
                        for serial in &detached {
                            guard.remove(serial);
                        }
                    }
                    for serial in detached {
                        tracing::info!(serial = %serial, "ADB device detached");
                        let _ = self.event_tx.send(AdbDeviceEvent::Detached(serial));
                    }
                }

                // Detect new attached devices
                for dev in devices {
                    if dev.state == AdbDeviceState::Device && !known_snapshot.contains(&dev.serial) {
                        match self.client.reverse(Some(&dev.serial), self.remote_port, self.local_port).await {
                            Ok(()) => {
                                if let Ok(mut guard) = self.reversed_serials.lock() {
                                    guard.insert(dev.serial.clone());
                                }
                                tracing::info!(
                                    serial = %dev.serial,
                                    model = ?dev.model,
                                    "ADB device attached and reversed"
                                );
                                let _ = self.event_tx.send(AdbDeviceEvent::Attached(dev));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    serial = %dev.serial,
                                    "Failed to set adb reverse for device: {e}"
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::debug!("AdbTracker poll failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn resolve(env: Option<&str>, existing: &[&str], suffix: &str) -> PathBuf {
        let set: Vec<PathBuf> = existing.iter().map(PathBuf::from).collect();
        resolve_adb_path_from(
            env,
            Some(Path::new("/app")),
            Some("/lad"),
            Some("/home_sdk"),
            Some("/root_sdk"),
            suffix,
            &|p| set.iter().any(|e| e == p),
        )
    }

    #[test]
    fn env_override_wins() {
        assert_eq!(resolve(Some(" /x/adb "), &["/app/adb"], ""), PathBuf::from("/x/adb"));
        assert_eq!(resolve(Some("  "), &["/app/adb"], ""), PathBuf::from("/app/adb"));
    }

    #[test]
    fn order_exe_dir_then_localappdata_then_sdk() {
        let all = [
            "/app/adb.exe",
            "/lad/Android/Sdk/platform-tools/adb.exe",
            "/home_sdk/platform-tools/adb.exe",
            "/root_sdk/platform-tools/adb.exe",
        ];
        assert_eq!(resolve(None, &all, ".exe"), PathBuf::from("/app/adb.exe"));
        assert_eq!(resolve(None, &all[1..], ".exe"), PathBuf::from("/lad/Android/Sdk/platform-tools/adb.exe"));
        assert_eq!(resolve(None, &all[2..], ".exe"), PathBuf::from("/home_sdk/platform-tools/adb.exe"));
        assert_eq!(resolve(None, &all[3..], ".exe"), PathBuf::from("/root_sdk/platform-tools/adb.exe"));
    }

    #[test]
    fn falls_back_to_path_and_skips_localappdata_off_windows() {
        assert_eq!(resolve(None, &[], ""), PathBuf::from("adb"));
        assert_eq!(resolve(None, &["/lad/Android/Sdk/platform-tools/adb"], ""), PathBuf::from("adb"));
    }

    #[test]
    fn detects_broken_default_server() {
        assert!(super::is_broken_server(
            "adb.exe: failed to check server version: protocol fault (couldn't read status): connection reset"
        ));
        assert!(!super::is_broken_server("error: no devices/emulators found"));
    }
}
