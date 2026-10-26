// SPDX-License-Identifier: GPL-3.0-or-later
//! Notideck sample plugin: Clock & Timer (`dev.notideck.clock`).
mod logic;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use logic::{action_name, Stopwatch, Timer};
use nd_plugin_sdk::{settings, Config, Event, Outbox, Session, Target};
use serde_json::{json, Map, Value};

enum Kind {
    Clock,
    Timer(Timer),
    Stopwatch(Stopwatch),
}

struct Slot {
    kind: Kind,
    settings: Map<String, Value>,
    visible: bool,
    last_title: Option<String>,
    last_sub: Option<String>,
    last_state: Option<u32>,
}

fn timer_total(s: &Map<String, Value>) -> u64 {
    let m = settings::f64(s, "minutes").unwrap_or(5.0).clamp(0.0, 600.0) as u64;
    let sec = settings::f64(s, "seconds").unwrap_or(0.0).clamp(0.0, 59.0) as u64;
    m * 60 + sec
}

impl Slot {
    fn new(t: &Target) -> Option<Slot> {
        let kind = match action_name(&t.action) {
            "clock" => Kind::Clock,
            "timer" => Kind::Timer(Timer::new(timer_total(&t.settings))),
            "stopwatch" => Kind::Stopwatch(Stopwatch::default()),
            _ => return None,
        };
        Some(Slot {
            kind,
            settings: t.settings.clone(),
            visible: true,
            last_title: None,
            last_sub: None,
            last_state: None,
        })
    }

    fn apply_settings(&mut self, s: &Map<String, Value>) {
        self.settings = s.clone();
        if let Kind::Timer(t) = &mut self.kind {
            t.set_total(timer_total(s));
        }
    }

    /// Push changed text/state to the agent.
    fn render(&mut self, ctx: &str, out: &Outbox, now: Instant) {
        let (title, sub, state) = match &self.kind {
            Kind::Clock => {
                let local = chrono::Local::now().naive_local();
                let h24 = settings::str(&self.settings, "format") != Some("12h");
                let secs = settings::bool(&self.settings, "seconds").unwrap_or(false);
                let sub = settings::bool(&self.settings, "date").unwrap_or(false).then(|| logic::format_date(&local));
                (logic::format_time(&local, h24, secs), Some(sub.unwrap_or_default()), None)
            }
            Kind::Timer(t) => (t.title(now), None, Some(u32::from(t.is_done()))),
            Kind::Stopwatch(w) => (w.title(now), None, None),
        };
        if self.last_title.as_deref() != Some(&title) {
            out.set_title(ctx, &title);
            self.last_title = Some(title);
        }
        if let Some(sub) = sub {
            if self.last_sub.as_deref() != Some(&sub) {
                out.set_subtitle(ctx, &sub);
                self.last_sub = Some(sub);
            }
        }
        if let Some(st) = state {
            if self.last_state != Some(st) {
                out.set_state(ctx, st);
                self.last_state = Some(st);
            }
        }
    }
}

fn handle(slots: &mut HashMap<String, Slot>, out: &Outbox, ev: Event, now: Instant) {
    match ev {
        Event::WillAppear(a) => {
            let ctx = a.target.context.clone();
            let Some(mut slot) = slots.remove(&ctx).or_else(|| Slot::new(&a.target)) else {
                out.log("warn", &format!("unknown action {:?}", a.target.action));
                return;
            };
            slot.apply_settings(&a.target.settings);
            slot.visible = true;
            slot.last_title = None;
            slot.last_sub = None;
            slot.last_state = None;
            slot.render(&ctx, out, now);
            slots.insert(ctx, slot);
        }
        Event::WillDisappear(t) => {
            // timers keep running off screen; a plain clock has nothing to keep
            if let Some(s) = slots.get_mut(&t.context) {
                s.visible = false;
            }
            if matches!(slots.get(&t.context).map(|s| &s.kind), Some(Kind::Clock)) {
                slots.remove(&t.context);
            }
        }
        Event::SettingsChanged(t) => {
            if let Some(s) = slots.get_mut(&t.context) {
                s.apply_settings(&t.settings);
                s.render(&t.context, out, now);
            }
        }
        Event::KeyDown(t) => {
            if let Some(s) = slots.get_mut(&t.context) {
                match &mut s.kind {
                    Kind::Timer(x) => x.toggle(now),
                    Kind::Stopwatch(x) => x.toggle(now),
                    Kind::Clock => {}
                }
                s.render(&t.context, out, now);
            }
        }
        Event::DialPress(t) => handle(slots, out, Event::KeyDown(t), now),
        Event::LongPress(t) => {
            if let Some(s) = slots.get_mut(&t.context) {
                match &mut s.kind {
                    Kind::Timer(x) => x.reset(),
                    Kind::Stopwatch(x) => x.reset(),
                    Kind::Clock => {}
                }
                s.render(&t.context, out, now);
            }
        }
        Event::DialRotate { target, ticks } => {
            if let Some(s) = slots.get_mut(&target.context) {
                if matches!(s.kind, Kind::Timer(_)) {
                    let minutes = settings::f64(&s.settings, "minutes").unwrap_or(5.0);
                    let minutes = (minutes + ticks as f64).clamp(0.0, 600.0).round();
                    let mut new = s.settings.clone();
                    new.insert("minutes".into(), json!(minutes as u64));
                    s.apply_settings(&new);
                    out.set_settings(&target.context, &new);
                    s.render(&target.context, out, now);
                }
            }
        }
        _ => {}
    }
}

fn tick(slots: &mut HashMap<String, Slot>, out: &Outbox, now: Instant) {
    for (ctx, s) in slots.iter_mut() {
        if let Kind::Timer(t) = &mut s.kind {
            if t.tick(now) {
                out.show_alert(ctx);
            }
        }
        if s.visible {
            s.render(ctx, out, now);
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cfg = match Config::from_process() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("clock plugin: {e}");
            std::process::exit(2);
        }
    };
    let Session { out, mut events, .. } = match Session::connect(cfg).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("clock plugin: {e}");
            std::process::exit(1);
        }
    };
    let mut slots: HashMap<String, Slot> = HashMap::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(ev) => handle(&mut slots, &out, ev, Instant::now()),
                None => break,
            },
            _ = ticker.tick() => tick(&mut slots, &out, Instant::now()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nd_plugin_sdk::Appear;
    use tokio::sync::mpsc::UnboundedReceiver;

    fn target(action: &str, settings: Value) -> Target {
        Target { context: "c".into(), action: action.into(), settings: settings.as_object().unwrap().clone() }
    }

    fn appear(action: &str, settings: Value) -> Event {
        Event::WillAppear(Appear { target: target(action, settings), controller: "key".into(), state: 0, value: 0.0 })
    }

    fn drain(rx: &mut UnboundedReceiver<String>) -> Vec<(String, Value)> {
        let mut v = vec![];
        while let Ok(m) = rx.try_recv() {
            let m: Value = serde_json::from_str(&m).unwrap();
            v.push((m["method"].as_str().unwrap().to_string(), m["params"].clone()));
        }
        v
    }

    #[test]
    fn timer_total_from_settings() {
        let s = json!({"minutes": 2, "seconds": "30"});
        assert_eq!(timer_total(s.as_object().unwrap()), 150);
        assert_eq!(timer_total(&Map::new()), 300);
        assert_eq!(timer_total(json!({"minutes": 9999, "seconds": 99}).as_object().unwrap()), 600 * 60 + 59);
    }

    #[test]
    fn unknown_action_is_logged() {
        let (out, mut rx) = Outbox::detached();
        let mut slots = HashMap::new();
        handle(&mut slots, &out, appear("nope", json!({})), Instant::now());
        assert_eq!(drain(&mut rx)[0].0, "log");
        assert!(slots.is_empty());
    }

    #[test]
    fn timer_flow() {
        let (out, mut rx) = Outbox::detached();
        let mut slots = HashMap::new();
        let t0 = Instant::now();
        let s = |n| t0 + Duration::from_secs(n);
        handle(&mut slots, &out, appear("timer", json!({"minutes": 0, "seconds": 3})), t0);
        let m = drain(&mut rx);
        assert_eq!(m[0], ("setTitle".into(), json!({"context": "c", "title": "0:03"})));
        assert_eq!(m[1], ("setState".into(), json!({"context": "c", "state": 0})));
        handle(&mut slots, &out, Event::KeyDown(target("timer", json!({}))), t0);
        tick(&mut slots, &out, s(1));
        assert_eq!(drain(&mut rx)[0].1["title"], "0:02");
        tick(&mut slots, &out, s(3));
        let m = drain(&mut rx);
        assert!(m.iter().any(|x| x.0 == "showAlert"));
        assert!(m.iter().any(|x| x.0 == "setTitle" && x.1["title"] == "Done"));
        assert!(m.iter().any(|x| x.0 == "setState" && x.1["state"] == 1));
        handle(&mut slots, &out, Event::LongPress(target("timer", json!({}))), s(4));
        let m = drain(&mut rx);
        assert!(m.iter().any(|x| x.0 == "setState" && x.1["state"] == 0));
    }

    #[test]
    fn dial_changes_minutes_and_persists() {
        let (out, mut rx) = Outbox::detached();
        let mut slots = HashMap::new();
        let t0 = Instant::now();
        handle(&mut slots, &out, appear("timer", json!({"minutes": 5, "seconds": 0, "x": 1})), t0);
        drain(&mut rx);
        let ev = Event::DialRotate { target: target("timer", json!({})), ticks: 3 };
        handle(&mut slots, &out, ev, t0);
        let m = drain(&mut rx);
        let set = m.iter().find(|x| x.0 == "setSettings").unwrap();
        assert_eq!(set.1["settings"], json!({"minutes": 8, "seconds": 0, "x": 1}));
        assert!(m.iter().any(|x| x.0 == "setTitle" && x.1["title"] == "8:00"));
        let ev = Event::DialRotate { target: target("timer", json!({})), ticks: -100 };
        handle(&mut slots, &out, ev, t0);
        assert!(drain(&mut rx).iter().any(|x| x.0 == "setTitle" && x.1["title"] == "0:00"));
    }

    #[test]
    fn stopwatch_flow() {
        let (out, mut rx) = Outbox::detached();
        let mut slots = HashMap::new();
        let t0 = Instant::now();
        handle(&mut slots, &out, appear("stopwatch", json!({})), t0);
        handle(&mut slots, &out, Event::KeyDown(target("stopwatch", json!({}))), t0);
        drain(&mut rx);
        tick(&mut slots, &out, t0 + Duration::from_secs(65));
        assert_eq!(drain(&mut rx)[0].1["title"], "1:05");
        handle(&mut slots, &out, Event::LongPress(target("stopwatch", json!({}))), t0 + Duration::from_secs(66));
        assert_eq!(drain(&mut rx)[0].1["title"], "0:00");
    }

    #[test]
    fn clock_title_and_dedup() {
        let (out, mut rx) = Outbox::detached();
        let mut slots = HashMap::new();
        let t0 = Instant::now();
        handle(&mut slots, &out, appear("clock", json!({"format": "24h", "seconds": false, "date": true})), t0);
        let m = drain(&mut rx);
        let title = m[0].1["title"].as_str().unwrap();
        assert_eq!(title.len(), 5);
        assert!(m.iter().any(|x| x.0 == "setSubtitle" && !x.1["subtitle"].as_str().unwrap().is_empty()));
        tick(&mut slots, &out, t0); // same minute: nothing to resend
        assert!(drain(&mut rx).is_empty());
        handle(&mut slots, &out, Event::WillDisappear(target("clock", json!({}))), t0);
        assert!(slots.is_empty());
    }
}
