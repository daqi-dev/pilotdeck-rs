//! Offset paging with JavaScript parseInt and Array.slice semantics.
use super::project::ProjectionError;
use pilotdeck_protocol::JsValue;

fn cursor(input: Option<&JsValue>) -> f64 {
    let Some(JsValue::String(s)) = input else {
        return 0.0;
    };
    let units = crate::runtime::memory::js_trim_units(s.units());
    let mut i = 0usize;
    let mut negative = false;
    if units.first() == Some(&(b'-' as u16)) {
        negative = true;
        i += 1;
    } else if units.first() == Some(&(b'+' as u16)) {
        i += 1;
    }
    let hex = units.get(i) == Some(&(b'0' as u16)) && matches!(units.get(i + 1), Some(0x78 | 0x58));
    if hex {
        i += 2;
    }
    let mut found = false;
    let mut n = 0f64;
    for u in &units[i..] {
        let d = match *u {
            0x30..=0x39 => u - 0x30,
            0x41..=0x46 if hex => u - 0x41 + 10,
            0x61..=0x66 if hex => u - 0x61 + 10,
            _ => break,
        };
        if !hex && d >= 10 {
            break;
        }
        found = true;
        n = n * (if hex { 16.0 } else { 10.0 }) + f64::from(d);
    }
    if !found || negative || !n.is_finite() {
        0.0
    } else {
        n
    }
}
fn truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.units().is_empty(),
        _ => true,
    }
}
/// Returns an exact range and optional cursor; does not infer direction.
pub fn page(
    total: usize,
    input: &JsValue,
) -> Result<(std::ops::Range<usize>, Option<String>), ProjectionError> {
    let offset = cursor(input.get("cursor"));
    let limit = input
        .get("limit")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined));
    let end = match limit {
        None => total as f64,
        Some(JsValue::Number(n)) if *n == 0.0 => total as f64,
        Some(JsValue::String(s)) => {
            let text = format!(
                "{}{}",
                crate::js_value::number_string(offset),
                String::from_utf16_lossy(s.units())
            );
            crate::js_value::string_to_number(&text)
        }
        Some(v) => {
            offset + crate::js_value::to_number(v).map_err(|e| ProjectionError(e.to_string()))?
        }
    };
    let start = crate::storage::slice_index(offset, total);
    let end = crate::storage::slice_index(end, total).max(start);
    let next = limit.filter(|v| truthy(v)).and_then(|_| {
        (offset + ((end - start) as f64) < total as f64)
            .then(|| crate::js_value::number_string(offset + (end - start) as f64))
    });
    Ok((start..end, next))
}
#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn golden_cursor_and_limits() {
        for (raw, want, next) in [
            (r#"{"cursor":"1junk","limit":1}"#, 1..2, None),
            (r#"{"cursor":"-1","limit":1}"#, 0..1, Some("1")),
            (r#"{"limit":-1}"#, 0..1, Some("1")),
            (r#"{"cursor":"1","limit":0}"#, 1..2, None),
            (r#"{"cursor":"999","limit":1}"#, 2..2, None),
        ] {
            let (range, cursor) = page(2, &parse_json(raw).unwrap()).unwrap();
            assert_eq!(range, want);
            assert_eq!(cursor.as_deref(), next);
        }
    }
}
