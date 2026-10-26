# Notideck plugins (protocol 1)

Plugins add actions to Notideck keys, dials and sliders. A plugin is a separate program that can be written in
any language. The Notideck agent starts it and talks to it with JSON-RPC 2.0 over a local WebSocket. If a plugin
crashes, the agent keeps running and restarts the plugin.

Plugins run as your user, with your permissions. The permissions in the manifest are a statement of intent that
Notideck shows you before you approve a plugin. They are not a sandbox, so only install plugins you trust.

## 1. Package layout

```
dev.example.myplugin/        # folder name = plugin id (recommended)
  manifest.json
  myplugin.exe / myplugin    # or a script, see "entry"
  icons/                     # PNG or SVG, referenced from the manifest and from setImage
```

Notideck looks for plugins in two places:
- **Bundled** plugins ship with Notideck and are trusted. They live in `plugins/` next to `Notideck.exe` on
  Windows, and in `/usr/lib/notideck/plugins` on Linux. `NOTIDECK_PLUGINS_DIR` (a path list) replaces the
  bundled locations.
- **User** plugins go in `<config dir>/plugins/<id>/`. That's `%APPDATA%\Notideck\plugins` on Windows and
  `~/.config/notideck/plugins` on Linux. You can install one from a `.zip` on the config UI's Plugins page or
  copy the folder in yourself. A user plugin must be **approved** in the UI before it runs. If a user plugin has
  the same id as a bundled one, the user plugin wins.

## 2. manifest.json

```json
{
  "id": "dev.notideck.clock",
  "name": "Clock & Timer",
  "version": "1.0.0",
  "author": "Notideck",
  "description": "Live clock, countdown timer and stopwatch keys.",
  "icon": "icons/plugin.svg",
  "protocol": 1,
  "entry": { "windows": "clock.exe", "linux": "clock" },
  "permissions": [],
  "actions": [
    {
      "id": "timer",
      "name": "Countdown timer",
      "description": "Tap to start or pause, long-press to reset. Turn a dial to change the time.",
      "icon": "icons/timer.svg",
      "controls": ["key", "dial"],
      "triggers": ["key_down", "long_press", "dial_rotate", "dial_press"],
      "settings": [
        { "key": "minutes", "label": "Minutes", "type": "number", "min": 0, "max": 600, "step": 1, "default": 5 },
        { "key": "sound", "label": "Flash when done", "type": "toggle", "default": true }
      ]
    }
  ]
}
```

| Field | Rules |
|---|---|
| `id` | Required, 3–64 chars, `[a-z0-9]` plus `.`, `-` and `_`, starting with a letter. Reverse-DNS recommended. |
| `name`, `version` | Required. Version is free text, shown as is. |
| `author`, `description`, `icon` | Optional. `icon` is a path inside the plugin folder. |
| `protocol` | Required, must be `1`. |
| `entry` | Required. A string (all OSes) or an object with `windows` / `linux` / `default` keys. Each value is a path relative to the plugin folder, or an array `[program, args...]` whose program is either relative to the folder or a bare name looked up on `PATH`, e.g. `["node", "main.js"]`. A plugin without an entry for this OS shows as *unsupported*. |
| `permissions` | Any of `network`, `exec`, `filesystem`. Shown in the UI and on approval. |
| `actions` | 1–64 actions. `id` is unique within the plugin (`[a-z0-9_-]`), `name` required. |
| `actions[].controls` | Controls it fits: `key`, `dial`, `slider`, `tile`. Default `["key"]`. |
| `actions[].triggers` | The bindings the UI creates when you pick this action (the first one is the default). Trigger names are the same as for built-in actions: `key_down`, `key_up`, `long_press`, `double_tap`, `dial_rotate`, `dial_press`, `dial_release`, `slider_value`, `touch_start`, `touch_end`. Default `["key_down"]`. |
| `actions[].settings` | Form fields, see below. |
| `actions[].face` | Optional tile face the UI sets when this action is placed on a tile: `default`, `media` or `stream` (see §4, *Tile faces*). Keys, dials and sliders ignore it. |

**Setting fields** (`type`): `text`, `password`, `textarea`, `number` (`min`, `max`, `step`), `toggle`, and
`select` (`options: [{ "value", "label" }]`). Every field has a `key` (unique within the action) and a `label`,
and may have `default`, `placeholder` and `help`. The UI stores the values as one JSON object per placed action,
and the plugin receives it as `settings`. Fields you haven't edited hold their `default` (the UI writes defaults
when it creates the action).

### Icons

Draw monochrome glyphs as light grey/white strokes (for example `#E3E6EA`) on a transparent background, with a small
margin. Notideck recolours them for the theme and the key colour (light or dark ink as needed). Coloured icons with
transparent margins (logos) are shown as-is, fitted inside the key. Opaque images (photos, album art) fill the key.

## 3. Lifecycle

1. The agent starts the entry with the plugin folder as the working directory and these arguments:
   `--port <n> --token <t> --plugin-id <id>`. The same values are also set as the environment variables
   `NOTIDECK_PORT`, `NOTIDECK_TOKEN` and `NOTIDECK_PLUGIN_ID`. On Windows, no console window is created.
2. The plugin connects to `ws://127.0.0.1:<port>/` and, within 10 s, sends the request
   `register {pluginId, token, protocol: 1}`. A wrong token or id closes the connection.
3. The agent replies `{protocol: 1, agent: {version, os: "windows"|"linux"}}`, then sends `action.willAppear`
   for each of this plugin's actions currently on screen.
4. The plugin should exit when its WebSocket closes. The agent closes it on shutdown, on disable and before a
   restart, and kills the process if it is still running 2 s later.
5. If the plugin exits or disconnects, the agent restarts it after 1, 2, 4 … 30 s. After 5 crashes within
   2 minutes it stays stopped as *crashed*, until you restart it from the UI.
6. Anything the plugin writes to stdout or stderr, plus its `log` calls, goes into the agent log and the
   plugin's log view in the UI (the last 200 lines).

## 4. Messages

Plain JSON-RPC 2.0, one JSON object per WebSocket text frame, with no batches. The agent sends notifications only
(no `id`, so no reply is expected). The plugin may send its messages as notifications, or as requests (with an
`id`) if it wants a result or an error back.

**`action`** in every event is the bare action id from the manifest (e.g. `"timer"`), not prefixed with the plugin id.

**Context.** Every placed action has a `context` string, which is stable as long as the key exists. Use it to
keep per-key state. The same context can disappear and appear again, for example when you switch pages.

### Agent → plugin

| Method | Params |
|---|---|
| `action.willAppear` | `context, action, settings, controller ("key"/"dial"/"slider"/"tile"), state, value` |
| `action.willDisappear` | `context, action, settings` |
| `settings.didChange` | `context, action, settings` (you edited it in the UI; not sent for the plugin's own `setSettings`) |
| `key.down`, `key.up`, `key.longPress`, `key.doubleTap` | `context, action, settings` |
| `dial.rotate` | `context, action, settings, ticks` (signed, + = clockwise) |
| `dial.press`, `dial.release` | `context, action, settings` |
| `slider.change` | `context, action, settings, value` (0..1) |
| `touch.start`, `touch.end` | `context, action, settings` |
| `system.focusChanged` | `app` (exe name, e.g. `"Code.exe"`), `title` (window title, may be empty) |
| `phone.playerClosed` | `session` — the user closed the phone player that was showing the stream this plugin started with `phone.play {session}` (not sent when the plugin replaced or stopped it) |

An input event only reaches the plugin if a binding with that trigger exists on the control. The UI creates the
bindings from `actions[].triggers`.

### Plugin → agent

| Method | Params | Effect |
|---|---|---|
| `setTitle` | `context, title` | Key title. |
| `setSubtitle` | `context, subtitle` | Second text line (tiles and wide keys). |
| `setDetail` | `context, detail` | Third text line (`media` / `stream` tile faces). |
| `setImage` | `context, image` | Key image: a path inside the plugin folder (`icons/x.svg`), a `data:image/png;base64,…` / `data:image/svg+xml;base64,…` / `data:image/jpeg;base64,…` URL, or `""` for no image. PNG/JPEG ≤ 512 KB. SVG is rendered to 144×144. Pale single-colour glyphs (light grey on transparent) are tinted automatically on the phone's light themes. |
| `setState` | `context, state` | State index (the key's per-state look from the UI). |
| `setValue` | `context, value` | Slider or dial position, 0..1. |
| `showOk`, `showAlert` | `context` | Flashes a check mark or a warning on the key. |
| `setSettings` | `context, settings` | Saves a new settings object for this key (persisted in the profile). |
| `log` | `level ("debug"/"info"/"warn"/"error"), message` | Goes into the agent log and the UI log view. |
| `openUrl` | `url` | Opens an `http(s)` URL in the default browser. |
| `phone.play` | `url, title?, subtitle?, maxHeight?, muted?, syncDelayMs?, session?, context?` | Plays an `https` HLS stream (`.m3u8`) full screen in the phone app's built-in player (the active phone if it has the player, else the first one that does). `maxHeight` caps the video height (0 = adaptive). `muted` plays without sound. `syncDelayMs` > 0 holds the shown frame at the PC's wall-clock time minus that many ms, using the stream's `EXT-X-PROGRAM-DATE-TIME`, so a PC player that holds the same delay stays in step with it; 0 = play near the live edge. `session` is an opaque string sent back in `phone.playerClosed`. If no connected phone has the player, `context` (if given) flashes an alert. The phone fetches the stream over its own network. |
| `phone.stop` | | Closes the phone player. |

**Tile faces.** `stream` (round 20): the image is a full-bleed still under a dark scrim, title = channel name,
subtitle = stream title, detail = viewer count, state 1 = live (red LIVE pill), state 0 = offline (greyscale).
`media`: the now-playing layout (art left, title / subtitle / detail, value = progress).

Errors (only for requests): `-32601` unknown method, `-32602` bad params, `-32001` unknown context (not one of
this plugin's on-screen or recently seen contexts), `-32002` not allowed (e.g. a non-http URL).
The agent keeps the last title, subtitle, image, state and value per context while the plugin runs. They are
reapplied when the key comes back on screen. While a plugin is not running, its keys are shown dimmed.

Updates are cheap but keep them reasonable: at most about 10 per second per key. A clock that updates once a
second is fine.

## 5. Writing one in Rust

`host/crates/nd-plugin-sdk` handles the arguments, registration, reconnection policy (exit on close) and
message types. The bundled plugins in `host/plugins/` (clock, http, weather) are examples. In any other language,
all you need is a WebSocket client and JSON. Section 4 is the whole protocol.
