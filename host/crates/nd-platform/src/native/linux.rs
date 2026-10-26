// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux platform backends: uinput keyboard injection, dummy fallback, and command launcher.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use evdev::KeyCode;

use crate::{InputInjector, Key, KeyCombo, Launcher, PlatformError, Result};

/// Converts our abstract [`Key`] into the corresponding Linux [`evdev::KeyCode`].
pub fn to_evdev_key(key: Key) -> KeyCode {
    match key {
        // Modifiers
        Key::Ctrl => KeyCode::KEY_LEFTCTRL,
        Key::Shift => KeyCode::KEY_LEFTSHIFT,
        Key::Alt => KeyCode::KEY_LEFTALT,
        Key::Meta => KeyCode::KEY_LEFTMETA,

        // Letters
        Key::A => KeyCode::KEY_A,
        Key::B => KeyCode::KEY_B,
        Key::C => KeyCode::KEY_C,
        Key::D => KeyCode::KEY_D,
        Key::E => KeyCode::KEY_E,
        Key::F => KeyCode::KEY_F,
        Key::G => KeyCode::KEY_G,
        Key::H => KeyCode::KEY_H,
        Key::I => KeyCode::KEY_I,
        Key::J => KeyCode::KEY_J,
        Key::K => KeyCode::KEY_K,
        Key::L => KeyCode::KEY_L,
        Key::M => KeyCode::KEY_M,
        Key::N => KeyCode::KEY_N,
        Key::O => KeyCode::KEY_O,
        Key::P => KeyCode::KEY_P,
        Key::Q => KeyCode::KEY_Q,
        Key::R => KeyCode::KEY_R,
        Key::S => KeyCode::KEY_S,
        Key::T => KeyCode::KEY_T,
        Key::U => KeyCode::KEY_U,
        Key::V => KeyCode::KEY_V,
        Key::W => KeyCode::KEY_W,
        Key::X => KeyCode::KEY_X,
        Key::Y => KeyCode::KEY_Y,
        Key::Z => KeyCode::KEY_Z,

        // Digits
        Key::Num0 => KeyCode::KEY_0,
        Key::Num1 => KeyCode::KEY_1,
        Key::Num2 => KeyCode::KEY_2,
        Key::Num3 => KeyCode::KEY_3,
        Key::Num4 => KeyCode::KEY_4,
        Key::Num5 => KeyCode::KEY_5,
        Key::Num6 => KeyCode::KEY_6,
        Key::Num7 => KeyCode::KEY_7,
        Key::Num8 => KeyCode::KEY_8,
        Key::Num9 => KeyCode::KEY_9,

        // Function keys
        Key::F1 => KeyCode::KEY_F1,
        Key::F2 => KeyCode::KEY_F2,
        Key::F3 => KeyCode::KEY_F3,
        Key::F4 => KeyCode::KEY_F4,
        Key::F5 => KeyCode::KEY_F5,
        Key::F6 => KeyCode::KEY_F6,
        Key::F7 => KeyCode::KEY_F7,
        Key::F8 => KeyCode::KEY_F8,
        Key::F9 => KeyCode::KEY_F9,
        Key::F10 => KeyCode::KEY_F10,
        Key::F11 => KeyCode::KEY_F11,
        Key::F12 => KeyCode::KEY_F12,
        Key::F13 => KeyCode::KEY_F13,
        Key::F14 => KeyCode::KEY_F14,
        Key::F15 => KeyCode::KEY_F15,
        Key::F16 => KeyCode::KEY_F16,
        Key::F17 => KeyCode::KEY_F17,
        Key::F18 => KeyCode::KEY_F18,
        Key::F19 => KeyCode::KEY_F19,
        Key::F20 => KeyCode::KEY_F20,
        Key::F21 => KeyCode::KEY_F21,
        Key::F22 => KeyCode::KEY_F22,
        Key::F23 => KeyCode::KEY_F23,
        Key::F24 => KeyCode::KEY_F24,

        // Editing / navigation
        Key::Enter => KeyCode::KEY_ENTER,
        Key::Escape => KeyCode::KEY_ESC,
        Key::Backspace => KeyCode::KEY_BACKSPACE,
        Key::Tab => KeyCode::KEY_TAB,
        Key::Space => KeyCode::KEY_SPACE,
        Key::Delete => KeyCode::KEY_DELETE,
        Key::Insert => KeyCode::KEY_INSERT,
        Key::Home => KeyCode::KEY_HOME,
        Key::End => KeyCode::KEY_END,
        Key::PageUp => KeyCode::KEY_PAGEUP,
        Key::PageDown => KeyCode::KEY_PAGEDOWN,
        Key::Up => KeyCode::KEY_UP,
        Key::Down => KeyCode::KEY_DOWN,
        Key::Left => KeyCode::KEY_LEFT,
        Key::Right => KeyCode::KEY_RIGHT,
        Key::PrintScreen => KeyCode::KEY_SYSRQ,
        Key::CapsLock => KeyCode::KEY_CAPSLOCK,

        // Punctuation
        Key::Minus => KeyCode::KEY_MINUS,
        Key::Equal => KeyCode::KEY_EQUAL,
        Key::LeftBracket => KeyCode::KEY_LEFTBRACE,
        Key::RightBracket => KeyCode::KEY_RIGHTBRACE,
        Key::Backslash => KeyCode::KEY_BACKSLASH,
        Key::Semicolon => KeyCode::KEY_SEMICOLON,
        Key::Apostrophe => KeyCode::KEY_APOSTROPHE,
        Key::Grave => KeyCode::KEY_GRAVE,
        Key::Comma => KeyCode::KEY_COMMA,
        Key::Period => KeyCode::KEY_DOT,
        Key::Slash => KeyCode::KEY_SLASH,

        // Media
        Key::MediaPlayPause => KeyCode::KEY_PLAYPAUSE,
        Key::MediaNext => KeyCode::KEY_NEXTSONG,
        Key::MediaPrev => KeyCode::KEY_PREVIOUSSONG,
        Key::MediaStop => KeyCode::KEY_STOPCD,
        Key::VolumeUp => KeyCode::KEY_VOLUMEUP,
        Key::VolumeDown => KeyCode::KEY_VOLUMEDOWN,
        Key::VolumeMute => KeyCode::KEY_MUTE,
    }
}

/// Helper mapping an ASCII character to a Key and an optional Shift modifier.
pub fn char_to_key(c: char) -> Option<(bool, Key)> {
    match c {
        'a'..='z' => {
            let idx = (c as u8 - b'a') as usize;
            const KEYS: [Key; 26] = [
                Key::A,
                Key::B,
                Key::C,
                Key::D,
                Key::E,
                Key::F,
                Key::G,
                Key::H,
                Key::I,
                Key::J,
                Key::K,
                Key::L,
                Key::M,
                Key::N,
                Key::O,
                Key::P,
                Key::Q,
                Key::R,
                Key::S,
                Key::T,
                Key::U,
                Key::V,
                Key::W,
                Key::X,
                Key::Y,
                Key::Z,
            ];
            Some((false, KEYS[idx]))
        }
        'A'..='Z' => {
            let idx = (c as u8 - b'A') as usize;
            const KEYS: [Key; 26] = [
                Key::A,
                Key::B,
                Key::C,
                Key::D,
                Key::E,
                Key::F,
                Key::G,
                Key::H,
                Key::I,
                Key::J,
                Key::K,
                Key::L,
                Key::M,
                Key::N,
                Key::O,
                Key::P,
                Key::Q,
                Key::R,
                Key::S,
                Key::T,
                Key::U,
                Key::V,
                Key::W,
                Key::X,
                Key::Y,
                Key::Z,
            ];
            Some((true, KEYS[idx]))
        }
        '0' => Some((false, Key::Num0)),
        '1' => Some((false, Key::Num1)),
        '2' => Some((false, Key::Num2)),
        '3' => Some((false, Key::Num3)),
        '4' => Some((false, Key::Num4)),
        '5' => Some((false, Key::Num5)),
        '6' => Some((false, Key::Num6)),
        '7' => Some((false, Key::Num7)),
        '8' => Some((false, Key::Num8)),
        '9' => Some((false, Key::Num9)),

        ')' => Some((true, Key::Num0)),
        '!' => Some((true, Key::Num1)),
        '@' => Some((true, Key::Num2)),
        '#' => Some((true, Key::Num3)),
        '$' => Some((true, Key::Num4)),
        '%' => Some((true, Key::Num5)),
        '^' => Some((true, Key::Num6)),
        '&' => Some((true, Key::Num7)),
        '*' => Some((true, Key::Num8)),
        '(' => Some((true, Key::Num9)),

        ' ' => Some((false, Key::Space)),
        '\t' => Some((false, Key::Tab)),
        '\n' | '\r' => Some((false, Key::Enter)),

        '-' => Some((false, Key::Minus)),
        '_' => Some((true, Key::Minus)),
        '=' => Some((false, Key::Equal)),
        '+' => Some((true, Key::Equal)),
        '[' => Some((false, Key::LeftBracket)),
        '{' => Some((true, Key::LeftBracket)),
        ']' => Some((false, Key::RightBracket)),
        '}' => Some((true, Key::RightBracket)),
        '\\' => Some((false, Key::Backslash)),
        '|' => Some((true, Key::Backslash)),
        ';' => Some((false, Key::Semicolon)),
        ':' => Some((true, Key::Semicolon)),
        '\'' => Some((false, Key::Apostrophe)),
        '"' => Some((true, Key::Apostrophe)),
        '`' => Some((false, Key::Grave)),
        '~' => Some((true, Key::Grave)),
        ',' => Some((false, Key::Comma)),
        '<' => Some((true, Key::Comma)),
        '.' => Some((false, Key::Period)),
        '>' => Some((true, Key::Period)),
        '/' => Some((false, Key::Slash)),
        '?' => Some((true, Key::Slash)),

        _ => None,
    }
}

/// Linux virtual keyboard using the kernel `/dev/uinput` subsystem.
pub struct UinputInjector {
    device: Mutex<evdev::uinput::VirtualDevice>,
}

impl UinputInjector {
    /// Attempts to open `/dev/uinput` and configure the virtual keyboard.
    pub fn open() -> Result<Self> {
        let uinput_path = Path::new("/dev/uinput");
        if !uinput_path.exists() {
            return Err(PlatformError::Unsupported("/dev/uinput does not exist"));
        }

        // Test file readability & writability before proceeding
        let test_open = std::fs::OpenOptions::new().read(true).write(true).open(uinput_path);
        if let Err(e) = test_open {
            return Err(match e.kind() {
                std::io::ErrorKind::PermissionDenied => {
                    PlatformError::PermissionDenied("/dev/uinput permission denied (is user in 'input' group?)".into())
                }
                _ => PlatformError::Io(e),
            });
        }

        // Collect all supported keys to expose to the OS
        let mut key_set = evdev::AttributeSet::<KeyCode>::new();
        const ALL_KEYS: &[Key] = &[
            Key::Ctrl,
            Key::Shift,
            Key::Alt,
            Key::Meta,
            Key::A,
            Key::B,
            Key::C,
            Key::D,
            Key::E,
            Key::F,
            Key::G,
            Key::H,
            Key::I,
            Key::J,
            Key::K,
            Key::L,
            Key::M,
            Key::N,
            Key::O,
            Key::P,
            Key::Q,
            Key::R,
            Key::S,
            Key::T,
            Key::U,
            Key::V,
            Key::W,
            Key::X,
            Key::Y,
            Key::Z,
            Key::Num0,
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
            Key::F1,
            Key::F2,
            Key::F3,
            Key::F4,
            Key::F5,
            Key::F6,
            Key::F7,
            Key::F8,
            Key::F9,
            Key::F10,
            Key::F11,
            Key::F12,
            Key::F13,
            Key::F14,
            Key::F15,
            Key::F16,
            Key::F17,
            Key::F18,
            Key::F19,
            Key::F20,
            Key::F21,
            Key::F22,
            Key::F23,
            Key::F24,
            Key::Enter,
            Key::Escape,
            Key::Backspace,
            Key::Tab,
            Key::Space,
            Key::Delete,
            Key::Insert,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
            Key::PrintScreen,
            Key::CapsLock,
            Key::Minus,
            Key::Equal,
            Key::LeftBracket,
            Key::RightBracket,
            Key::Backslash,
            Key::Semicolon,
            Key::Apostrophe,
            Key::Grave,
            Key::Comma,
            Key::Period,
            Key::Slash,
            Key::MediaPlayPause,
            Key::MediaNext,
            Key::MediaPrev,
            Key::MediaStop,
            Key::VolumeUp,
            Key::VolumeDown,
            Key::VolumeMute,
        ];

        for &k in ALL_KEYS {
            key_set.insert(to_evdev_key(k));
        }

        #[allow(deprecated)]
        let builder = evdev::uinput::VirtualDeviceBuilder::new()
            .map_err(PlatformError::Io)?
            .name("Notideck Virtual Keyboard")
            .with_keys(&key_set)
            .map_err(PlatformError::Io)?;

        let device = builder.build().map_err(PlatformError::Io)?;

        // Give the kernel and udev a short moment to settle
        thread::sleep(Duration::from_millis(50));

        Ok(Self { device: Mutex::new(device) })
    }
}

impl InputInjector for UinputInjector {
    fn key_combo(&self, combo: &KeyCombo) -> Result<()> {
        let mut dev = self.device.lock().map_err(|e| PlatformError::Other(format!("Mutex poisoned: {e}")))?;

        // Press modifiers in order
        for &m in &combo.modifiers {
            let evdev_key = to_evdev_key(m);
            let ev = evdev::InputEvent::new(evdev::EventType::KEY.0, evdev_key.0, 1);
            dev.emit(&[ev]).map_err(PlatformError::Io)?;
        }

        // Tap key down
        let evdev_key = to_evdev_key(combo.key);
        let ev_down = evdev::InputEvent::new(evdev::EventType::KEY.0, evdev_key.0, 1);
        dev.emit(&[ev_down]).map_err(PlatformError::Io)?;

        // Hold key briefly so target applications register the keystroke
        thread::sleep(Duration::from_millis(10));

        // Tap key up
        let ev_up = evdev::InputEvent::new(evdev::EventType::KEY.0, evdev_key.0, 0);
        dev.emit(&[ev_up]).map_err(PlatformError::Io)?;

        // Release modifiers in reverse order
        for &m in combo.modifiers.iter().rev() {
            let evdev_key = to_evdev_key(m);
            let ev = evdev::InputEvent::new(evdev::EventType::KEY.0, evdev_key.0, 0);
            dev.emit(&[ev]).map_err(PlatformError::Io)?;
        }

        Ok(())
    }

    fn type_text(&self, text: &str) -> Result<()> {
        for c in text.chars() {
            if let Some((shift, key)) = char_to_key(c) {
                let modifiers = if shift { vec![Key::Shift] } else { vec![] };
                self.key_combo(&KeyCombo { modifiers, key })?;
                thread::sleep(Duration::from_millis(2));
            } else {
                tracing::warn!(character = %c, "Skipping unsupported character in type_text");
            }
        }
        Ok(())
    }
}

/// Fallback injector that logs events and records them for test assertions.
#[derive(Debug, Default)]
pub struct DummyInputInjector {
    pub history_combos: Arc<Mutex<VecDeque<KeyCombo>>>,
    pub history_text: Arc<Mutex<VecDeque<String>>>,
}

impl DummyInputInjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn last_combo(&self) -> Option<KeyCombo> {
        self.history_combos.lock().ok()?.back().cloned()
    }

    pub fn last_text(&self) -> Option<String> {
        self.history_text.lock().ok()?.back().cloned()
    }
}

impl InputInjector for DummyInputInjector {
    fn key_combo(&self, combo: &KeyCombo) -> Result<()> {
        tracing::info!(?combo, "DummyInputInjector: injected key combo");
        if let Ok(mut hist) = self.history_combos.lock() {
            hist.push_back(combo.clone());
        }
        Ok(())
    }

    fn type_text(&self, text: &str) -> Result<()> {
        tracing::info!(text, "DummyInputInjector: typed text");
        if let Ok(mut hist) = self.history_text.lock() {
            hist.push_back(text.to_string());
        }
        Ok(())
    }
}

/// Wait for a detached child on a helper thread so it doesn't linger as a zombie.
fn reap(mut child: std::process::Child) {
    thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Linux launcher using `xdg-open` and direct process spawning.
#[derive(Debug, Default)]
pub struct LinuxLauncher;

impl LinuxLauncher {
    pub fn new() -> Self {
        Self
    }
}

impl Launcher for LinuxLauncher {
    fn open_url(&self, url: &str) -> Result<()> {
        tracing::info!(url, "Opening URL with xdg-open");
        // Never let a value be parsed by xdg-open as an option.
        if url.starts_with('-') {
            return Err(PlatformError::Other(format!("refusing URL that starts with '-': '{url}'")));
        }
        let mut cmd = Command::new("xdg-open");
        cmd.arg(url);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        reap(cmd.spawn().map_err(PlatformError::Io)?);
        Ok(())
    }

    fn launch(&self, target: &str, args: &[String]) -> Result<()> {
        tracing::info!(target, ?args, "Launching target");
        let mut cmd = Command::new(target);
        cmd.args(args);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        match cmd.spawn() {
            Ok(child) => {
                reap(child);
                Ok(())
            }
            Err(e) if args.is_empty() && !target.starts_with('-') => {
                // If direct execution fails and no args were given, fall back to xdg-open (e.g. for files / desktop apps)
                tracing::debug!("Direct execution of '{target}' failed ({e}), attempting xdg-open fallback");
                let mut fallback = Command::new("xdg-open");
                fallback.arg(target);
                fallback.stdin(std::process::Stdio::null());
                fallback.stdout(std::process::Stdio::null());
                fallback.stderr(std::process::Stdio::null());
                let child = fallback.spawn().map_err(|e2| {
                    PlatformError::Other(format!(
                        "Failed to spawn '{target}' directly ({e}) and fallback xdg-open failed ({e2})"
                    ))
                })?;
                reap(child);
                Ok(())
            }
            Err(e) => Err(PlatformError::Io(e)),
        }
    }
}

#[path = "linux_audio.rs"]
mod audio_impl;
#[path = "linux_mpris.rs"]
mod mpris_impl;
pub use audio_impl::{PulseAudio, PulseMixer};
pub use mpris_impl::MprisMedia;
