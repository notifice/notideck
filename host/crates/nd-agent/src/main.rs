// SPDX-License-Identifier: GPL-3.0-or-later
//! Notideck host agent (SPEC §3.2, ADR 0003).
//!
//! Default: window + tray (Windows, or Linux with `--features linux-gui`).
//! `--headless`: no GUI, prints the config UI URL and waits for Ctrl+C.

#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::Result;
use nd_agent::{app, instance, logging, selftest};
use tracing::{info, warn};

const HELP: &str = "\
Notideck host agent

USAGE: nd-agent [OPTIONS]

OPTIONS:
    --headless    Run without a window or tray; print the config UI URL
    --minimized   Start in the tray only (used by autostart)
    --open        Open the config UI in your browser once the agent is running (headless mode;
                  the window opens by itself otherwise). A second launch opens it too.
    --quit        Ask a running Notideck to quit, wait until it has, then exit
    --selftest    Print host diagnostics as JSON and exit
    --install-vcam    Install the Notideck Camera driver (run as administrator / root; the app's
                      Install webcam button does this for you)
    --uninstall-vcam  Remove the Notideck Camera driver (run as administrator / root)
    --console     Attach a console for log output (Windows)
    --help        Show this help

ENVIRONMENT:
    NOTIDECK_CONTROL_PORT, NOTIDECK_UI_PORT   override the listening ports
    NOTIDECK_NO_ADB                           do not use adb
    NOTIDECK_VCAM_FAKE_CONSUMER=WxH           pretend an app uses the camera (testing)
    RUST_LOG                                  log filter (default: info)
";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    headless: bool,
    minimized: bool,
    selftest: bool,
    open: bool,
    quit: bool,
    install_vcam: bool,
    uninstall_vcam: bool,
    console: bool,
    help: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> std::result::Result<Args, String> {
    let mut out = Args::default();
    for a in args {
        match a.as_str() {
            "--headless" => out.headless = true,
            "--minimized" => out.minimized = true,
            "--selftest" => out.selftest = true,
            "--open" => out.open = true,
            "--quit" => out.quit = true,
            "--console" => out.console = true,
            "--install-vcam" => out.install_vcam = true,
            "--uninstall-vcam" => out.uninstall_vcam = true,
            "--help" | "-h" => out.help = true,
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(out)
}

/// Windows GUI-subsystem processes have no console; attach the parent's (or make one).
/// Returns whether a console is available.
#[cfg(windows)]
fn attach_console() -> bool {
    use windows::Win32::System::Console::{
        AllocConsole, AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
    };
    // SAFETY: plain Win32 calls without pointers.
    unsafe {
        // stdout already redirected (pipe/file, e.g. `--selftest > out.json` or WSL interop): keep it,
        // attaching a console would swap the std handles away from the pipe.
        if GetStdHandle(STD_OUTPUT_HANDLE).is_ok_and(|h| !h.is_invalid() && !h.0.is_null()) {
            return true;
        }
        AttachConsole(ATTACH_PARENT_PROCESS).is_ok() || AllocConsole().is_ok()
    }
}

/// Native error dialog for startup failures.
#[cfg(windows)]
fn show_error(text: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    // SAFETY: HSTRINGs outlive the call; no owner window.
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("Notideck"), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
fn attach_console() -> bool {
    true
}

fn main() -> Result<()> {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            attach_console();
            eprintln!("{e}\n\n{HELP}");
            std::process::exit(2);
        }
    };
    let mut console = cfg!(not(windows));
    if args.console || args.selftest || args.help || args.quit || args.install_vcam || args.uninstall_vcam {
        console = attach_console();
    }
    if args.help {
        print!("{HELP}");
        return Ok(());
    }
    if args.selftest {
        selftest::run();
        return Ok(());
    }
    if args.install_vcam || args.uninstall_vcam {
        std::process::exit(nd_agent::camera::webcam::run_cli(args.install_vcam));
    }

    // Dev runs on overridden ports get their own instance lock.
    let suffix = std::env::var("NOTIDECK_UI_PORT").ok();
    // `--quit` only talks to the running instance: no second agent, no lock held.
    if args.quit {
        std::process::exit(instance::run_quit(&instance::data_dir(), suffix.as_deref(), QUIT_TIMEOUT));
    }
    let _lock = match instance::acquire(suffix.as_deref()) {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            // Another instance is running: ask it to show its window.
            let ok = instance::read_info(&instance::data_dir()).map(|info| instance::notify_primary(&info)).transpose();
            match ok {
                Ok(Some(_)) => {}
                Ok(None) => eprintln!("Notideck is already running."),
                Err(e) => eprintln!("Notideck is already running, but could not be reached: {e}"),
            }
            return Ok(());
        }
        Err(e) => {
            eprintln!("single-instance check failed: {e}; continuing");
            None
        }
    };

    let data_dir = instance::data_dir();
    let _log_guard = logging::init(&data_dir, console);
    info!("Starting Notideck Host Agent v{}", env!("CARGO_PKG_VERSION"));

    let opts = app::AgentOptions {
        control_port: env_port("NOTIDECK_CONTROL_PORT", nd_proto::CONTROL_PORT),
        ui_port: env_port("NOTIDECK_UI_PORT", nd_proto::CONFIG_UI_PORT),
        no_adb: std::env::var_os("NOTIDECK_NO_ADB").is_some(),
    };
    let handle = match app::start(opts) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("Agent failed to start: {e:#}");
            // A GUI-subsystem exe has no console: without this the app just silently vanishes.
            #[cfg(windows)]
            if !console {
                show_error(&format!("Notideck could not start.\n\n{e:#}"));
            }
            return Err(e);
        }
    };

    #[cfg(any(windows, feature = "linux-gui"))]
    if !args.headless && gui_display_available() {
        let show = !args.minimized && !handle.settings().start_minimized;
        return nd_agent::gui::run(handle, show);
    }

    info!("Config UI: {}", handle.ui_url());
    println!("Config UI: {}", handle.ui_url());
    info!("Notideck agent ready. Press Ctrl+C to terminate.");
    if args.open {
        open_in_browser(&handle.ui_url());
    }
    wait_for_exit(&handle);
    info!("Shutdown signal received");
    handle.shutdown();
    Ok(())
}

/// Windows always has a desktop; on Linux the window needs a Wayland or X11 display.
#[cfg(any(windows, feature = "linux-gui"))]
fn gui_display_available() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(windows))]
    {
        let set = |v| std::env::var_os(v).is_some_and(|x| !x.is_empty());
        let ok = set("WAYLAND_DISPLAY") || set("DISPLAY");
        if !ok {
            warn!("no WAYLAND_DISPLAY or DISPLAY: running headless");
        }
        ok
    }
}

/// How long `--quit` waits for the running instance to exit.
const QUIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Open `url` in the default browser (headless `--open`, and a second launch of a headless agent).
fn open_in_browser(url: &str) {
    #[cfg(windows)]
    let res = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd").args(["/C", "start", "", url]).creation_flags(0x0800_0000).spawn()
    };
    #[cfg(not(windows))]
    let res = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match res {
        // Reap in the background so the opener does not linger as a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => warn!("cannot open the browser: {e}; open {url} yourself"),
    }
}

/// Block the main thread until SIGINT / Ctrl+C or a quit request; a second launch opens the browser.
#[allow(dead_code)]
fn wait_for_exit(handle: &nd_agent::AgentHandle) {
    use nd_agent::ShellEvent;
    let mut events = handle.shell_events();
    let url = handle.ui_url();
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(async {
            let ctrl_c = tokio::signal::ctrl_c();
            tokio::pin!(ctrl_c);
            loop {
                tokio::select! {
                    _ = &mut ctrl_c => break,
                    ev = events.recv() => match ev {
                        Ok(ShellEvent::Quit) => break,
                        Ok(ShellEvent::Show) => open_in_browser(&url),
                        Ok(ShellEvent::PairingPending(_)) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => {
                            let _ = (&mut ctrl_c).await;
                            break;
                        }
                    },
                }
            }
        }),
        Err(e) => {
            warn!("cannot wait for Ctrl+C: {e}");
            loop {
                std::thread::park();
            }
        }
    }
}

/// Port from an env var, or `default` when unset or unparsable.
fn env_port(var: &str, default: u16) -> u16 {
    match std::env::var(var) {
        Ok(v) => v.parse().unwrap_or_else(|_| {
            tracing::warn!("{var}={v:?} is not a port; using {default}");
            default
        }),
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> std::result::Result<Args, String> {
        parse_args(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_flags() {
        assert_eq!(p(&[]).unwrap(), Args::default());
        let a = p(&["--headless", "--minimized"]).unwrap();
        assert!(a.headless && a.minimized && !a.selftest);
        assert!(p(&["--bogus"]).is_err());
        assert!(p(&["--install-vcam"]).unwrap().install_vcam);
        assert!(p(&["--uninstall-vcam"]).unwrap().uninstall_vcam);
        let a = p(&["--quit"]).unwrap();
        assert!(a.quit && !a.open && !a.headless);
        let a = p(&["--headless", "--open"]).unwrap();
        assert!(a.open && a.headless && !a.quit);
        assert!(HELP.contains("--quit") && HELP.contains("--open"));
    }
}
