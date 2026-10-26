// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure URL building, response parsing and WMO code mapping.
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Geo {
    pub lat: f64,
    pub lon: f64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Current {
    pub temp: f64,
    pub code: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Units {
    Celsius,
    Fahrenheit,
}

impl Units {
    pub fn parse(s: Option<&str>) -> Units {
        match s {
            Some("fahrenheit") => Units::Fahrenheit,
            _ => Units::Celsius,
        }
    }
    pub fn param(self) -> &'static str {
        match self {
            Units::Celsius => "celsius",
            Units::Fahrenheit => "fahrenheit",
        }
    }
}

/// Percent-encode for a URL query value (UTF-8 bytes).
pub fn encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

pub fn geocode_url(city: &str) -> String {
    format!("https://geocoding-api.open-meteo.com/v1/search?name={}&count=1", encode(city.trim()))
}

pub fn forecast_url(lat: f64, lon: f64, units: Units) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code&temperature_unit={}",
        units.param()
    )
}

pub fn parse_geocode(v: &Value) -> Option<Geo> {
    let r = v.get("results")?.as_array()?.first()?;
    Some(Geo {
        lat: r.get("latitude")?.as_f64()?,
        lon: r.get("longitude")?.as_f64()?,
        name: r.get("name")?.as_str()?.to_string(),
    })
}

pub fn parse_current(v: &Value) -> Option<Current> {
    let c = v.get("current")?;
    Some(Current { temp: c.get("temperature_2m")?.as_f64()?, code: c.get("weather_code")?.as_u64()? as u32 })
}

/// `21°`, never `-0°`.
pub fn format_temp(t: f64) -> String {
    let r = t.round();
    format!("{}°", if r == 0.0 { 0.0 } else { r })
}

/// WMO weather code -> (glyph file stem in `icons/`, label).
pub fn wmo(code: u32) -> (&'static str, &'static str) {
    match code {
        0 => ("sun", "Clear"),
        1 => ("sun", "Mostly clear"),
        2 => ("partly-cloudy", "Partly cloudy"),
        3 => ("cloud", "Cloudy"),
        45 | 48 => ("fog", "Fog"),
        51 | 53 | 55 => ("rain", "Drizzle"),
        56 | 57 => ("rain", "Freezing drizzle"),
        61 | 63 | 65 => ("rain", "Rain"),
        66 | 67 => ("rain", "Freezing rain"),
        71 | 73 | 75 => ("snow", "Snow"),
        77 => ("snow", "Snow grains"),
        80..=82 => ("rain", "Showers"),
        85 | 86 => ("snow", "Snow showers"),
        95 => ("storm", "Thunderstorm"),
        96 | 99 => ("storm", "Thunderstorm, hail"),
        _ => ("cloud", "Unknown"),
    }
}

#[cfg(test)]
pub const GLYPHS: [&str; 7] = ["sun", "partly-cloudy", "cloud", "fog", "rain", "snow", "storm"];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encoding_and_urls() {
        assert_eq!(encode("New York"), "New%20York");
        assert_eq!(encode("İstanbul"), "%C4%B0stanbul");
        assert_eq!(encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(geocode_url(" Istanbul "), "https://geocoding-api.open-meteo.com/v1/search?name=Istanbul&count=1");
        assert_eq!(
            forecast_url(41.01, 28.95, Units::Fahrenheit),
            "https://api.open-meteo.com/v1/forecast?latitude=41.01&longitude=28.95&current=temperature_2m,weather_code&temperature_unit=fahrenheit"
        );
    }

    #[test]
    fn units() {
        assert_eq!(Units::parse(Some("fahrenheit")), Units::Fahrenheit);
        assert_eq!(Units::parse(Some("x")), Units::Celsius);
        assert_eq!(Units::parse(None).param(), "celsius");
    }

    #[test]
    fn geocode_parse() {
        let v =
            json!({"results": [{"name": "Istanbul", "latitude": 41.01384, "longitude": 28.94966, "country": "TR"}]});
        assert_eq!(parse_geocode(&v), Some(Geo { lat: 41.01384, lon: 28.94966, name: "Istanbul".into() }));
        assert_eq!(parse_geocode(&json!({"generationtime_ms": 0.5})), None);
        assert_eq!(parse_geocode(&json!({"results": []})), None);
    }

    #[test]
    fn current_parse() {
        let v = json!({"current": {"time": "x", "temperature_2m": 66.5, "weather_code": 80}});
        assert_eq!(parse_current(&v), Some(Current { temp: 66.5, code: 80 }));
        assert_eq!(parse_current(&json!({"current": {}})), None);
        assert_eq!(parse_current(&json!({"error": true})), None);
    }

    #[test]
    fn temps() {
        assert_eq!(format_temp(21.4), "21°");
        assert_eq!(format_temp(21.5), "22°");
        assert_eq!(format_temp(-0.2), "0°");
        assert_eq!(format_temp(-3.6), "-4°");
    }

    #[test]
    fn codes() {
        assert_eq!(wmo(0), ("sun", "Clear"));
        assert_eq!(wmo(2).0, "partly-cloudy");
        assert_eq!(wmo(3).1, "Cloudy");
        assert_eq!(wmo(48).0, "fog");
        assert_eq!(wmo(63).0, "rain");
        assert_eq!(wmo(81).0, "rain");
        assert_eq!(wmo(75).0, "snow");
        assert_eq!(wmo(86).0, "snow");
        assert_eq!(wmo(99).0, "storm");
        assert_eq!(wmo(1234).0, "cloud");
        for c in 0..100 {
            assert!(GLYPHS.contains(&wmo(c).0));
        }
    }
}
