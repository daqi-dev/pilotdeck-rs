use crate::runtime::connection::RequestError;
use pilotdeck_protocol::{JsObject, JsValue};
use std::{
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) static UNDEFINED: JsValue = JsValue::Undefined;

pub(super) fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
pub(super) fn text(value: &JsValue, key: &str) -> Option<String> {
    field(value, key).as_string()?.to_utf8().ok()
}
pub(super) fn nonempty(value: &JsValue) -> bool {
    value
        .as_string()
        .is_some_and(|s| !crate::metadata::js_trim(&String::from_utf16_lossy(s.units())).is_empty())
}
pub(super) fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
pub(super) fn safe_revision(value: &JsValue) -> bool {
    value.as_number().is_some_and(|n| {
        n.is_finite() && n.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(&n)
    })
}
pub(super) fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in fields {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}
pub(super) fn set(value: &mut JsValue, key: &str, next: JsValue) {
    if let JsValue::Object(o) = value {
        o.insert(key.into(), next);
    }
}
pub(super) fn spread(value: &JsValue) -> JsObject {
    match value {
        JsValue::Object(o) => o.clone(),
        JsValue::Array(a) => {
            let mut o = JsObject::new();
            for (i, v) in a.iter().enumerate() {
                o.insert(i.to_string().into(), v.clone());
            }
            o
        }
        JsValue::String(s) => {
            let mut o = JsObject::new();
            for (i, unit) in s.units().iter().enumerate() {
                o.insert(
                    i.to_string().into(),
                    JsValue::String(pilotdeck_protocol::JsString::from_utf16(vec![*unit])),
                );
            }
            o
        }
        _ => JsObject::new(),
    }
}
// Node isDeepStrictEqual ignores object insertion order, unlike JsValue::eq.
pub(super) fn deep_equal(a: &JsValue, b: &JsValue) -> bool {
    let mut pending = vec![(a, b)];
    while let Some((a, b)) = pending.pop() {
        match (a, b) {
            (JsValue::Object(a), JsValue::Object(b)) if a.len() == b.len() => {
                for (key, value) in a.entries() {
                    let Some(other) = b.get_key(key) else {
                        return false;
                    };
                    pending.push((value, other));
                }
            }
            (JsValue::Array(a), JsValue::Array(b)) if a.len() == b.len() => {
                pending.extend(a.iter().zip(b))
            }
            (JsValue::Number(a), JsValue::Number(b))
                if a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()) => {}
            (JsValue::Number(_), JsValue::Number(_)) => return false,
            _ if a == b => {}
            _ => return false,
        }
    }
    true
}
pub(super) fn error(
    code: &str,
    message: impl Into<pilotdeck_protocol::JsString>,
    status: Option<u16>,
) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into(),
        details: status.map(|s| Box::new(object([("httpStatus", JsValue::Number(s.into()))]))),
        validation: None,
    }
}
pub(super) fn resolve(cwd: &Path, path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    let absolute = if absolute.is_absolute() {
        absolute
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(absolute)
    };
    let mut out = PathBuf::new();
    for c in absolute.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}
pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
pub(super) fn now() -> String {
    iso_timestamp(now_ms())
}
pub(super) fn iso_timestamp(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let time = ms.rem_euclid(86_400_000);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3_600_000,
        time / 60_000 % 60,
        time / 1000 % 60,
        time % 1000
    )
}
pub(super) fn expiry_ms(text: &str) -> Option<i64> {
    // shortcut: expiry accepts ISO timestamps with a zone; extend if the owner emits other formats.
    if !text.is_ascii() || text.len() < 19 {
        return None;
    }
    let n = |a: usize, b: usize| text.get(a..b)?.parse::<i64>().ok();
    if text.get(4..5)? != "-"
        || text.get(7..8)? != "-"
        || !matches!(text.get(10..11)?, "T" | "t" | " ")
        || text.get(13..14)? != ":"
        || text.get(16..17)? != ":"
    {
        return None;
    }
    let (mut y, m, d, h, min, s) = (
        n(0, 4)?,
        n(5, 7)?,
        n(8, 10)?,
        n(11, 13)?,
        n(14, 16)?,
        n(17, 19)?,
    );
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let limit = match m {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return None,
    };
    if !(1..=limit).contains(&d)
        || !(0..=23).contains(&h)
        || !(0..=59).contains(&min)
        || !(0..=59).contains(&s)
    {
        return None;
    }
    y -= i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * mp + 2) / 5 + d - 1 - 719468;
    let mut tail = &text[19..];
    let mut frac = 0;
    if let Some(rest) = tail.strip_prefix('.') {
        let len = rest.bytes().take_while(u8::is_ascii_digit).count();
        if len == 0 {
            return None;
        }
        let digits = &rest[..len.min(3)];
        frac = digits.parse::<i64>().ok()? * 10_i64.pow((3 - digits.len()) as u32);
        tail = &rest[len..];
    }
    let offset = if matches!(tail, "Z" | "z" | "+00:00") {
        0
    } else {
        let sign = match tail.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        if tail.len() != 6 || &tail[3..4] != ":" {
            return None;
        }
        let h = tail[1..3].parse::<i64>().ok()?;
        let m = tail[4..6].parse::<i64>().ok()?;
        if h > 23 || m > 59 {
            return None;
        }
        sign * (h * 60 + m) * 60_000
    };
    Some(days * 86_400_000 + h * 3_600_000 + min * 60_000 + s * 1000 + frac - offset)
}
