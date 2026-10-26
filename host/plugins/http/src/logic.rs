// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure request-building and response-formatting logic.
use nd_plugin_sdk::settings;
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// `Name: value` per line. Blank lines, `#` comments and lines without a colon are skipped.
pub fn parse_headers(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once(':')?;
            let k = k.trim();
            (!k.is_empty() && !k.contains(char::is_whitespace)).then(|| (k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Build the request from a key's settings.
pub fn build_request(s: &Map<String, Value>) -> Result<Request, String> {
    let url = settings::text(s, "url").ok_or("no URL set")?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("URL must start with http:// or https:// ({url})"));
    }
    let method = match settings::str(s, "method").map(|m| m.trim().to_ascii_uppercase()).as_deref() {
        Some(m @ ("GET" | "POST" | "PUT" | "DELETE")) => m.to_string(),
        _ => "GET".into(),
    };
    let mut headers = settings::str(s, "headers").map(parse_headers).unwrap_or_default();
    let body = settings::str(s, "body").filter(|b| !b.trim().is_empty()).map(String::from);
    if body.is_some() && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
        let ct = settings::text(s, "content_type").unwrap_or_else(|| "application/json".into());
        headers.push(("Content-Type".into(), ct));
    }
    Ok(Request { method, url, headers, body })
}

/// Text for the key title: the value at JSON pointer `pointer` in `body`. `None` if `pointer` is empty.
pub fn show_value(body: &str, pointer: &str) -> Option<String> {
    let pointer = pointer.trim();
    if pointer.is_empty() {
        return None;
    }
    let shown = match serde_json::from_str::<Value>(body) {
        Ok(v) => match v.pointer(if pointer == "/" { "" } else { pointer }) {
            Some(x) => display(x),
            None => "--".into(),
        },
        Err(_) => "--".into(),
    };
    Some(shown)
}

fn display(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && (f - f.round()).abs() > 1e-9 => {
                format!("{f:.2}").trim_end_matches('0').trim_end_matches('.').to_string()
            }
            _ => n.to_string(),
        },
        other => other.to_string(),
    };
    if s.chars().count() > 40 {
        s.chars().take(39).collect::<String>() + "…"
    } else {
        s
    }
}

pub fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn m(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn headers() {
        let h = parse_headers("Authorization: Bearer a:b\n\n# no\nX-Y:1\nbad line\n : x\n");
        assert_eq!(h, vec![("Authorization".into(), "Bearer a:b".into()), ("X-Y".into(), "1".into())]);
        assert_eq!(parse_headers("A: b\r\nC: d\r\n").len(), 2);
    }

    #[test]
    fn request_building() {
        let r = build_request(&m(json!({"url": " https://x/y ", "method": "post", "body": "{}", "headers": "A: 1"})))
            .unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.url, "https://x/y");
        assert_eq!(r.headers, vec![("A".into(), "1".into()), ("Content-Type".into(), "application/json".into())]);
        assert_eq!(r.body.as_deref(), Some("{}"));

        let r = build_request(&m(json!({"url": "http://h", "body": "x", "content_type": "text/plain"}))).unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.headers, vec![("Content-Type".into(), "text/plain".into())]);

        let r = build_request(&m(json!({"url": "http://h", "body": "x", "headers": "content-type: a/b"}))).unwrap();
        assert_eq!(r.headers.len(), 1);

        let r = build_request(&m(json!({"url": "http://h", "body": "  "}))).unwrap();
        assert!(r.body.is_none() && r.headers.is_empty());
    }

    #[test]
    fn request_errors() {
        assert!(build_request(&m(json!({}))).is_err());
        assert!(build_request(&m(json!({"url": "ftp://x"}))).is_err());
        assert!(build_request(&m(json!({"url": "file:///etc/passwd"}))).is_err());
    }

    #[test]
    fn pointers() {
        let body = r#"{"data":{"temp":21.456,"n":3,"s":"hi","ok":true,"l":[1,2],"z":null},"a/b":1}"#;
        assert_eq!(show_value(body, "/data/temp").as_deref(), Some("21.46"));
        assert_eq!(show_value(body, "/data/n").as_deref(), Some("3"));
        assert_eq!(show_value(body, "/data/s").as_deref(), Some("hi"));
        assert_eq!(show_value(body, "/data/ok").as_deref(), Some("true"));
        assert_eq!(show_value(body, "/data/l/1").as_deref(), Some("2"));
        assert_eq!(show_value(body, "/data/z").as_deref(), Some("null"));
        assert_eq!(show_value(body, "/a~1b").as_deref(), Some("1"));
        assert_eq!(show_value(body, "/nope").as_deref(), Some("--"));
        assert_eq!(show_value("not json", "/x").as_deref(), Some("--"));
        assert_eq!(show_value(body, "  "), None);
        assert_eq!(show_value("[5]", "/0").as_deref(), Some("5"));
    }

    #[test]
    fn long_values_truncate() {
        let body = json!({"s": "x".repeat(100)}).to_string();
        assert_eq!(show_value(&body, "/s").unwrap().chars().count(), 40);
    }

    #[test]
    fn success_range() {
        assert!(is_success(200) && is_success(204) && !is_success(301) && !is_success(404));
    }
}
