// SPDX-License-Identifier: GPL-3.0-or-later
use serde_json::{Map, Value};

/// The placed action an event is about.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub context: String,
    pub action: String,
    pub settings: Map<String, Value>,
}

/// `action.willAppear` payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Appear {
    pub target: Target,
    /// `"key"`, `"dial"`, `"slider"` or `"tile"`.
    pub controller: String,
    pub state: u32,
    pub value: f64,
}

/// Every agent -> plugin notification (PLUGINS.md section 4).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    WillAppear(Appear),
    WillDisappear(Target),
    SettingsChanged(Target),
    KeyDown(Target),
    KeyUp(Target),
    LongPress(Target),
    DoubleTap(Target),
    /// `ticks` is signed, + = clockwise.
    DialRotate {
        target: Target,
        ticks: i64,
    },
    DialPress(Target),
    DialRelease(Target),
    /// `value` is 0..1.
    SliderChange {
        target: Target,
        value: f64,
    },
    TouchStart(Target),
    TouchEnd(Target),
    FocusChanged {
        app: String,
        title: String,
    },
    /// Round 21: the user closed the phone player showing the stream started with this `session`.
    PlayerClosed {
        session: String,
    },
    /// A method this SDK version doesn't know (forward compatibility).
    Unknown {
        method: String,
        params: Value,
    },
}

impl Event {
    /// The target of a per-context event.
    pub fn target(&self) -> Option<&Target> {
        match self {
            Event::WillAppear(a) => Some(&a.target),
            Event::WillDisappear(t)
            | Event::SettingsChanged(t)
            | Event::KeyDown(t)
            | Event::KeyUp(t)
            | Event::LongPress(t)
            | Event::DoubleTap(t)
            | Event::DialPress(t)
            | Event::DialRelease(t)
            | Event::TouchStart(t)
            | Event::TouchEnd(t) => Some(t),
            Event::DialRotate { target, .. } | Event::SliderChange { target, .. } => Some(target),
            Event::FocusChanged { .. } | Event::PlayerClosed { .. } | Event::Unknown { .. } => None,
        }
    }

    /// Decode a notification. Returns `None` for malformed params of a known method.
    pub fn decode(method: &str, params: Value) -> Option<Event> {
        let target = |p: &Value| -> Option<Target> {
            Some(Target {
                context: p.get("context")?.as_str()?.to_string(),
                action: p.get("action").and_then(Value::as_str).unwrap_or("").to_string(),
                settings: p.get("settings").and_then(Value::as_object).cloned().unwrap_or_default(),
            })
        };
        Some(match method {
            "action.willAppear" => Event::WillAppear(Appear {
                target: target(&params)?,
                controller: params.get("controller").and_then(Value::as_str).unwrap_or("key").to_string(),
                state: params.get("state").and_then(Value::as_u64).unwrap_or(0) as u32,
                value: params.get("value").and_then(Value::as_f64).unwrap_or(0.0),
            }),
            "action.willDisappear" => Event::WillDisappear(target(&params)?),
            "settings.didChange" => Event::SettingsChanged(target(&params)?),
            "key.down" => Event::KeyDown(target(&params)?),
            "key.up" => Event::KeyUp(target(&params)?),
            "key.longPress" => Event::LongPress(target(&params)?),
            "key.doubleTap" => Event::DoubleTap(target(&params)?),
            "dial.rotate" => {
                Event::DialRotate { target: target(&params)?, ticks: params.get("ticks").and_then(Value::as_i64)? }
            }
            "dial.press" => Event::DialPress(target(&params)?),
            "dial.release" => Event::DialRelease(target(&params)?),
            "slider.change" => {
                Event::SliderChange { target: target(&params)?, value: params.get("value").and_then(Value::as_f64)? }
            }
            "touch.start" => Event::TouchStart(target(&params)?),
            "touch.end" => Event::TouchEnd(target(&params)?),
            "system.focusChanged" => Event::FocusChanged {
                app: params.get("app").and_then(Value::as_str).unwrap_or("").to_string(),
                title: params.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
            },
            "phone.playerClosed" => {
                Event::PlayerClosed { session: params.get("session").and_then(Value::as_str).unwrap_or("").to_string() }
            }
            _ => Event::Unknown { method: method.to_string(), params },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t() -> Target {
        Target {
            context: "p:1".into(),
            action: "dev.x/a".into(),
            settings: json!({"k": 1}).as_object().unwrap().clone(),
        }
    }

    fn base() -> Value {
        json!({"context": "p:1", "action": "dev.x/a", "settings": {"k": 1}})
    }

    #[test]
    fn will_appear() {
        let mut p = base();
        p["controller"] = json!("dial");
        p["state"] = json!(1);
        p["value"] = json!(0.5);
        match Event::decode("action.willAppear", p).unwrap() {
            Event::WillAppear(a) => {
                assert_eq!(a.target, t());
                assert_eq!((a.controller.as_str(), a.state, a.value), ("dial", 1, 0.5));
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn simple_and_extra_fields() {
        assert_eq!(Event::decode("key.down", base()), Some(Event::KeyDown(t())));
        assert_eq!(Event::decode("key.longPress", base()), Some(Event::LongPress(t())));
        let mut p = base();
        p["ticks"] = json!(-2);
        assert_eq!(Event::decode("dial.rotate", p), Some(Event::DialRotate { target: t(), ticks: -2 }));
        let mut p = base();
        p["value"] = json!(0.25);
        assert_eq!(Event::decode("slider.change", p), Some(Event::SliderChange { target: t(), value: 0.25 }));
        assert_eq!(
            Event::decode("system.focusChanged", json!({"app": "Code.exe", "title": ""})),
            Some(Event::FocusChanged { app: "Code.exe".into(), title: "".into() })
        );
    }

    #[test]
    fn every_method_maps() {
        for m in [
            "action.willDisappear",
            "settings.didChange",
            "key.down",
            "key.up",
            "key.longPress",
            "key.doubleTap",
            "dial.press",
            "dial.release",
            "touch.start",
            "touch.end",
        ] {
            let e = Event::decode(m, base()).unwrap();
            assert_eq!(e.target(), Some(&t()), "{m}");
        }
    }

    #[test]
    fn bad_and_unknown() {
        assert_eq!(Event::decode("key.down", json!({})), None);
        assert_eq!(Event::decode("dial.rotate", base()), None);
        assert!(matches!(Event::decode("future.thing", json!({"a": 1})), Some(Event::Unknown { .. })));
    }
}
