# Third-party software and thanks

Notideck stands on the work of many open-source projects. Thank you to everyone who builds and maintains them.

Notideck itself is GPL-3.0-or-later. It uses, and in its release packages redistributes, the components below under
their own licences. All of them are compatible with GPL-3.0-or-later. This file is a summary, not legal advice.

The full licence text of every library compiled into Notideck ships with each release package as
`THIRD_PARTY_LICENSES.txt`, and is attached to every GitHub release. `scripts/third-party-licenses.sh` generates it
from `Cargo.lock`.

## Bundled programs

| Component | Licence | Where |
|---|---|---|
| [Android platform-tools](https://developer.android.com/tools/releases/platform-tools) (`adb`) by Google | Apache-2.0 (plus the notices in `adb-NOTICE.txt`) | Windows and Linux packages. Downloaded unmodified from Google at build time. |
| `WebView2Loader.dll` from the [Microsoft WebView2 SDK](https://developer.microsoft.com/microsoft-edge/webview2/) | BSD-3-Clause style (text from the [Microsoft.Web.WebView2](https://www.nuget.org/packages/Microsoft.Web.WebView2) package, kept in `packaging/windows/WebView2-LICENSE.txt`) | Windows package. The WebView2 runtime itself is part of Windows and is not bundled. |
| [AppImage runtime](https://github.com/AppImage/type2-runtime), built in by [appimagetool](https://github.com/AppImage/appimagetool) | MIT | Linux AppImage |
| [NSIS](https://nsis.sourceforge.io/) installer stub | zlib/libpng | Windows setup program |

## PC app (Rust crates)

Direct dependencies. `Cargo.lock` pins the exact versions, and `THIRD_PARTY_LICENSES.txt` covers the whole tree.

| Crates | Licence | Used for |
|---|---|---|
| [tokio](https://tokio.rs), tokio-tungstenite, futures-util | MIT | async runtime, WebSocket (OBS, plugins) |
| [axum](https://github.com/tokio-rs/axum), tower, http-body-util | MIT | local config web server |
| [serde](https://serde.rs), serde_json | MIT or Apache-2.0 | settings and JSON |
| [tracing](https://github.com/tokio-rs/tracing), tracing-subscriber, tracing-appender | MIT | logs |
| anyhow, thiserror, bytes, uuid, chrono, regex, url, hex, base64, rand, tempfile | MIT and/or Apache-2.0 | utilities |
| [prost](https://github.com/tokio-rs/prost), prost-build | Apache-2.0 | protobuf messages |
| [protoc-bin-vendored](https://github.com/stepancheg/rust-protoc-bin-vendored) (Google's `protoc`) | MIT (protoc: BSD-3-Clause) | protobuf code generation at build time |
| [rustls](https://github.com/rustls/rustls), tokio-rustls | Apache-2.0 or ISC or MIT | TLS |
| [ring](https://github.com/briansmith/ring) | Apache-2.0 AND ISC (parts derived from BoringSSL / OpenSSL) | cryptography |
| [rcgen](https://github.com/rustls/rcgen), [x509-parser](https://github.com/rusticata/x509-parser) | MIT or Apache-2.0 | certificates |
| hmac, sha2 ([RustCrypto](https://github.com/RustCrypto)), subtle | MIT or Apache-2.0; subtle: BSD-3-Clause | pairing |
| webpki-roots | CDLA-Permissive-2.0 (Mozilla's CA list) | trusted roots for HTTPS |
| [mdns-sd](https://github.com/keepsimple1/mdns-sd), if-addrs | MIT or Apache-2.0; if-addrs: MIT or BSD-3-Clause | Wi-Fi discovery |
| [ureq](https://github.com/algesten/ureq) | MIT or Apache-2.0 | HTTP requests (site icons, plugins) |
| [qrcode](https://github.com/kennytm/qrcode-rust), [png](https://github.com/image-rs/image-png) | MIT or Apache-2.0 | pairing QR code, icons |
| [zune-jpeg](https://github.com/etemesi254/zune-image) | MIT or Apache-2.0 or Zlib | album art |
| [jpeg-encoder](https://github.com/vstroebel/jpeg-encoder) | (MIT or Apache-2.0) AND IJG | camera preview images |
| [resvg](https://github.com/linebender/resvg) | Apache-2.0 or MIT | plugin SVG icons |
| [zip](https://github.com/zip-rs/zip2) | MIT | plugin packages |
| [wry](https://github.com/tauri-apps/wry), [tao](https://github.com/tauri-apps/tao), [tray-icon](https://github.com/tauri-apps/tray-icon) (Tauri project), [gtk-rs](https://gtk-rs.org) | MIT and/or Apache-2.0 | app window and tray |
| [windows-rs](https://github.com/microsoft/windows-rs), embed-resource | MIT or Apache-2.0; embed-resource: MIT | Windows APIs (input, audio, media, virtual camera) |
| [zbus](https://github.com/dbus2/zbus), [evdev](https://github.com/emberian/evdev), [x11rb](https://github.com/psychon/x11rb), libc | MIT and/or Apache-2.0 | Linux D-Bus (MPRIS), keyboard input, X11 |
| [sysinfo](https://github.com/GuillaumeGomez/sysinfo), include_dir | MIT | system stats, embedded config UI |
| [openh264](https://github.com/ralfbiedert/openh264-rs) (optional feature, off by default) | BSD-2-Clause | H.264 decoding fallback, built from Cisco's source |

Licence families across the whole tree: MIT, Apache-2.0 (some with the LLVM exception), BSD-2/3-Clause, ISC, Zlib,
Unicode-3.0, Unlicense, CC0, 0BSD, BSL-1.0, CDLA-Permissive-2.0, IJG, and **MPL-2.0** for `cssparser`,
`cssparser-macros`, `dtoa-short`, `option-ext` and `selectors` (pulled in by the web view stack). MPL-2.0 is file-level
copyleft: Notideck uses these crates unmodified, and their source is available from crates.io. No crate in the tree is
GPL-incompatible.

**JPEG.** This software is based in part on the work of the Independent JPEG Group (via `jpeg-encoder`).

**H.264 and OpenH264.** Cisco's patent grant covers only the binaries Cisco builds. Notideck's optional `openh264`
feature compiles the source, so it carries no Cisco patent licence. The default Windows path uses the Media Foundation
H.264 decoder that ships with Windows, and the phone encodes with its own hardware encoder.

## Phone app (Android)

| Library | Licence | Used for |
|---|---|---|
| [AndroidX](https://developer.android.com/jetpack/androidx) core, activity, lifecycle | Apache-2.0 | app framework |
| [Jetpack Compose](https://developer.android.com/compose) and Material 3 | Apache-2.0 | user interface |
| [CameraX](https://developer.android.com/media/camera/camerax) | Apache-2.0 | QR code scanner camera |
| [Media3 ExoPlayer](https://github.com/androidx/media) (+ HLS, UI) | Apache-2.0 | built-in stream player |
| [Kotlin](https://kotlinlang.org) and [kotlinx.coroutines](https://github.com/Kotlin/kotlinx.coroutines) | Apache-2.0 | language and concurrency |
| [Wire](https://github.com/square/wire) by Square | Apache-2.0 | protobuf messages |
| [Conscrypt](https://github.com/google/conscrypt) | Apache-2.0 | TLS 1.3 on Android 8 and 9 |
| [ZXing](https://github.com/zxing/zxing) core | Apache-2.0 | QR code scanning |

## Config UI

The web UI in `host/ui/` is hand-written. It has no vendored JavaScript or CSS libraries, and its icons are drawn
for this project.

## Data and services

- **Weather plugin:** [Weather data by Open-Meteo.com](https://open-meteo.com/), licensed
  [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). City search uses the Open-Meteo geocoding API, which is
  based on [GeoNames](https://www.geonames.org/) (CC BY 4.0). Open-Meteo is free for non-commercial use.
- **Phone microphone on Windows** works with [VB-CABLE](https://vb-audio.com/Cable/) by VB-Audio (free download,
  not open source, not bundled).
- **OBS control** talks to [OBS Studio](https://obsproject.com/)'s built-in
  [obs-websocket](https://github.com/obsproject/obs-websocket) through its public protocol.

## Build tools

Releases are built with [Rust](https://www.rust-lang.org/) and Cargo, [mingw-w64](https://www.mingw-w64.org/),
[Gradle](https://gradle.org/), the Android SDK, NSIS and appimagetool, on GitHub Actions.

## Ideas and prior art

The design draws on Elgato Stream Deck (the concept) and [scrcpy](https://github.com/Genymobile/scrcpy) (the idea of
the media framing; no code copied). Names and trademarks belong to their owners. Notideck is not affiliated with
Elgato, Google, Microsoft, Cisco, VB-Audio, Open-Meteo or the OBS Project.
