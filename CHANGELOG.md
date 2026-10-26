# Changelog

All notable changes to Notideck are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-10-26

First public release. Notideck turns an old Android phone (Android 8 or newer) into a Stream Deck-style
controller and webcam for your Windows or Linux PC.

### Added

- **Deck on the phone.** A grid of keys, dials and sliders with live values (clock, CPU, memory, network,
  volume, now playing). Key presses, hotkeys, text typing, launching apps and commands, and volume and
  media control run on the PC.
- **USB and Wi-Fi.** Plug the phone in and it connects over USB with no setup, or pair over Wi-Fi by
  scanning a QR code. Every connection is encrypted and the phone has to be approved once on the PC.
- **Profiles, pages and templates.** Several profiles with multiple pages each, drag-and-drop editing in
  the browser-style config UI, an installed-app picker for launch actions, and ready-made page templates
  (media, OBS, per-app mixer, profile switcher, apps and more).
- **Automatic profile switching.** The deck follows the app you are using (by program, window class or
  title), with a "Home" action and a pause toggle.
- **Per-app volume mixer.** One slider per running app, with mute, on Windows and Linux.
- **Media control.** Now-playing tile with album art and progress, transport buttons, seek, shuffle,
  repeat and per-player volume.
- **OBS Studio.** Scenes, record, stream, mute and live status keys over OBS's built-in WebSocket, with
  no extra setup.
- **Phone as a webcam.** Camera image settings on the phone (focus, exposure, white balance,
  stabilisation, flash and more) from the Camera tab.
- **Plugins.** Out-of-process plugins in any language (see `docs/PLUGINS.md`). Bundled: clock, HTTP
  requests and weather. Plugins can play an HLS stream on the phone (`phone.play`).
- **Phone look.** Side rail with Home, Back, page dots and up to three pinned keys, swipe between pages,
  and themes: Black (AMOLED), Graphite and light and dark pastel themes.
- **Installers and setup wizard.** Windows setup.exe and portable zip, Linux tarball (with
  `install.sh`) and AppImage. A first-run wizard installs the phone app over USB, connects the phone,
  picks a starter profile and sets up extras.

### Platforms

- **Windows 11 (x64):** everything above, including the webcam (virtual camera).
- **Windows 10 (x64):** deck, mixer, media and plugins. Webcam is not supported.
- **Linux (x86_64):** deck, mixer, media, OBS and plugins, over USB or Wi-Fi.
- **Phone:** Android 8.0 or newer.

### Known limitations

- The Linux webcam (v4l2loopback) is not available yet.
- Linux runs without its own window: the config UI opens in your web browser.
- The webcam does not work on Windows 10.
- The phone app is debug-signed, so it is installed from the bundled APK (not from an app store) and
  future updates must be installed over it from the same source.
