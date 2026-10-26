// SPDX-License-Identifier: GPL-3.0-or-later
//! Notideck sample plugin: HTTP Request (`dev.notideck.http`).
mod logic;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use logic::{build_request, is_success, show_value, Request};
use nd_plugin_sdk::{settings, Config, Event, Outbox, Session};
use serde_json::{Map, Value};
use tokio::sync::mpsc;

const TIMEOUT: Duration = Duration::from_secs(10);

struct Slot {
    settings: Map<String, Value>,
    visible: bool,
    in_flight: bool,
    last_poll: Option<Instant>,
}

struct Done {
    ctx: String,
    press: bool,
    result: Result<(u16, String), String>,
}

fn interval(s: &Map<String, Value>) -> Option<Duration> {
    let secs = settings::f64(s, "interval").unwrap_or(0.0);
    (secs >= 1.0).then(|| Duration::from_secs(secs as u64))
}

/// Blocking request; returns (status, body text).
fn perform(req: &Request) -> Result<(u16, String), String> {
    let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
    let mut r = agent.request(&req.method, &req.url);
    for (k, v) in &req.headers {
        r = r.set(k, v);
    }
    let res = match &req.body {
        Some(b) => r.send_string(b),
        None => r.call(),
    };
    match res {
        Ok(resp) => {
            let status = resp.status();
            Ok((status, resp.into_string().unwrap_or_default()))
        }
        Err(ureq::Error::Status(code, resp)) => Ok((code, resp.into_string().unwrap_or_default())),
        Err(e) => Err(e.to_string()),
    }
}

fn start(slots: &mut HashMap<String, Slot>, ctx: &str, press: bool, out: &Outbox, tx: &mpsc::UnboundedSender<Done>) {
    let Some(slot) = slots.get_mut(ctx) else { return };
    if slot.in_flight {
        return;
    }
    let req = match build_request(&slot.settings) {
        Ok(r) => r,
        Err(e) => {
            out.log("warn", &e);
            if press {
                out.show_alert(ctx);
            } else {
                out.set_title(ctx, "--");
            }
            slot.last_poll = Some(Instant::now());
            return;
        }
    };
    slot.in_flight = true;
    slot.last_poll = Some(Instant::now());
    let (tx, ctx) = (tx.clone(), ctx.to_string());
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || perform(&req)).await.unwrap_or_else(|e| Err(e.to_string()));
        let _ = tx.send(Done { ctx, press, result });
    });
}

fn finish(slots: &mut HashMap<String, Slot>, out: &Outbox, d: Done) {
    let Some(slot) = slots.get_mut(&d.ctx) else { return };
    slot.in_flight = false;
    let pointer = settings::str(&slot.settings, "show").unwrap_or("").to_string();
    match d.result {
        Ok((status, body)) => {
            if let Some(v) = show_value(&body, &pointer) {
                out.set_title(&d.ctx, &if is_success(status) { v } else { "--".into() });
            }
            if d.press {
                if is_success(status) {
                    out.show_ok(&d.ctx);
                } else {
                    out.show_alert(&d.ctx);
                }
            }
            if !is_success(status) {
                out.log("warn", &format!("HTTP {status}"));
            }
        }
        Err(e) => {
            out.log("error", &format!("request failed: {e}"));
            if d.press {
                out.show_alert(&d.ctx);
            }
            if !pointer.trim().is_empty() {
                out.set_title(&d.ctx, "--");
            }
        }
    }
}

fn handle(slots: &mut HashMap<String, Slot>, out: &Outbox, tx: &mpsc::UnboundedSender<Done>, ev: Event) {
    match ev {
        Event::WillAppear(a) => {
            let ctx = a.target.context.clone();
            let in_flight = slots.get(&ctx).is_some_and(|s| s.in_flight);
            slots.insert(ctx, Slot { settings: a.target.settings, visible: true, in_flight, last_poll: None });
        }
        Event::WillDisappear(t) => {
            if let Some(s) = slots.get_mut(&t.context) {
                s.visible = false;
            }
        }
        Event::SettingsChanged(t) => {
            if let Some(s) = slots.get_mut(&t.context) {
                s.settings = t.settings;
                s.last_poll = None; // re-poll with the new settings
            }
        }
        Event::KeyDown(t) => {
            slots.entry(t.context.clone()).or_insert(Slot {
                settings: t.settings.clone(),
                visible: true,
                in_flight: false,
                last_poll: None,
            });
            start(slots, &t.context, true, out, tx);
        }
        _ => {}
    }
}

fn poll_due(slots: &mut HashMap<String, Slot>, out: &Outbox, tx: &mpsc::UnboundedSender<Done>, now: Instant) {
    let due: Vec<String> = slots
        .iter()
        .filter(|(_, s)| {
            s.visible
                && !s.in_flight
                && interval(&s.settings).is_some_and(|iv| s.last_poll.is_none_or(|t| now.duration_since(t) >= iv))
        })
        .map(|(c, _)| c.clone())
        .collect();
    for c in due {
        start(slots, &c, false, out, tx);
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cfg = match Config::from_process() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("http plugin: {e}");
            std::process::exit(2);
        }
    };
    let Session { out, mut events, .. } = match Session::connect(cfg).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("http plugin: {e}");
            std::process::exit(1);
        }
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<Done>();
    let mut slots: HashMap<String, Slot> = HashMap::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(ev) => handle(&mut slots, &out, &tx, ev),
                None => break,
            },
            Some(d) = rx.recv() => finish(&mut slots, &out, d),
            _ = ticker.tick() => poll_due(&mut slots, &out, &tx, Instant::now()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn slot(s: Value) -> Slot {
        Slot { settings: s.as_object().unwrap().clone(), visible: true, in_flight: true, last_poll: None }
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<(String, Value)> {
        let mut v = vec![];
        while let Ok(m) = rx.try_recv() {
            let m: Value = serde_json::from_str(&m).unwrap();
            v.push((m["method"].as_str().unwrap().to_string(), m["params"].clone()));
        }
        v
    }

    #[test]
    fn interval_setting() {
        assert_eq!(interval(json!({"interval": 30}).as_object().unwrap()), Some(Duration::from_secs(30)));
        assert_eq!(interval(json!({"interval": 0}).as_object().unwrap()), None);
        assert_eq!(interval(&Map::new()), None);
    }

    #[test]
    fn press_results() {
        let (out, mut rx) = Outbox::detached();
        let mut slots = HashMap::from([("c".to_string(), slot(json!({"show": "/v"})))]);
        finish(&mut slots, &out, Done { ctx: "c".into(), press: true, result: Ok((200, r#"{"v": 7}"#.into())) });
        let m = drain(&mut rx);
        assert_eq!(m[0].1["title"], "7");
        assert_eq!(m[1].0, "showOk");
        assert!(!slots["c"].in_flight);

        finish(&mut slots, &out, Done { ctx: "c".into(), press: true, result: Ok((500, "{}".into())) });
        let m = drain(&mut rx);
        assert!(m.iter().any(|x| x.0 == "showAlert"));
        assert!(m.iter().any(|x| x.0 == "log" && x.1["message"] == "HTTP 500"));

        finish(&mut slots, &out, Done { ctx: "c".into(), press: false, result: Err("timeout".into()) });
        let m = drain(&mut rx);
        assert!(m.iter().all(|x| x.0 != "showAlert" && x.0 != "showOk"));
        assert!(m.iter().any(|x| x.0 == "log"));
    }

    #[test]
    fn invalid_settings_alert_without_network() {
        let (out, mut rx) = Outbox::detached();
        let (tx, _drx) = mpsc::unbounded_channel();
        let mut slots = HashMap::from([("c".to_string(), Slot { in_flight: false, ..slot(json!({})) })]);
        start(&mut slots, "c", true, &out, &tx);
        let m = drain(&mut rx);
        assert_eq!(m[0].0, "log");
        assert_eq!(m[1].0, "showAlert");
        assert!(!slots["c"].in_flight);
    }
}
