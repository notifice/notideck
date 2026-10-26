// SPDX-License-Identifier: GPL-3.0-or-later
//! Native backend selection. M1 fills in `linux.rs` and `windows.rs`.

#[cfg(not(any(target_os = "linux", windows)))]
use crate::PlatformError;
use crate::{AudioControl, Autostart, InputInjector, KeyboardAccess, Launcher, MediaInfo, Result, SystemStats};

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::{to_evdev_key, DummyInputInjector, LinuxLauncher, UinputInjector};

#[cfg(target_os = "linux")]
pub mod apps_linux;
#[cfg(windows)]
pub mod apps_windows;
#[cfg(target_os = "linux")]
pub mod focus_kwin;
#[cfg(target_os = "linux")]
pub mod focus_linux;
#[cfg(windows)]
pub mod focus_windows;
#[cfg(target_os = "linux")]
pub(crate) mod icon_theme;
#[cfg(target_os = "linux")]
pub mod mic_linux;
#[cfg(windows)]
pub mod mic_windows;
#[cfg(windows)]
pub mod mixer_windows;
#[cfg(windows)]
pub(crate) mod win_icon;
#[cfg(windows)]
pub mod windows;

#[cfg(windows)]
pub use windows::{WindowsInputInjector, WindowsLauncher};

pub mod stats;
pub use stats::SysinfoStats;

/// Construct the best input injector for this OS.
///
/// On Linux, attempts to initialize [`UinputInjector`]; if `/dev/uinput` is inaccessible
/// (e.g. without udev rules or root permissions), it logs a warning and falls back to
/// [`DummyInputInjector`] so tests and dry runs do not fail.
pub fn input() -> Result<Box<dyn InputInjector>> {
    #[cfg(target_os = "linux")]
    {
        match UinputInjector::open() {
            Ok(injector) => {
                tracing::info!("Initialized Linux uinput virtual keyboard injector");
                Ok(Box::new(injector))
            }
            Err(err) => {
                tracing::warn!(
                    "Cannot initialize uinput virtual keyboard ({err}); falling back to dummy logging injector"
                );
                Ok(Box::new(DummyInputInjector::new()))
            }
        }
    }

    #[cfg(windows)]
    {
        tracing::info!("Initialized Windows SendInput injector");
        Ok(Box::new(WindowsInputInjector::new()))
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err(PlatformError::Unsupported("input injection not implemented yet on this OS"))
    }
}

/// Construct the launcher for this OS.
pub fn launcher() -> Result<Box<dyn Launcher>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(LinuxLauncher::new()))
    }

    #[cfg(windows)]
    {
        Ok(Box::new(WindowsLauncher::new()))
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err(PlatformError::Unsupported("launcher not implemented yet on this OS"))
    }
}

/// Construct the audio backend for this OS, or [`crate::NullAudio`] if none is reachable.
///
/// Linux: PulseAudio / PipeWire-pulse (`linux::PulseAudio`). Windows: WASAPI (`windows::WasapiAudio`).
pub fn audio() -> Box<dyn AudioControl> {
    #[cfg(target_os = "linux")]
    {
        match linux::PulseAudio::open() {
            Ok(a) => return Box::new(a),
            Err(err) => tracing::warn!("No audio backend ({err}); audio actions are disabled"),
        }
    }
    #[cfg(windows)]
    {
        match windows::WasapiAudio::open() {
            Ok(a) => return Box::new(a),
            Err(err) => tracing::warn!("No audio backend ({err}); audio actions are disabled"),
        }
    }
    #[allow(unreachable_code)]
    Box::new(crate::NullAudio)
}

/// Construct the per-app mixer backend (round 12, M4).
///
/// Linux: `pactl -f json` sink-inputs (`linux::PulseMixer`). Windows: WASAPI sessions (`mixer_windows::WasapiMixer`).
pub fn app_audio() -> Box<dyn crate::AppAudio> {
    #[cfg(target_os = "linux")]
    {
        match linux::PulseMixer::open() {
            Ok(a) => return Box::new(a),
            Err(err) => tracing::warn!("No per-app audio backend ({err}); the mixer is disabled"),
        }
    }
    #[cfg(windows)]
    {
        match mixer_windows::WasapiMixer::open() {
            Ok(a) => return Box::new(a),
            Err(err) => tracing::warn!("No per-app audio backend ({err}); the mixer is disabled"),
        }
    }
    #[allow(unreachable_code)]
    Box::new(crate::NullAppAudio)
}

/// Construct the installed-app catalog (round 13): Windows AppsFolder, Linux XDG desktop files.
pub fn app_catalog() -> Box<dyn crate::AppCatalog> {
    #[cfg(target_os = "linux")]
    {
        return Box::new(apps_linux::DesktopFiles::new());
    }
    #[cfg(windows)]
    {
        return Box::new(apps_windows::AppsFolder::new());
    }
    #[allow(unreachable_code)]
    Box::new(crate::NullAppCatalog)
}

/// Construct the now-playing source (MPRIS on Linux, GSMTC on Windows).
pub fn media() -> Box<dyn MediaInfo> {
    #[cfg(target_os = "linux")]
    {
        match linux::MprisMedia::open() {
            Ok(m) => return Box::new(m),
            Err(err) => tracing::warn!("No media backend ({err}); now-playing is disabled"),
        }
    }
    #[cfg(windows)]
    {
        match windows::GsmtcMedia::open() {
            Ok(m) => return Box::new(m),
            Err(err) => tracing::warn!("No media backend ({err}); now-playing is disabled"),
        }
    }
    #[allow(unreachable_code)]
    Box::new(crate::NullMedia)
}

/// Construct the focused-window source (SPEC §6.3). Filled in by round 8:
/// Windows `SetWinEventHook`; Linux picks Sway / Hyprland IPC from their env vars, else X11.
pub fn focus() -> Box<dyn crate::FocusWatcher> {
    #[cfg(target_os = "linux")]
    {
        return focus_linux::select();
    }
    #[cfg(windows)]
    {
        return Box::new(focus_windows::WindowsFocus);
    }
    #[allow(unreachable_code)]
    Box::new(crate::NullFocus)
}

/// Construct the phone-mic output (round 25): Windows WASAPI into a virtual cable
/// (`mic_windows::WasapiMicSink`), Linux PipeWire stream or `module-pipe-source` (`mic_linux::PipeSourceSink`).
pub fn mic_sink() -> Box<dyn crate::MicSink> {
    #[cfg(target_os = "linux")]
    {
        return Box::new(mic_linux::PipeSourceSink::new());
    }
    #[cfg(windows)]
    {
        return Box::new(mic_windows::WasapiMicSink::new());
    }
    #[allow(unreachable_code)]
    Box::new(crate::NullMicSink)
}

/// Construct the system stats sampler (`sysinfo`, all OSes).
pub fn stats() -> Box<dyn SystemStats> {
    Box::new(SysinfoStats::new())
}

/// Start-at-login registration that launches `exe` with `args`.
/// Implemented in `native/autostart.rs` (R4-3).
pub fn autostart(exe: &std::path::Path, args: &[&str]) -> Box<dyn Autostart> {
    autostart::create(exe, args)
}

pub mod autostart;
pub mod keyboard;

/// Current keyboard injection access (Linux: is `/dev/uinput` writable).
pub fn keyboard_access() -> KeyboardAccess {
    keyboard::access()
}

/// Try to grant keyboard access (Linux: `pkexec` installs the uaccess udev rule).
/// Blocks until the user answers the polkit prompt. `Unsupported` elsewhere.
pub fn fix_keyboard_access() -> Result<()> {
    keyboard::fix()
}
