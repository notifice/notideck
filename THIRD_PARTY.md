# Third-party software

Notideck is GPL-3.0-or-later. It uses and, in some builds, redistributes the components below under their own licences. All are compatible with GPL-3.0-or-later. This file is a summary, not legal advice. The authoritative licence text for each Rust crate is in its package, and `cargo metadata` lists every licence in the dependency tree.

## Bundled in the Windows package

| Component | Licence | Notes |
|---|---|---|
| Android platform-tools (`adb.exe`, `AdbWinApi.dll`, `AdbWinUsbApi.dll`) | Apache-2.0 (the Android SDK also carries other notices) | Downloaded unmodified from Google by `scripts/build-windows.sh`. The package ships `adb-NOTICE.txt` with the licence texts. |
| `WebView2Loader.dll` | Microsoft WebView2 SDK licence (BSD-3-Clause style, redistribution allowed) | Loads the Evergreen WebView2 runtime that Windows 11 already includes. The runtime itself is not bundled. |

## Host agent (Rust crates)

Direct dependencies of note. `Cargo.lock` pins exact versions.

| Crate | Licence | Used for |
|---|---|---|
| tokio, axum, tower, tracing, serde, serde_json, anyhow, thiserror, bytes, uuid, chrono, regex, base64, hex | MIT or Apache-2.0 | runtime, web server, utilities |
| prost, prost-build, protoc-bin-vendored | Apache-2.0 (protoc: BSD-3-Clause) | protobuf codegen |
| rustls, tokio-rustls | Apache-2.0 or ISC or MIT | TLS |
| ring | Apache-2.0 AND ISC (plus BoringSSL-derived code under OpenSSL/ISC terms) | crypto backend |
| rcgen, x509-parser, hmac, sha2, subtle, rand | MIT or Apache-2.0 | certificates, pairing |
| mdns-sd, if-addrs | MIT or Apache-2.0 | LAN discovery |
| qrcode, png | MIT or Apache-2.0 | pairing QR code, PNG icons |
| zune-jpeg | MIT or Apache-2.0 or Zlib | MPRIS album art |
| wry, tao | MIT or Apache-2.0 | native window |
| windows (windows-rs) | MIT or Apache-2.0 | Win32, WASAPI, GSMTC, Media Foundation |
| zbus, evdev, x11rb, libc | MIT / MIT or Apache-2.0 | Linux D-Bus (MPRIS), uinput, X11 |
| sysinfo, include_dir | MIT | system stats, embedded UI |
| openh264 + openh264-sys2 (optional feature) | BSD-2-Clause | H.264 decode fallback, built from source |

Licence families across the whole tree: MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unicode-3.0, Unlicense, CC0, and **MPL-2.0** for `cssparser`, `cssparser-macros`, `dtoa-short`, `option-ext` and `selectors` (pulled in by the web view stack). MPL-2.0 is file-level copyleft: we use these crates unmodified, so there is no extra obligation beyond keeping their licence and source availability. No crate in the tree is GPL-incompatible.

**H.264 and OpenH264.** Cisco's patent grant covers only the binaries Cisco builds. Notideck's optional `openh264` feature compiles the source, so it carries no Cisco patent licence. The default Windows path uses the OS's Media Foundation H.264 decoder instead, and the Android encoder is the phone's hardware MediaCodec.

## Android app

| Library | Licence |
|---|---|
| AndroidX (core, activity, lifecycle), Jetpack Compose + Material 3, CameraX | Apache-2.0 |
| Kotlin, kotlinx.coroutines | Apache-2.0 |
| Wire (protobuf runtime) | Apache-2.0 |
| Conscrypt (TLS 1.3 on Android 8-9) | Apache-2.0 |
| ZXing core (QR scanning) | Apache-2.0 |

## Config UI

The web UI in `host/ui/` is hand-written and has no vendored JavaScript or CSS libraries.

## Ideas and prior art

The design draws on Elgato Stream Deck (concept), scrcpy (the media-frame idea; no code copied), and OBS / obs-websocket (protocol used through its public API). Names and trademarks belong to their owners. Notideck is not affiliated with Elgato, Google, Microsoft or the OBS Project.
