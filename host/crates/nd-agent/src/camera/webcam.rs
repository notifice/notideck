// SPDX-License-Identifier: GPL-3.0-or-later
//! Virtual camera plumbing: install state, the `--install-vcam` / `--uninstall-vcam` command
//! line, elevation for the "Install webcam" button (UAC on Windows, `pkexec` on Linux), and the
//! thread that keeps the MF virtual camera object (Windows) or the v4l2loopback sink (Linux)
//! alive. Everything is a harmless stub elsewhere.

use std::path::PathBuf;

/// Where `notideck_vcam.dll` ships: next to the executable.
pub fn app_dll_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("notideck"));
    exe.parent().map(|d| d.to_path_buf()).unwrap_or_default().join(nd_vcam::registration::DLL_NAME)
}

/// The agent's in-memory frame slot, handed to the Linux sink (Windows uses the DLL's section).
#[cfg(windows)]
pub type VcamMem = ();
#[cfg(not(windows))]
pub type VcamMem = nd_vcam::MemShm;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallInfo {
    /// `installed` | `not_installed` | `outdated` | `unsupported` | `module_missing` (Linux)
    pub state: &'static str,
    pub app_dll: PathBuf,
    pub app_dll_present: bool,
    pub registered_path: Option<PathBuf>,
}

#[cfg(windows)]
pub fn install_info() -> InstallInfo {
    use nd_vcam::InstallState;
    let app_dll = app_dll_path();
    let present = app_dll.is_file();
    let (state, registered_path) = match nd_vcam::install_state(&app_dll) {
        InstallState::NotInstalled => ("not_installed", None),
        InstallState::Installed { path } => ("installed", Some(path)),
        InstallState::Outdated { path } => ("outdated", Some(path)),
    };
    InstallInfo { state, app_dll, app_dll_present: present, registered_path }
}

/// Linux: how the loopback module state maps to the UI's install states.
#[cfg(target_os = "linux")]
fn linux_state_name(s: &nd_vcam::v4l2::LoopbackState) -> &'static str {
    use nd_vcam::v4l2::LoopbackState;
    match s {
        LoopbackState::ModuleMissing => "module_missing",
        LoopbackState::NotLoaded | LoopbackState::NoDevice => "not_installed",
        LoopbackState::Ready(_) => "installed",
    }
}

#[cfg(target_os = "linux")]
pub fn install_info() -> InstallInfo {
    let state = linux_state_name(&nd_vcam::v4l2::loopback_state());
    InstallInfo { state, app_dll: PathBuf::new(), app_dll_present: true, registered_path: None }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn install_info() -> InstallInfo {
    let app_dll = app_dll_path();
    InstallInfo { state: "unsupported", app_dll_present: app_dll.is_file(), app_dll, registered_path: None }
}

// ---------------------------------------------------------------------------- CLI

#[cfg(not(target_os = "linux"))]
fn log_path(verb: &str) -> PathBuf {
    crate::instance::data_dir().join(format!("vcam-{verb}.log"))
}

/// `--install-vcam` / `--uninstall-vcam`: run (elevated) from the app's "Install webcam" button
/// or by hand from an Administrator prompt. Prints the result, records it for the
/// non-elevated parent and returns the process exit code (0 ok, 1 failed, 2 unsupported).
pub fn run_cli(install: bool) -> i32 {
    let verb = if install { "install" } else { "uninstall" };
    let result = do_cli(install);
    let (text, code) = match result {
        Ok(msg) => (msg, 0),
        Err(CliError::Unsupported(m)) => (m, 2),
        Err(CliError::Failed(m)) => (m, 1),
    };
    println!("{text}");
    // On Linux the parent reads our stdout instead (pkexec runs us as root with another HOME).
    #[cfg(not(target_os = "linux"))]
    {
        let _ = std::fs::create_dir_all(crate::instance::data_dir());
        let _ = std::fs::write(log_path(verb), format!("{code}\n{text}\n"));
    }
    #[cfg(target_os = "linux")]
    let _ = verb;
    code
}

#[allow(dead_code)] // Failed: Windows only; Unsupported: other platforms
enum CliError {
    Unsupported(String),
    Failed(String),
}

#[cfg(windows)]
fn do_cli(install: bool) -> Result<String, CliError> {
    if install {
        let dll = app_dll_path();
        if !dll.is_file() {
            return Err(CliError::Failed(format!("{} was not found next to Notideck.exe.", dll.display())));
        }
        match nd_vcam::install(&dll) {
            Ok(p) => Ok(format!("Notideck Camera installed ({}).", p.display())),
            Err(e) => Err(CliError::Failed(format!("Installing the webcam failed: {e:#}"))),
        }
    } else {
        match nd_vcam::uninstall() {
            Ok(()) => Ok("Notideck Camera uninstalled.".into()),
            Err(e) => Err(CliError::Failed(format!("Uninstalling the webcam failed: {e:#}"))),
        }
    }
}

#[cfg(target_os = "linux")]
fn do_cli(install: bool) -> Result<String, CliError> {
    let res = if install { nd_vcam::v4l2::install() } else { nd_vcam::v4l2::uninstall() };
    res.map_err(|e| {
        CliError::Failed(format!("{} the webcam failed: {e:#}", if install { "Installing" } else { "Uninstalling" }))
    })
}

#[cfg(not(any(windows, target_os = "linux")))]
fn do_cli(_install: bool) -> Result<String, CliError> {
    Err(CliError::Unsupported("The Notideck webcam driver is only available on Windows in this version.".into()))
}

/// Message the elevated child left behind, for the parent's error text.
#[cfg(not(target_os = "linux"))]
fn child_message(verb: &str) -> Option<String> {
    let text = std::fs::read_to_string(log_path(verb)).ok()?;
    Some(text.split_once('\n').map(|x| x.1).unwrap_or(&text).trim().to_string()).filter(|s| !s.is_empty())
}

/// Start `Notideck.exe --install-vcam` (or `--uninstall-vcam`) through a UAC prompt and wait for
/// it. Blocking; call from a worker thread.
#[cfg(windows)]
pub fn run_elevated(install: bool) -> Result<(), String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let verb = if install { "install" } else { "uninstall" };
    let _ = std::fs::remove_file(log_path(verb));
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let (op, file, params) = (
        HSTRING::from("runas"),
        HSTRING::from(exe.as_os_str()),
        HSTRING::from(if install { "--install-vcam" } else { "--uninstall-vcam" }),
    );
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(op.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: `info` and the HSTRINGs it points to outlive the calls; the process handle is
    // closed exactly once.
    unsafe {
        if let Err(e) = ShellExecuteExW(&mut info) {
            // 0x800704C7 = ERROR_CANCELLED: the user declined the UAC prompt.
            return Err(if e.code().0 as u32 == 0x8007_04C7 {
                "Administrator permission was not granted.".into()
            } else {
                format!("Could not ask for administrator permission: {e}")
            });
        }
        let h = info.hProcess;
        if h.is_invalid() {
            return Err("The installer did not start.".into());
        }
        WaitForSingleObject(h, 180_000);
        let mut code = 1u32;
        let ok = GetExitCodeProcess(h, &mut code).is_ok();
        let _ = CloseHandle(h);
        if ok && code == 0 {
            Ok(())
        } else if ok && code == 259 {
            Err("The installer took too long.".into())
        } else {
            Err(child_message(verb).unwrap_or_else(|| format!("The installer failed (exit code {code}).")))
        }
    }
}

/// Exit-status mapping for `pkexec <exe> --install-vcam`. `out` is the child's stdout.
#[cfg(target_os = "linux")]
fn pkexec_result(code: Option<i32>, out: &str) -> Result<(), String> {
    let msg = out.trim();
    match code {
        Some(0) => Ok(()),
        Some(126) => Err("The administrator password was not entered, so nothing was changed.".into()),
        Some(127) => Err("Could not ask for the administrator password (is a polkit agent running?).".into()),
        Some(c) if msg.is_empty() => Err(format!("The installer failed (exit code {c}).")),
        _ if msg.is_empty() => Err("The installer was stopped.".into()),
        _ => Err(msg.to_string()),
    }
}

/// Start `notideck --install-vcam` (or `--uninstall-vcam`) as root through `pkexec` and wait for
/// it. Blocking; call from a worker thread.
#[cfg(target_os = "linux")]
pub fn run_elevated(install: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = std::process::Command::new("pkexec")
        .arg(&exe)
        .arg(if install { "--install-vcam" } else { "--uninstall-vcam" })
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "pkexec (polkit) is not installed, so Notideck cannot ask for the administrator password. Run \
                 `sudo notideck --install-vcam` in a terminal instead."
                    .to_string()
            } else {
                format!("Could not start pkexec: {e}")
            }
        })?;
    pkexec_result(out.status.code(), &String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn run_elevated(install: bool) -> Result<(), String> {
    let _ = install;
    Err("The Notideck webcam driver is only available on Windows in this version.".into())
}

// ---------------------------------------------------------------------------- virtual camera object

#[cfg(windows)]
mod vcam_host {
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Arc, Mutex};
    use tracing::{error, info};

    enum Cmd {
        Ensure,
        Remove,
    }

    /// Owns the `IMFVirtualCamera` on a dedicated COM thread for the app's lifetime.
    pub struct VcamHost {
        tx: Mutex<Sender<Cmd>>,
        error: Arc<Mutex<Option<String>>>,
    }

    impl VcamHost {
        pub fn spawn(_mem: Option<super::VcamMem>, _settings: super::super::SettingsFn) -> Self {
            let (tx, rx) = channel::<Cmd>();
            let error = Arc::new(Mutex::new(None));
            let err2 = error.clone();
            let _ = std::thread::Builder::new().name("nd-vcam".into()).spawn(move || {
                let mut cam: Option<nd_vcam::VirtualCamera> = None;
                for cmd in rx {
                    match cmd {
                        Cmd::Ensure if cam.is_none() => match nd_vcam::VirtualCamera::create_and_start() {
                            Ok(c) => {
                                info!("Notideck Camera registered with Media Foundation");
                                *err2.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                cam = Some(c);
                            }
                            Err(e) => {
                                error!("cannot start the virtual camera: {e:#}");
                                *err2.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("{e:#}"));
                            }
                        },
                        Cmd::Ensure => {}
                        Cmd::Remove => cam = None,
                    }
                }
            });
            VcamHost { tx: Mutex::new(tx), error }
        }
        /// Create and start the camera if it does not exist yet (needs the DLL installed).
        pub fn ensure(&self) {
            let _ = self.tx.lock().unwrap_or_else(|e| e.into_inner()).send(Cmd::Ensure);
        }
        pub fn remove(&self) {
            let _ = self.tx.lock().unwrap_or_else(|e| e.into_inner()).send(Cmd::Remove);
        }
        pub fn error(&self) -> Option<String> {
            self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }
}

#[cfg(target_os = "linux")]
mod vcam_host {
    use super::super::logic::user_mode;
    use super::super::SettingsFn;
    use nd_vcam::v4l2::{loopback_state, LoopbackState, OutputFormat, V4l2Sink};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tracing::{error, info};

    /// How often the loopback device is re-checked (it appears after an install, vanishes when the
    /// module is unloaded).
    const RECHECK: Duration = Duration::from_secs(5);

    enum Cmd {
        Ensure,
        Remove,
    }

    /// What to do with the sink after looking at the device and the settings.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Action {
        Keep,
        Drop,
        Start(PathBuf, OutputFormat),
        Restart(PathBuf, OutputFormat),
    }

    /// `current` = (device, format, readers) of the running sink.
    pub(super) fn decide(
        state: &LoopbackState,
        current: Option<(&Path, OutputFormat, u32)>,
        want: OutputFormat,
    ) -> Action {
        match (state, current) {
            (LoopbackState::Ready(dev), None) => Action::Start(dev.clone(), want),
            (LoopbackState::Ready(dev), Some((cur_dev, fmt, readers))) => {
                if cur_dev != dev.as_path() || (fmt != want && readers == 0) {
                    Action::Restart(dev.clone(), want)
                } else {
                    Action::Keep
                }
            }
            (_, Some(_)) => Action::Drop,
            (_, None) => Action::Keep,
        }
    }

    /// The output format for a `camera_mode` setting: its size capped at 1920x1080 and made even
    /// (fps at most 60), else the sink's default (1280x720@30).
    pub(super) fn output_format(mode: &str) -> OutputFormat {
        match user_mode(mode) {
            Some(m) => OutputFormat {
                width: (m.w.min(1920) & !1).max(2),
                height: (m.h.min(1080) & !1).max(2),
                fps: m.fps.clamp(1, 60),
            },
            None => OutputFormat::default(),
        }
    }

    /// Owns the `V4l2Sink` on its own thread for the app's lifetime.
    pub struct VcamHost {
        tx: Mutex<Sender<Cmd>>,
        error: Arc<Mutex<Option<String>>>,
    }

    impl VcamHost {
        pub fn spawn(mem: Option<super::VcamMem>, settings: SettingsFn) -> Self {
            let (tx, rx) = channel::<Cmd>();
            let error = Arc::new(Mutex::new(None));
            let err2 = error.clone();
            let set_err = move |e: Option<String>| *err2.lock().unwrap_or_else(|e| e.into_inner()) = e;
            let _ = std::thread::Builder::new().name("nd-vcam".into()).spawn(move || {
                let Some(mem) = mem else { return };
                let mut wanted = false;
                let mut sink: Option<(V4l2Sink, PathBuf)> = None;
                loop {
                    match rx.recv_timeout(RECHECK) {
                        Ok(Cmd::Ensure) => wanted = true,
                        Ok(Cmd::Remove) => {
                            wanted = false;
                            sink = None;
                            set_err(None);
                            continue;
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                    if !wanted {
                        continue;
                    }
                    let want = output_format(&settings().mode);
                    let cur = sink.as_ref().map(|(s, d)| (d.as_path(), s.format(), s.readers()));
                    match decide(&loopback_state(), cur, want) {
                        Action::Keep => {}
                        Action::Drop => {
                            info!("v4l2loopback device is gone; closing the Notideck Camera sink");
                            sink = None;
                        }
                        Action::Start(dev, fmt) | Action::Restart(dev, fmt) => {
                            sink = None; // close the old writer before reopening the device
                            match V4l2Sink::start(&dev, mem.clone(), fmt) {
                                Ok(s) => {
                                    info!(
                                        "Notideck Camera writing to {} ({}x{}@{})",
                                        dev.display(),
                                        fmt.width,
                                        fmt.height,
                                        fmt.fps
                                    );
                                    set_err(None);
                                    sink = Some((s, dev));
                                }
                                Err(e) => {
                                    error!("cannot start the v4l2 sink: {e:#}");
                                    set_err(Some(format!("{e:#}")));
                                }
                            }
                        }
                    }
                    if let Some((s, _)) = &sink {
                        set_err(s.error());
                    }
                }
            });
            VcamHost { tx: Mutex::new(tx), error }
        }
        /// Start the sink when the loopback device exists; the thread keeps re-checking.
        pub fn ensure(&self) {
            let _ = self.tx.lock().unwrap_or_else(|e| e.into_inner()).send(Cmd::Ensure);
        }
        pub fn remove(&self) {
            let _ = self.tx.lock().unwrap_or_else(|e| e.into_inner()).send(Cmd::Remove);
        }
        pub fn error(&self) -> Option<String> {
            self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn fmt(w: u32, h: u32, fps: u32) -> OutputFormat {
            OutputFormat { width: w, height: h, fps }
        }
        const DEV: &str = "/dev/video10";

        #[test]
        fn format_from_mode() {
            assert_eq!(output_format("auto"), OutputFormat::default());
            assert_eq!(output_format("1920x1080@30"), fmt(1920, 1080, 30));
            assert_eq!(output_format("3840x2160@60"), fmt(1920, 1080, 60));
            assert_eq!(output_format("1279x719@25"), fmt(1278, 718, 25));
            assert_eq!(output_format("640x480@120"), fmt(640, 480, 60));
        }

        #[test]
        fn decisions() {
            let ready = LoopbackState::Ready(DEV.into());
            let d = Path::new(DEV);
            let hd = fmt(1280, 720, 30);
            let fhd = fmt(1920, 1080, 30);
            assert_eq!(decide(&ready, None, hd), Action::Start(DEV.into(), hd));
            assert_eq!(decide(&LoopbackState::NoDevice, None, hd), Action::Keep);
            assert_eq!(decide(&LoopbackState::ModuleMissing, Some((d, hd, 0)), hd), Action::Drop);
            assert_eq!(decide(&LoopbackState::NotLoaded, Some((d, hd, 2)), hd), Action::Drop);
            assert_eq!(decide(&ready, Some((d, hd, 0)), hd), Action::Keep);
            assert_eq!(decide(&ready, Some((d, hd, 0)), fhd), Action::Restart(DEV.into(), fhd));
            assert_eq!(decide(&ready, Some((d, hd, 1)), fhd), Action::Keep, "readers block a size change");
            let other = Path::new("/dev/video11");
            assert_eq!(decide(&ready, Some((other, hd, 1)), hd), Action::Restart(DEV.into(), hd));
        }

        #[test]
        fn state_names() {
            use super::super::linux_state_name as n;
            assert_eq!(n(&LoopbackState::ModuleMissing), "module_missing");
            assert_eq!(n(&LoopbackState::NotLoaded), "not_installed");
            assert_eq!(n(&LoopbackState::NoDevice), "not_installed");
            assert_eq!(n(&LoopbackState::Ready(DEV.into())), "installed");
        }

        #[test]
        fn pkexec_codes() {
            use super::super::pkexec_result as r;
            assert!(r(Some(0), "ok").is_ok());
            assert!(r(Some(126), "").unwrap_err().contains("password"));
            assert!(r(Some(127), "").unwrap_err().contains("polkit"));
            assert_eq!(r(Some(1), "boom\n").unwrap_err(), "boom");
            assert!(r(Some(1), "").unwrap_err().contains("exit code 1"));
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod vcam_host {
    pub struct VcamHost;
    impl VcamHost {
        pub fn spawn(_mem: Option<super::VcamMem>, _settings: super::super::SettingsFn) -> Self {
            VcamHost
        }
        pub fn ensure(&self) {}
        pub fn remove(&self) {}
        pub fn error(&self) -> Option<String> {
            None
        }
    }
}

pub use vcam_host::VcamHost;
