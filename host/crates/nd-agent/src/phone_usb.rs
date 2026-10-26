// SPDX-License-Identifier: GPL-3.0-or-later
//! Setup wizard support (round 23): list USB phones through adb, check whether the phone app is
//! installed, and install the bundled APK. Parsing is kept in pure functions.

use nd_config::{BackendError, PhoneUsbStatus, UsbPhone};
use nd_transport::adb::{AdbClient, AdbDevice, AdbDeviceState};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Package id of the phone app.
pub const PACKAGE: &str = "dev.notideck";
/// Overrides the APK location.
pub const APK_ENV: &str = "NOTIDECK_APK";
/// APK file name next to the exe.
pub const APK_NAME: &str = "notideck-phone.apk";

/// Per-command timeout for the quick shell probes.
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// `adb install` timeout (the APK is pushed and verified on the phone).
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

/// APK search order: `NOTIDECK_APK`, `<exe dir>/notideck-phone.apk`, `<exe dir>/../share/notideck/notideck-phone.apk`.
pub fn apk_candidates(env: Option<&str>, exe_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(e) = env.map(str::trim).filter(|e| !e.is_empty()) {
        v.push(PathBuf::from(e));
    }
    if let Some(d) = exe_dir {
        v.push(d.join(APK_NAME));
        v.push(d.join("..").join("share").join("notideck").join(APK_NAME));
    }
    v
}

/// First existing candidate (`exists` reports whether a path is a file).
pub fn find_apk_from(candidates: &[PathBuf], exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates.iter().find(|p| exists(p)).cloned()
}

/// The bundled phone APK, if any.
pub fn find_apk() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let env = std::env::var(APK_ENV).ok();
    find_apk_from(&apk_candidates(env.as_deref(), exe_dir.as_deref()), &|p| p.is_file())
}

/// `adb shell pm list packages <filter>` output: is exactly `package` listed?
pub fn parse_pm_list(output: &str, package: &str) -> bool {
    output.lines().any(|l| l.trim().strip_prefix("package:").is_some_and(|p| p.trim() == package))
}

/// First `versionName=` in `adb shell dumpsys package <pkg>` output.
pub fn parse_version_name(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|l| l.trim().strip_prefix("versionName="))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// State name the API shows for an adb state.
pub fn state_name(s: &AdbDeviceState) -> String {
    match s {
        AdbDeviceState::Device => "device".into(),
        AdbDeviceState::Offline => "offline".into(),
        AdbDeviceState::Unauthorized => "unauthorized".into(),
        AdbDeviceState::Authorizing => "authorizing".into(),
        AdbDeviceState::Recovery => "recovery".into(),
        AdbDeviceState::Sideload => "sideload".into(),
        AdbDeviceState::Other(o) => o.clone(),
    }
}

/// A phone entry before the app probes. `adb devices -l` writes spaces in the model as `_`.
pub fn phone_from_device(d: &AdbDevice) -> UsbPhone {
    UsbPhone {
        serial: d.serial.clone(),
        model: d.model.as_deref().unwrap_or_default().replace('_', " "),
        state: state_name(&d.state),
        app_installed: None,
        app_version: None,
    }
}

async fn probe_app(adb: &AdbClient, serial: &str) -> (Option<bool>, Option<String>) {
    let listed = adb.run(&["-s", serial, "shell", "pm", "list", "packages", PACKAGE], PROBE_TIMEOUT).await;
    let Ok((true, out, _)) = listed else { return (None, None) };
    if !parse_pm_list(&out, PACKAGE) {
        return (Some(false), None);
    }
    let version = match adb.run(&["-s", serial, "shell", "dumpsys", "package", PACKAGE], PROBE_TIMEOUT).await {
        Ok((true, out, _)) => parse_version_name(&out),
        _ => None,
    };
    (Some(true), version)
}

/// `GET /api/phone/usb`. Blocking: call from a blocking thread inside the agent runtime.
pub fn status(no_adb: bool) -> PhoneUsbStatus {
    let apk_available = find_apk().is_some();
    if no_adb {
        return PhoneUsbStatus { adb_ok: false, apk_available, phones: Vec::new() };
    }
    let adb = AdbClient::new();
    let rt = tokio::runtime::Handle::current();
    let devices = match rt.block_on(adb.list_devices()) {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!("phone usb status: {e}");
            return PhoneUsbStatus { adb_ok: false, apk_available, phones: Vec::new() };
        }
    };
    let phones = devices
        .iter()
        .map(|d| {
            let mut p = phone_from_device(d);
            if d.state == AdbDeviceState::Device {
                (p.app_installed, p.app_version) = rt.block_on(probe_app(&adb, &d.serial));
            }
            p
        })
        .collect();
    PhoneUsbStatus { adb_ok: true, apk_available, phones }
}

/// `POST /api/phone/install`. Blocking.
pub fn install(no_adb: bool, serial: &str) -> Result<(), BackendError> {
    let Some(apk) = find_apk() else {
        return Err(BackendError::NotFound("this copy of Notideck does not include the phone app".into()));
    };
    if no_adb {
        return Err(BackendError::NotFound(format!("no phone '{serial}' (adb is off)")));
    }
    let adb = AdbClient::new();
    let rt = tokio::runtime::Handle::current();
    let devices = rt.block_on(adb.list_devices()).map_err(|e| BackendError::Conflict(e.to_string()))?;
    let Some(dev) = devices.iter().find(|d| d.serial == serial) else {
        return Err(BackendError::NotFound(format!("no phone '{serial}' is connected")));
    };
    if dev.state != AdbDeviceState::Device {
        return Err(BackendError::Conflict(format!("the phone is '{}', not ready", state_name(&dev.state))));
    }
    let apk_str = apk.to_string_lossy().into_owned();
    match rt.block_on(adb.run(&["-s", serial, "install", "-r", &apk_str], INSTALL_TIMEOUT)) {
        Ok((true, out, _)) if out.contains("Success") => Ok(()),
        Ok((_, out, err)) => Err(BackendError::Conflict(install_failure_message(&out, &err))),
        Err(e) => Err(BackendError::Conflict(e.to_string())),
    }
}

/// adb's own words for why an install failed (stderr, else stdout), trimmed to one short block.
pub fn install_failure_message(stdout: &str, stderr: &str) -> String {
    let text = if stderr.trim().is_empty() { stdout } else { stderr };
    let msg: String =
        text.trim().lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("install failed").trim().into();
    msg.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nd_transport::adb::parse_devices_output;

    #[test]
    fn apk_lookup_order() {
        let exe = Path::new("/opt/nd/bin");
        let c = apk_candidates(Some("/x/custom.apk"), Some(exe));
        assert_eq!(
            c,
            vec![
                PathBuf::from("/x/custom.apk"),
                PathBuf::from("/opt/nd/bin/notideck-phone.apk"),
                PathBuf::from("/opt/nd/bin/../share/notideck/notideck-phone.apk"),
            ]
        );
        assert_eq!(apk_candidates(Some("  "), Some(exe)).len(), 2);
        // The env path wins when it exists, the exe-dir file next, the share dir last.
        let all = |_: &Path| true;
        assert_eq!(find_apk_from(&c, &all), Some(PathBuf::from("/x/custom.apk")));
        let no_env = |p: &Path| p != Path::new("/x/custom.apk");
        assert_eq!(find_apk_from(&c, &no_env), Some(PathBuf::from("/opt/nd/bin/notideck-phone.apk")));
        let share_only = |p: &Path| p.components().any(|c| c.as_os_str() == "share"); // separator-agnostic (Windows)
        assert_eq!(
            find_apk_from(&c, &share_only),
            Some(PathBuf::from("/opt/nd/bin/../share/notideck/notideck-phone.apk"))
        );
        assert_eq!(find_apk_from(&c, &|_| false), None);
    }

    #[test]
    fn devices_output_maps_states_and_models() {
        let out = "List of devices attached\n\
            TEST0SERIAL             device usb:1-1 product:F8331 model:F8331 device:F8331 transport_id:1\n\
            0123456789ABCDEF       unauthorized usb:1-2 transport_id:2\n\
            R58M12345              offline usb:1-3 product:x model:Pixel_7_Pro device:y transport_id:3\n\
            emulator-5554          device product:sdk model:sdk_gphone64 device:emu transport_id:4\n";
        let phones: Vec<_> = parse_devices_output(out).iter().map(phone_from_device).collect();
        assert_eq!(phones.len(), 4);
        assert_eq!((phones[0].state.as_str(), phones[0].model.as_str()), ("device", "F8331"));
        assert_eq!((phones[1].state.as_str(), phones[1].model.as_str()), ("unauthorized", ""));
        assert_eq!((phones[2].state.as_str(), phones[2].model.as_str()), ("offline", "Pixel 7 Pro"));
        assert_eq!(phones[3].model, "sdk gphone64");
        assert!(phones.iter().all(|p| p.app_installed.is_none() && p.app_version.is_none()));
    }

    #[test]
    fn pm_list_matches_the_exact_package() {
        assert!(parse_pm_list("package:dev.notideck\n", PACKAGE));
        assert!(parse_pm_list("package:dev.notideck.beta\r\npackage:dev.notideck\r\n", PACKAGE));
        assert!(!parse_pm_list("package:dev.notideck.beta\n", PACKAGE));
        assert!(!parse_pm_list("", PACKAGE));
        assert!(!parse_pm_list("error: closed\n", PACKAGE));
    }

    #[test]
    fn dumpsys_version_name() {
        let out = "Packages:\n  Package [dev.notideck] (abc):\n    userId=10123\n    versionCode=10000 minSdk=26\n    versionName=1.0.0\n    splits=[base]\n";
        assert_eq!(parse_version_name(out).as_deref(), Some("1.0.0"));
        assert_eq!(parse_version_name("versionName=\n"), None);
        assert_eq!(parse_version_name("nothing here"), None);
    }

    #[test]
    fn install_failure_uses_the_last_adb_line() {
        let err = "adb: failed to install x.apk: Failure [INSTALL_FAILED_VERSION_DOWNGRADE]\n";
        assert!(install_failure_message("", err).contains("INSTALL_FAILED_VERSION_DOWNGRADE"));
        assert_eq!(install_failure_message("Performing Streamed Install\nFailure [X]\n", ""), "Failure [X]");
        assert_eq!(install_failure_message("", ""), "install failed");
    }

    #[test]
    fn no_adb_gives_an_empty_status() {
        // `status(true)` never touches the runtime or adb.
        let s = status(true);
        assert!(!s.adb_ok && s.phones.is_empty());
    }
}
