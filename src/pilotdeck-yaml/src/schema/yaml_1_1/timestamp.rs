// Port of yaml@2.9.0 dist/schema/yaml-1.1/timestamp.js (ISC).
use crate::schema::{Resolve, Tag};
use pilotdeck_protocol::JsString;
use regex::Regex;
use std::sync::LazyLock;
static INT_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+$").unwrap());
static FLOAT_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*$").unwrap());
static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([0-9]{4})-([0-9]{1,2})-([0-9]{1,2})(?:(?:t|T|[ \t]+)([0-9]{1,2}):([0-9]{1,2}):([0-9]{1,2}(\.[0-9]+)?)(?:[ \t]*(Z|[-+][012]?[0-9](?::[0-9]{2})?))?)?$").unwrap()
});
pub(crate) fn int_time() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:int",
        default: true,
        key_only: false,
        pattern: Some(&INT_TIME),
        resolve: Resolve::IntTime,
        format: Some("TIME"),
        collection: None,
    }
}
pub(crate) fn float_time() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:float",
        default: true,
        key_only: false,
        pattern: Some(&FLOAT_TIME),
        resolve: Resolve::FloatTime,
        format: Some("TIME"),
        collection: None,
    }
}
pub(crate) fn timestamp() -> Tag {
    Tag {
        name: "tag:yaml.org,2002:timestamp",
        default: true,
        key_only: false,
        pattern: Some(&TIMESTAMP),
        resolve: Resolve::Timestamp,
        format: None,
        collection: None,
    }
}
pub(crate) fn parse_sexagesimal(s: &str) -> f64 {
    let neg = s.starts_with('-');
    let s = s.trim_start_matches(['-', '+']).replace('_', "");
    let n = s
        .split(':')
        .fold(0.0, |n, s| n * 60.0 + s.parse::<f64>().unwrap_or(0.0));
    if neg { -n } else { n }
}
pub(crate) fn resolve(s: &str) -> Result<i64, JsString> {
    let Some(c) = TIMESTAMP.captures(s) else {
        return Err("!!timestamp expects a date, starting with yyyy-mm-dd".into());
    };
    let num = |i| {
        c.get(i).map_or(0, |m| {
            m.as_str()
                .split('.')
                .next()
                .unwrap()
                .parse::<i64>()
                .unwrap()
        })
    };
    let mut year = num(1);
    if year < 100 {
        year += 1900;
    }
    let month = num(2) - 1;
    year += month.div_euclid(12);
    let month = month.rem_euclid(12) + 1;
    let base = chrono::NaiveDate::from_ymd_opt(year as i32, month as u32, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let ms = c.get(7).map_or(0, |m| {
        format!("{}00", &m.as_str()[1..])[..3]
            .parse::<i64>()
            .unwrap()
    });
    let mut date = base + ((num(3) - 1) * 86400 + num(4) * 3600 + num(5) * 60 + num(6)) * 1000 + ms;
    if let Some(tz) = c.get(8).filter(|s| s.as_str() != "Z") {
        let mut d = parse_sexagesimal(tz.as_str());
        if d.abs() < 30.0 {
            d *= 60.0;
        }
        date -= (60000.0 * d) as i64;
    }
    Ok(date)
}

// stringifySexagesimal, required only for complex-key coercion in toJSON.
pub(crate) fn stringify_sexagesimal(mut value: f64) -> JsString {
    let sign = if value < 0.0 {
        value = -value;
        "-"
    } else {
        ""
    };
    let mut parts = vec![value % 60.0];
    if value < 60.0 {
        parts.insert(0, 0.0);
    } else {
        value = (value - parts[0]) / 60.0;
        parts.insert(0, value % 60.0);
        if value >= 60.0 {
            value = (value - parts[0]) / 60.0;
            parts.insert(0, value);
        }
    }
    let mut text = format!(
        "{sign}{}",
        parts
            .into_iter()
            .map(|v| {
                let s = crate::nodes::to_js::number_string(v).to_utf8().unwrap();
                if s.len() < 2 { format!("0{s}") } else { s }
            })
            .collect::<Vec<_>>()
            .join(":")
    );
    if let Some(at) = text
        .rfind(':')
        .and_then(|at| text[at..].find("000000").map(|p| at + p))
    {
        text.truncate(at);
    }
    text.into()
}
