// SPDX-License-Identifier: GPL-3.0-or-later
//! OBS's own obs-websocket settings.
//!
//! File: `<obs config>/plugin_config/obs-websocket/config.json` where `<obs config>` is
//! - Windows: `%APPDATA%\obs-studio`
//! - Linux: `$XDG_CONFIG_HOME/obs-studio` (default `~/.config/obs-studio`), then the Flatpak path
//!   `~/.var/app/com.obsproject.Studio/config/obs-studio`; the first whose `obs-studio` dir exists wins.
//!
//! Shape (obs-websocket 5.x): `{"alerts_enabled":false,"auth_required":true,"first_load":false,
//! "server_enabled":true,"server_password":"…","server_port":4455}`. Unknown keys must be preserved on write.

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectedConfig {
    pub path: PathBuf,
    /// The file exists (false: OBS config dir exists but the websocket plugin never wrote its settings).
    pub exists: bool,
    pub server_enabled: bool,
    pub server_port: u16,
    pub auth_required: bool,
    /// Never serialized (the config API shows `has_password` only).
    #[serde(skip)]
    pub server_password: String,
}

const DEFAULT_PORT: u16 = 4455;

#[cfg(windows)]
fn obs_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("APPDATA")?).join("obs-studio");
    d.is_dir().then_some(d)
}

#[cfg(not(windows))]
fn obs_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".config")))?;
    let flatpak = home.map(|h| h.join(".var/app/com.obsproject.Studio/config"));
    [Some(xdg), flatpak].into_iter().flatten().map(|d| d.join("obs-studio")).find(|d| d.is_dir())
}

/// The settings file path when an OBS config dir exists (OBS installed and run at least once). None otherwise.
pub fn config_path() -> Option<PathBuf> {
    obs_dir().map(|d| d.join("plugin_config").join("obs-websocket").join("config.json"))
}

/// Read the settings at [`config_path`]. A missing file → `exists: false` with OBS defaults (disabled, 4455,
/// auth required, no password). None when there is no OBS config dir or the file is unparsable.
pub fn read() -> Option<DetectedConfig> {
    read_from(&config_path()?)
}

/// Same as [`read`] for an explicit path (tests).
pub fn read_from(path: &Path) -> Option<DetectedConfig> {
    let mut cfg = DetectedConfig {
        path: path.to_path_buf(),
        exists: false,
        server_enabled: false,
        server_port: DEFAULT_PORT,
        auth_required: true,
        server_password: String::new(),
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(cfg),
        Err(_) => return None,
    };
    let v: Value = serde_json::from_str(&text).ok()?;
    let o = v.as_object()?;
    cfg.exists = true;
    cfg.server_enabled = o.get("server_enabled").and_then(Value::as_bool).unwrap_or(false);
    cfg.server_port =
        o.get("server_port").and_then(Value::as_u64).and_then(|p| u16::try_from(p).ok()).unwrap_or(DEFAULT_PORT);
    cfg.auth_required = o.get("auth_required").and_then(Value::as_bool).unwrap_or(true);
    cfg.server_password = o.get("server_password").and_then(Value::as_str).unwrap_or_default().to_string();
    Some(cfg)
}

/// Switch OBS's websocket server on: `server_enabled = true`, `first_load = false`, `auth_required = true`, and
/// a new random 16-char alphanumeric `server_password` when it is empty; port stays (default 4455). Creates the
/// file (and the `plugin_config/obs-websocket` dirs) when missing; keeps unknown keys. Writes atomically
/// (temp file + rename). OBS reads this file at startup, so OBS must be restarted (or started) afterwards.
/// Err when there is no OBS config dir (OBS never ran on this account).
pub fn enable_server() -> Result<DetectedConfig, String> {
    let path = config_path().ok_or_else(|| "OBS Studio has not been run on this account yet".to_string())?;
    enable_server_at(&path)
}

/// Same as [`enable_server`] for an explicit path (tests).
pub fn enable_server_at(path: &Path) -> Result<DetectedConfig, String> {
    let mut obj: Map<String, Value> = match std::fs::read_to_string(path) {
        Ok(t) => match serde_json::from_str(&t) {
            Ok(Value::Object(o)) => o,
            _ => return Err(format!("{} is not a valid OBS settings file", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    obj.insert("server_enabled".into(), json!(true));
    obj.insert("first_load".into(), json!(false));
    obj.insert("auth_required".into(), json!(true));
    obj.entry("server_port").or_insert(json!(DEFAULT_PORT));
    if obj.get("server_password").and_then(Value::as_str).unwrap_or_default().is_empty() {
        obj.insert("server_password".into(), json!(crate::util::random_alnum(16)));
    }
    let text = serde_json::to_string_pretty(&Value::Object(obj)).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".notideck-tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })?;
    read_from(path).ok_or_else(|| "settings unreadable after write".to_string())
}
