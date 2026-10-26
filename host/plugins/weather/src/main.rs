// SPDX-License-Identifier: GPL-3.0-or-later
//! Notideck sample plugin: Weather (`dev.notideck.weather`), data from Open-Meteo.
mod logic;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use logic::{forecast_url, format_temp, geocode_url, parse_current, parse_geocode, wmo, Geo, Units};
use nd_plugin_sdk::{settings, Config, Event, Outbox, Session};
use serde_json::{Map, Value};
use tokio::sync::mpsc;

const TIMEOUT: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_secs(60);

type GeoCache = Arc<Mutex<HashMap<String, Geo>>>;

struct Slot {
    settings: Map<String, Value>,
    visible: bool,
    in_flight: bool,
    next_at: Option<Instant>,
}

struct Done {
    ctx: String,
    result: Result<Report, String>,
}

struct Report {
    temp: f64,
    code: u32,
    place: String,
}

fn refresh(s: &Map<String, Value>) -> Duration {
    let min = settings::f64(s, "refresh").unwrap_or(15.0).clamp(1.0, 24.0 * 60.0);
    Duration::from_secs((min * 60.0) as u64)
}

fn get_json(url: &str) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Blocking: geocode (cached per city) then fetch the current weather.
fn fetch(city: &str, units: Units, cache: &GeoCache) -> Result<Report, String> {
    let key = city.trim().to_lowercase();
    let cached = cache.lock().ok().and_then(|c| c.get(&key).cloned());
    let geo = match cached {
        Some(g) => g,
        None => {
            let g = parse_geocode(&get_json(&geocode_url(city))?).ok_or_else(|| format!("city not found: {city}"))?;
            if let Ok(mut c) = cache.lock() {
                c.insert(key, g.clone());
            }
            g
        }
    };
    let cur =
        parse_current(&get_json(&forecast_url(geo.lat, geo.lon, units))?).ok_or("unexpected forecast response")?;
    Ok(Report { temp: cur.temp, code: cur.code, place: geo.name })
}

fn start(
    slots: &mut HashMap<String, Slot>,
    ctx: &str,
    out: &Outbox,
    tx: &mpsc::UnboundedSender<Done>,
    cache: &GeoCache,
) {
    let Some(slot) = slots.get_mut(ctx) else { return };
    if slot.in_flight {
        return;
    }
    let Some(city) = settings::text(&slot.settings, "city") else {
        out.set_title(ctx, "--");
        out.set_subtitle(ctx, "Set a city");
        slot.next_at = Some(Instant::now() + RETRY);
        return;
    };
    let units = Units::parse(settings::str(&slot.settings, "units"));
    slot.in_flight = true;
    let (tx, ctx, cache) = (tx.clone(), ctx.to_string(), cache.clone());
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || fetch(&city, units, &cache))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let _ = tx.send(Done { ctx, result });
    });
}

fn finish(slots: &mut HashMap<String, Slot>, out: &Outbox, d: Done, now: Instant) {
    let Some(slot) = slots.get_mut(&d.ctx) else { return };
    slot.in_flight = false;
    match d.result {
        Ok(r) => {
            let (glyph, label) = wmo(r.code);
            out.set_title(&d.ctx, &format_temp(r.temp));
            out.set_subtitle(&d.ctx, &format!("{} · {}", r.place, label));
            out.set_image(&d.ctx, &format!("icons/{glyph}.svg"));
            slot.next_at = Some(now + refresh(&slot.settings));
        }
        Err(e) => {
            out.log("error", &format!("weather: {e}"));
            out.set_title(&d.ctx, "--");
            slot.next_at = Some(now + RETRY.min(refresh(&slot.settings)));
        }
    }
}

fn handle(slots: &mut HashMap<String, Slot>, ev: Event) {
    match ev {
        Event::WillAppear(a) => {
            // keep the schedule if we already know this key, so a page flip doesn't refetch
            let next_at = slots.get(&a.target.context).and_then(|s| s.next_at);
            let in_flight = slots.get(&a.target.context).is_some_and(|s| s.in_flight);
            slots.insert(a.target.context, Slot { settings: a.target.settings, visible: true, in_flight, next_at });
        }
        Event::WillDisappear(t) => {
            if let Some(s) = slots.get_mut(&t.context) {
                s.visible = false;
            }
        }
        Event::SettingsChanged(t) => {
            if let Some(s) = slots.get_mut(&t.context) {
                s.settings = t.settings;
                s.next_at = None;
            }
        }
        Event::KeyDown(t) => {
            // a tap refreshes now
            if let Some(s) = slots.get_mut(&t.context) {
                s.next_at = None;
            }
        }
        _ => {}
    }
}

fn poll_due(
    slots: &mut HashMap<String, Slot>,
    out: &Outbox,
    tx: &mpsc::UnboundedSender<Done>,
    cache: &GeoCache,
    now: Instant,
) {
    let due: Vec<String> = slots
        .iter()
        .filter(|(_, s)| s.visible && !s.in_flight && s.next_at.is_none_or(|t| now >= t))
        .map(|(c, _)| c.clone())
        .collect();
    for c in due {
        start(slots, &c, out, tx, cache);
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cfg = match Config::from_process() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("weather plugin: {e}");
            std::process::exit(2);
        }
    };
    let Session { out, mut events, .. } = match Session::connect(cfg).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("weather plugin: {e}");
            std::process::exit(1);
        }
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<Done>();
    let cache: GeoCache = Arc::default();
    let mut slots: HashMap<String, Slot> = HashMap::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(ev) => handle(&mut slots, ev),
                None => break,
            },
            Some(d) = rx.recv() => finish(&mut slots, &out, d, Instant::now()),
            _ = ticker.tick() => poll_due(&mut slots, &out, &tx, &cache, Instant::now()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nd_plugin_sdk::{Appear, Target};
    use serde_json::json;

    fn target(s: Value) -> Target {
        Target { context: "c".into(), action: "current".into(), settings: s.as_object().unwrap().clone() }
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
    fn refresh_setting() {
        assert_eq!(refresh(&Map::new()), Duration::from_secs(900));
        assert_eq!(refresh(json!({"refresh": 0}).as_object().unwrap()), Duration::from_secs(60));
        assert_eq!(refresh(json!({"refresh": "30"}).as_object().unwrap()), Duration::from_secs(1800));
    }

    #[test]
    fn success_and_error_rendering() {
        let (out, mut rx) = Outbox::detached();
        let mut slots = HashMap::new();
        let ev = Event::WillAppear(Appear {
            target: target(json!({"city": "Istanbul"})),
            controller: "tile".into(),
            state: 0,
            value: 0.0,
        });
        handle(&mut slots, ev);
        slots.get_mut("c").unwrap().in_flight = true;
        let now = Instant::now();
        let r = Report { temp: 20.6, code: 2, place: "Istanbul".into() };
        finish(&mut slots, &out, Done { ctx: "c".into(), result: Ok(r) }, now);
        let m = drain(&mut rx);
        assert_eq!(m[0].1["title"], "21°");
        assert_eq!(m[1].1["subtitle"], "Istanbul · Partly cloudy");
        assert_eq!(m[2].1["image"], "icons/partly-cloudy.svg");
        assert_eq!(slots["c"].next_at, Some(now + Duration::from_secs(900)));

        finish(&mut slots, &out, Done { ctx: "c".into(), result: Err("boom".into()) }, now);
        let m = drain(&mut rx);
        assert_eq!(m[0].0, "log");
        assert_eq!(m[1].1["title"], "--");
        assert_eq!(slots["c"].next_at, Some(now + RETRY));
    }

    #[test]
    fn missing_city_needs_no_network() {
        let (out, mut rx) = Outbox::detached();
        let (tx, _drx) = mpsc::unbounded_channel();
        let mut slots = HashMap::from([(
            "c".to_string(),
            Slot { settings: Map::new(), visible: true, in_flight: false, next_at: None },
        )]);
        start(&mut slots, "c", &out, &tx, &GeoCache::default());
        assert_eq!(drain(&mut rx)[0].1["title"], "--");
        assert!(!slots["c"].in_flight);
    }

    #[test]
    fn appear_keeps_schedule_settings_reset_it() {
        let mut slots = HashMap::new();
        let ap = |s| Event::WillAppear(Appear { target: target(s), controller: "key".into(), state: 0, value: 0.0 });
        handle(&mut slots, ap(json!({})));
        let t = Instant::now() + Duration::from_secs(5);
        slots.get_mut("c").unwrap().next_at = Some(t);
        handle(&mut slots, Event::WillDisappear(target(json!({}))));
        handle(&mut slots, ap(json!({})));
        assert_eq!(slots["c"].next_at, Some(t));
        handle(&mut slots, Event::SettingsChanged(target(json!({"city": "x"}))));
        assert_eq!(slots["c"].next_at, None);
    }
}
