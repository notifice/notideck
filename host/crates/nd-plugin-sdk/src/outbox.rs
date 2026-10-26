// SPDX-License-Identifier: GPL-3.0-or-later
use serde_json::{json, Map, Value};
use tokio::sync::mpsc::UnboundedSender;

/// Cheap, clonable handle for sending notifications to the agent. Sends never block or fail loudly:
/// once the socket is gone they are dropped (the run loop sees the close and ends).
#[derive(Debug, Clone)]
pub struct Outbox {
    tx: UnboundedSender<String>,
}

/// Encode a JSON-RPC notification.
pub(crate) fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

impl Outbox {
    pub(crate) fn new(tx: UnboundedSender<String>) -> Outbox {
        Outbox { tx }
    }

    /// An outbox that is not connected to an agent; the receiver yields the encoded JSON-RPC frames.
    /// For unit-testing plugin logic without a socket.
    pub fn detached() -> (Outbox, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Outbox { tx }, rx)
    }

    /// Send any method (escape hatch for protocol additions).
    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.tx.send(notification(method, params));
    }

    pub fn set_title(&self, context: &str, title: &str) {
        self.notify("setTitle", json!({"context": context, "title": title}));
    }
    pub fn set_subtitle(&self, context: &str, subtitle: &str) {
        self.notify("setSubtitle", json!({"context": context, "subtitle": subtitle}));
    }
    /// A path inside the plugin folder (`icons/x.svg`), a `data:` URL, or `""` for none.
    pub fn set_image(&self, context: &str, image: &str) {
        self.notify("setImage", json!({"context": context, "image": image}));
    }
    pub fn set_state(&self, context: &str, state: u32) {
        self.notify("setState", json!({"context": context, "state": state}));
    }
    /// Slider/dial position, 0..1.
    pub fn set_value(&self, context: &str, value: f64) {
        self.notify("setValue", json!({"context": context, "value": value}));
    }
    pub fn show_ok(&self, context: &str) {
        self.notify("showOk", json!({"context": context}));
    }
    pub fn show_alert(&self, context: &str) {
        self.notify("showAlert", json!({"context": context}));
    }
    /// Persist a new settings object for this key.
    pub fn set_settings(&self, context: &str, settings: &Map<String, Value>) {
        self.notify("setSettings", json!({"context": context, "settings": settings}));
    }
    /// `level` is `debug`, `info`, `warn` or `error`.
    pub fn log(&self, level: &str, message: &str) {
        self.notify("log", json!({"level": level, "message": message}));
    }
    pub fn open_url(&self, url: &str) {
        self.notify("openUrl", json!({"url": url}));
    }
    /// Third text line (stream / media tiles).
    pub fn set_detail(&self, context: &str, detail: &str) {
        self.notify("setDetail", json!({"context": context, "detail": detail}));
    }
    /// Play an https HLS stream in the phone's built-in player. `context` (optional) gets an alert flash if no
    /// phone can play it. `max_height` 0 = adaptive.
    pub fn phone_play(&self, context: Option<&str>, url: &str, title: &str, subtitle: &str, max_height: u32) {
        let mut p = json!({"url": url, "title": title, "subtitle": subtitle, "maxHeight": max_height});
        if let Some(c) = context {
            p["context"] = json!(c);
        }
        self.notify("phone.play", p);
    }
    /// Round 21: `phone.play` with every option. `session` is echoed back in `Event::PlayerClosed`.
    pub fn phone_play_with(&self, context: Option<&str>, req: &PhonePlay) {
        let mut p = json!({
            "url": req.url, "title": req.title, "subtitle": req.subtitle, "maxHeight": req.max_height,
            "muted": req.muted, "syncDelayMs": req.sync_delay_ms, "session": req.session,
        });
        if let Some(c) = context {
            p["context"] = json!(c);
        }
        self.notify("phone.play", p);
    }
    /// Close the phone player.
    pub fn phone_stop(&self) {
        self.notify("phone.stop", json!({}));
    }
}

/// Options for [`Outbox::phone_play_with`] (round 21).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhonePlay {
    pub url: String,
    pub title: String,
    pub subtitle: String,
    /// 0 = adaptive.
    pub max_height: u32,
    /// Play without sound (the PC plays it).
    pub muted: bool,
    /// 0 = no sync; else keep the frame at host time minus this (see docs/PLUGINS.md).
    pub sync_delay_ms: u32,
    pub session: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(f: impl FnOnce(&Outbox)) -> Vec<Value> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f(&Outbox::new(tx));
        let mut v = vec![];
        while let Ok(m) = rx.try_recv() {
            v.push(serde_json::from_str(&m).unwrap());
        }
        v
    }

    #[test]
    fn encodes_notifications() {
        let v = collect(|o| {
            o.set_title("c", "12:00");
            o.set_subtitle("c", "Mon");
            o.set_image("c", "icons/a.svg");
            o.set_state("c", 1);
            o.set_value("c", 0.5);
            o.show_ok("c");
            o.show_alert("c");
            o.set_settings("c", json!({"a": 1}).as_object().unwrap());
            o.log("info", "hi");
            o.open_url("https://x");
        });
        let methods: Vec<_> = v.iter().map(|m| m["method"].as_str().unwrap()).collect();
        assert_eq!(
            methods,
            [
                "setTitle",
                "setSubtitle",
                "setImage",
                "setState",
                "setValue",
                "showOk",
                "showAlert",
                "setSettings",
                "log",
                "openUrl"
            ]
        );
        assert!(v.iter().all(|m| m["jsonrpc"] == "2.0" && m.get("id").is_none()));
        assert_eq!(v[0]["params"], json!({"context": "c", "title": "12:00"}));
        assert_eq!(v[3]["params"]["state"], 1);
        assert_eq!(v[7]["params"]["settings"], json!({"a": 1}));
        assert_eq!(v[8]["params"], json!({"level": "info", "message": "hi"}));
    }

    #[test]
    fn dropped_receiver_is_silent() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        Outbox::new(tx).show_ok("c");
    }
}
