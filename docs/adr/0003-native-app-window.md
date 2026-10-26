# ADR 0003: Config UI lives in a native app window (embedded webview)

Status: accepted (2026-10-11). Supersedes the "opened in the browser" part of ADR 0001.

## Context
ADR 0001 made the config UI a local web SPA opened in the user's browser. The user wants Notideck to
feel like one desktop app: no browser tab, and everything (deck editing, devices, settings) done
inside the app.

## Decision
- `nd-agent` opens its own window using **tao + wry** (the windowing/webview layer under Tauri) and a
  **tray-icon** tray. The window hosts the same SPA, loaded from the agent's loopback server
  (`http://127.0.0.1:<ui_port>/?token=…`).
- Windows uses the system WebView2 runtime (preinstalled on Windows 11), so nothing extra is bundled.
- Linux keeps headless + browser fallback until the GUI build deps (webkit2gtk-4.1, gtk3,
  appindicator) are installed. Then the same shell is enabled with the `linux-gui` feature.
- We don't use the full Tauri framework: we already have an HTTP API, and we don't want its config,
  CLI or bundler in the build. Installers come in M10.
- The webview only navigates to the loopback UI. Every other URL opens in the system browser.
  Shell-only commands (open external link, hide window, quit) go over wry IPC.
- Closing the window hides it to the tray; "Quit" in the tray stops the agent.

## Consequences
- The UI stays plain HTML/CSS/JS, can still be tested with headless Chromium, and works in a browser
  for headless setups.
- The Windows build cross-compiles from WSL with mingw-w64 (`x86_64-pc-windows-gnu`).
