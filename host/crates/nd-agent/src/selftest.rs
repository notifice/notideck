// SPDX-License-Identifier: GPL-3.0-or-later
//! `--selftest`: print host diagnostics as JSON.

use crate::instance;
use nd_platform::{native, AudioTarget};
use nd_transport::AdbClient;
use serde_json::{json, Value};
use std::time::Duration;

fn res<T: serde::Serialize, E: std::fmt::Display>(r: Result<T, E>) -> Value {
    match r {
        Ok(v) => json!(v),
        Err(e) => json!({ "error": e.to_string() }),
    }
}

/// Webcam diagnostics: driver registration, bundled DLL, H.264 decoder, shared memory.
fn webcam_report() -> Value {
    let info = crate::camera::webcam::install_info();
    let decoder = match nd_media::default_decoder() {
        Ok(d) => json!(d.name()),
        Err(e) => json!({ "error": e.to_string() }),
    };
    #[cfg(windows)]
    let shm_open = nd_vcam::ShmMapping::open().is_some();
    #[cfg(not(windows))]
    let shm_open = false;
    json!({
        "install_state": info.state,
        "dll_path": info.app_dll.display().to_string(),
        "dll_present": info.app_dll_present,
        "registered_path": info.registered_path.map(|p| p.display().to_string()),
        "decoder": decoder,
        "shm_open": shm_open,
    })
}

/// Collect the diagnostics report.
pub fn report() -> Value {
    let audio = native::audio();
    let audio_json = json!({
        "backend": audio.backend_name(),
        "output": { "volume": res(audio.volume(AudioTarget::Output)), "muted": res(audio.muted(AudioTarget::Output)) },
        "input": { "volume": res(audio.volume(AudioTarget::Input)), "muted": res(audio.muted(AudioTarget::Input)) },
    });

    let media = native::media();
    let media_json = json!({ "backend": media.backend_name(), "now_playing": res(media.now_playing()) });

    // CPU and network are deltas, so the first sample only primes the counters.
    let stats = native::stats();
    let _ = stats.snapshot();
    std::thread::sleep(Duration::from_millis(500));
    let stats_json = res(stats.snapshot());

    let client = AdbClient::new();
    let version = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
        .and_then(|rt| rt.block_on(client.version()).map_err(|e| e.to_string()));
    let adb_json = match version {
        Ok(v) => json!({ "path": client.path().display().to_string(), "version": v }),
        Err(e) => json!({ "path": client.path().display().to_string(), "error": e }),
    };

    let cfg_dir = nd_core::default_config_root();
    let lan_enabled = crate::settings::load(&cfg_dir).lan_enabled;
    let addrs = crate::pairing::host_addrs();
    let identity_json = match nd_transport::HostIdentity::load_or_create(&cfg_dir) {
        Ok(id) => json!({ "host_id": id.host_id(), "fingerprint": id.fingerprint().to_hex() }),
        Err(e) => json!({ "error": e.to_string() }),
    };
    // Starting and stopping a daemon proves the mDNS socket can be bound on this machine.
    let mdns_json = match mdns_sd::ServiceDaemon::new() {
        Ok(d) => {
            let _ = d.shutdown();
            json!({ "started": true })
        }
        Err(e) => json!({ "started": false, "error": e.to_string() }),
    };
    let webcam_json = webcam_report();
    let exe = std::env::current_exe().unwrap_or_else(|_| "notideck".into());
    let data = instance::data_dir();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "audio": audio_json,
        "media": media_json,
        "stats": stats_json,
        "adb": adb_json,
        "identity": identity_json,
        "lan_enabled": lan_enabled,
        "advertised_addrs": addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
        "mdns": mdns_json,
        "webcam": webcam_json,
        "keyboard_access": json!(native::keyboard_access()),
        "autostart_enabled": res(native::autostart(&exe, &["--minimized"]).is_enabled()),
        "data_dir": data.display().to_string(),
        "config_dir": nd_core::default_config_root().display().to_string(),
        "log_dir": crate::logging::log_dir(&data).display().to_string(),
    })
}

/// Print the report to stdout.
pub fn run() {
    println!("{}", serde_json::to_string_pretty(&report()).unwrap_or_default());
}
