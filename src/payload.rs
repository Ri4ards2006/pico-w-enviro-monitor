//! Request bodies. Values are rounded to sensor resolution (0.01).

use core::fmt::Write;

use crate::sensor::Reading;

pub type Body = heapless::String<256>;

/// Wire format selected at build time via `PAYLOAD_FORMAT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `application/json` — Telegraf `http_listener_v2` (→ InfluxDB → Grafana),
    /// Node-RED, Home Assistant webhooks, …
    Json,
    /// `application/x-www-form-urlencoded` — ThingSpeak channel update API
    /// (field1 = °C, field2 = %RH, field3 = hPa, `api_key` in the body).
    ThingSpeak,
}

impl Format {
    pub const fn parse(s: &str) -> Option<Self> {
        match s.as_bytes() {
            b"json" => Some(Self::Json),
            b"thingspeak" => Some(Self::ThingSpeak),
            _ => None,
        }
    }

    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::ThingSpeak => "application/x-www-form-urlencoded",
        }
    }
}

/// Render the body. `node_id` must be a plain token (validated in build.rs);
/// `api_key` is only embedded for ThingSpeak.
pub fn render(fmt: Format, r: &Reading, node_id: &str, api_key: &str, uptime_s: u64) -> Result<Body, core::fmt::Error> {
    let mut b = Body::new();
    match fmt {
        Format::Json => write!(
            b,
            r#"{{"node":"{}","temperature_c":{:.2},"humidity_pct":{:.2},"pressure_hpa":{:.2},"uptime_s":{}}}"#,
            node_id, r.temperature_c, r.humidity_pct, r.pressure_hpa, uptime_s
        )?,
        Format::ThingSpeak => write!(
            b,
            "api_key={}&field1={:.2}&field2={:.2}&field3={:.2}",
            api_key, r.temperature_c, r.humidity_pct, r.pressure_hpa
        )?,
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: Reading = Reading { temperature_c: 21.534, humidity_pct: 45.2, pressure_hpa: 1013.249 };

    #[test]
    fn json_body() {
        let b = render(Format::Json, &R, "enviro-01", "SECRET", 42).unwrap();
        assert_eq!(
            b.as_str(),
            r#"{"node":"enviro-01","temperature_c":21.53,"humidity_pct":45.20,"pressure_hpa":1013.25,"uptime_s":42}"#
        );
        assert!(!b.contains("SECRET"), "API key must not leak into JSON body");
    }

    #[test]
    fn thingspeak_body() {
        let b = render(Format::ThingSpeak, &R, "enviro-01", "KEY123", 0).unwrap();
        assert_eq!(b.as_str(), "api_key=KEY123&field1=21.53&field2=45.20&field3=1013.25");
    }

    #[test]
    fn negative_temperature() {
        let r = Reading { temperature_c: -5.0, ..R };
        let b = render(Format::Json, &r, "n", "", 0).unwrap();
        assert!(b.contains(r#""temperature_c":-5.00"#));
    }

    #[test]
    fn format_parse() {
        assert_eq!(Format::parse("json"), Some(Format::Json));
        assert_eq!(Format::parse("thingspeak"), Some(Format::ThingSpeak));
        assert_eq!(Format::parse("mqtt"), None);
    }
}
