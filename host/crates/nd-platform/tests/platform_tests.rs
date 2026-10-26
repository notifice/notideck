// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(target_os = "linux")] // evdev / native::linux only exist on Linux

use nd_platform::native::linux::{char_to_key, to_evdev_key, DummyInputInjector};
use nd_platform::native::{input, launcher};
use nd_platform::{InputInjector, Key, KeyCombo};

#[test]
fn test_to_evdev_key_mappings() {
    assert_eq!(to_evdev_key(Key::A), evdev::KeyCode::KEY_A);
    assert_eq!(to_evdev_key(Key::Enter), evdev::KeyCode::KEY_ENTER);
    assert_eq!(to_evdev_key(Key::Ctrl), evdev::KeyCode::KEY_LEFTCTRL);
    assert_eq!(to_evdev_key(Key::MediaPlayPause), evdev::KeyCode::KEY_PLAYPAUSE);
}

#[test]
fn test_char_to_key_mappings() {
    assert_eq!(char_to_key('a'), Some((false, Key::A)));
    assert_eq!(char_to_key('A'), Some((true, Key::A)));
    assert_eq!(char_to_key('1'), Some((false, Key::Num1)));
    assert_eq!(char_to_key('!'), Some((true, Key::Num1)));
    assert_eq!(char_to_key(' '), Some((false, Key::Space)));
    assert_eq!(char_to_key('\n'), Some((false, Key::Enter)));
    assert_eq!(char_to_key('?'), Some((true, Key::Slash)));
}

#[test]
fn test_dummy_injector_records_combos_and_text() {
    let dummy = DummyInputInjector::new();
    let combo = KeyCombo { modifiers: vec![Key::Ctrl, Key::Shift], key: Key::P };
    dummy.key_combo(&combo).expect("dummy key_combo should succeed");
    assert_eq!(dummy.last_combo(), Some(combo));

    dummy.type_text("Hello").expect("dummy type_text should succeed");
    assert_eq!(dummy.last_text(), Some("Hello".to_string()));
}

#[test]
fn test_native_input_factory_fallback_does_not_panic() {
    // In unprivileged test environments (like WSL without udev rule),
    // native::input() must smoothly fall back to DummyInputInjector without error.
    let injector = input().expect("native::input() must succeed");
    let combo = KeyCombo { modifiers: vec![Key::Alt], key: Key::Tab };
    injector.key_combo(&combo).expect("combo injection should succeed");
    injector.type_text("abc").expect("typing should succeed");
}

#[test]
fn test_native_launcher_factory() {
    let l = launcher().expect("native::launcher() must succeed on Linux");
    // Test that launching an empty/failing target with args returns an error cleanly
    let res = l.launch("nonexistent_binary_xyz_12345", &["arg1".into()]);
    assert!(res.is_err());
}
