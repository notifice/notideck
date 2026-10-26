# ADR 0001: Config UI is a browser SPA

- Status: accepted (2026-10-11)
- Resolves: SPEC §15.1

## Context
The agent needs a profile/layout editor. Options: a native window via Tauri, or a web SPA served by the agent.

## Decision
The agent serves a local single-page app and a small JSON/WebSocket API on `127.0.0.1:47801`
(`nd_proto::CONFIG_UI_PORT`). The user opens it in their normal browser.

- Bind to loopback only. Never `0.0.0.0`.
- Mutating API calls require a per-run token passed in the opened URL, to block drive-by requests from other
  web pages (CSRF / DNS rebinding). `Host` and `Origin` headers are checked.
- The SPA is static assets embedded in the agent binary.

## Consequences
- No extra runtime (WebView, Electron) to ship; one binary on Windows and Linux.
- A Tauri shell can load the same SPA later without changing the API.
- The agent must be running for the UI to be available.
