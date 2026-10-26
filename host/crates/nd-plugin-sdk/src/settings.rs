// SPDX-License-Identifier: GPL-3.0-or-later
//! Lenient readers for the `settings` object (the UI may store numbers as strings and vice versa).
use serde_json::{Map, Value};

pub type Settings = Map<String, Value>;

pub fn str<'a>(s: &'a Settings, key: &str) -> Option<&'a str> {
    s.get(key).and_then(Value::as_str)
}

/// A string value, trimmed, `None` if absent or empty.
pub fn text(s: &Settings, key: &str) -> Option<String> {
    str(s, key).map(str::trim).filter(|v| !v.is_empty()).map(String::from)
}

pub fn f64(s: &Settings, key: &str) -> Option<f64> {
    match s.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(v) => v.trim().parse().ok(),
        _ => None,
    }
}

pub fn bool(s: &Settings, key: &str) -> Option<bool> {
    match s.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(v) => match v.as_str() {
            "true" | "1" | "on" => Some(true),
            "false" | "0" | "off" => Some(false),
            _ => None,
        },
        Value::Number(n) => n.as_f64().map(|x| x != 0.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lenient() {
        let s = json!({"a": "5", "b": 2.5, "c": "true", "d": 0, "e": "  ", "f": " x "});
        let s = s.as_object().unwrap();
        assert_eq!(f64(s, "a"), Some(5.0));
        assert_eq!(f64(s, "b"), Some(2.5));
        assert_eq!(bool(s, "c"), Some(true));
        assert_eq!(bool(s, "d"), Some(false));
        assert_eq!(text(s, "e"), None);
        assert_eq!(text(s, "f").as_deref(), Some("x"));
        assert_eq!(f64(s, "zz"), None);
    }
}
