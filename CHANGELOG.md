# Changelog

All notable changes to Notideck are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- **Lower phone-microphone latency.** The PC keeps less audio buffered (30 ms on Windows instead of 60, growing
  only after dropouts), drains built-up delay by playing about 1% faster instead of letting it creep up to 150 ms
  and skip, and the phone sends 10 ms packets instead of 20 ms.

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
- **Phone as a microphone.** Use the phone mic in Discord, OBS, Zoom and more from the Microphone tab, with
  gain, mute and on-phone noise suppression. Linux: a "Notideck Microphone" input (PipeWire or PulseAudio).
  Windows: plays into the free [VB-CABLE](https://vb-audio.com/Cable/) (optional install, only needed for this feature); pick "CABLE Output" as
  your input. Notideck keeps your default speaker and mic from switching to the cable.
- **Linux webcam.** "Notideck Camera" through the v4l2loopback kernel module, set up once from the Camera tab.
- **Linux app window and tray**, and auto-switch on KDE Plasma Wayland, Sway and Hyprland as well as X11.
- **Camera preview tile and deck sleep.** A deck tile shows the phone camera (tap to preview, long press for
  full screen). The deck sleeps after 2 minutes idle (black screen, faint clock, first touch wakes it).
- **Plugins.** Out-of-process plugins in any language (see `docs/PLUGINS.md`). Bundled: clock, HTTP
  requests and weather. Plugins can play an HLS stream on the phone (`phone.play`).
- **Phone look.** Side rail with Home, Back, page dots and up to three pinned keys, swipe between pages,
  and themes: Black (AMOLED), Graphite and light and dark pastel themes.
- **Installers and setup wizard.** Windows setup.exe and portable zip, Linux tarball (with
  `install.sh`) and AppImage. A first-run wizard installs the phone app over USB, connects the phone,
  picks a starter profile and sets up extras.

### Platforms

- **Windows 11 (x64):** everything above, including the webcam (virtual camera). The phone microphone needs
  the free [VB-CABLE](https://vb-audio.com/Cable/) (optional, only for the microphone).
- **Windows 10 (x64):** deck, mixer, media, microphone and plugins. Webcam is not supported.
- **Linux (x86_64):** everything above, over USB or Wi-Fi. The webcam needs the v4l2loopback kernel module (optional, only for the webcam).
  Auto-switch works on KDE Plasma (Wayland), Sway, Hyprland and X11.
- **Phone:** Android 8.0 or newer.

### Known limitations

- Auto-switch does not work on GNOME Wayland yet.
- The phone microphone on Windows has not been tested on real hardware yet (it is tested on Linux).
- The Linux webcam has been tested with ffmpeg; browsers, OBS and Discord as readers are not tested yet.
- The webcam does not work on Windows 10.
- The phone app is debug-signed, so it is installed from the bundled APK (not from an app store) and
  future updates must be installed over it from the same source.
