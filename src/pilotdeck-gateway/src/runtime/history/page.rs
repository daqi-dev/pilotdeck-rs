//! Offset paging with JavaScript parseInt and Array.slice semantics.
use super::project::ProjectionError;
use pilotdeck_protocol::JsValue;

fn cursor(input: Option<&JsValue>) -> Result<f64, ProjectionError> {
    let Some(value) = input.filter(|value| truthy(value)) else {
        return Ok(0.0);
    };
    let converted = crate::js_value::to_string_units_checked(value)
        .map_err(|e| ProjectionError(e.message().into()))?;
    let units = crate::runtime::memory::js_trim_units(&converted);
    let mut i = 0usize;
    let mut negative = false;
    if units.first() == Some(&(b'-' as u16)) {
        negative = true;
        i += 1;
    } else if units.first() == Some(&(b'+' as u16)) {
        i += 1;
    }
    // TS passes an explicit radix 10: e.g. "0x10" parses as zero.
    let mut found = false;
    let mut n = 0f64;
    for u in &units[i..] {
        if !(0x30..=0x39).contains(u) {
            break;
        }
        found = true;
        n = n * 10.0 + f64::from(*u - 0x30);
    }
    if !found || negative || !n.is_finite() {
        Ok(0.0)
    } else {
        Ok(n)
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
    let offset = cursor(input.get("cursor"))?;
    let limit = input
        .get("limit")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined));
    let end = match limit {
        None => total as f64,
        Some(JsValue::Number(n)) if *n == 0.0 => total as f64,
        Some(v @ (JsValue::String(_) | JsValue::Array(_) | JsValue::Object(_))) => {
            let units = crate::js_value::to_string_units_checked(v)
                .map_err(|e| ProjectionError(e.message().into()))?;
            let text = format!(
                "{}{}",
                crate::js_value::number_string(offset),
                String::from_utf16_lossy(&units)
            );
            crate::js_value::string_to_number(&text)
        }
        Some(v) => {
            offset
                + crate::js_value::to_number(v)
                    .map_err(|_| ProjectionError(crate::js_value::StackOverflow::MESSAGE.into()))?
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
    fn array_limit_uses_javascript_addition() {
        let input = parse_json(r#"{"cursor":"1","limit":[2]}"#).unwrap();
        let (range, next) = page(20, &input).unwrap();
        assert_eq!(range, 1..12);
        assert_eq!(next.as_deref(), Some("12"));
    }
    #[test]
    fn golden_cursor_and_limits() {
        for (raw, want, next) in [
            (r#"{"cursor":"1junk","limit":1}"#, 1..2, None),
            (r#"{"cursor":"-1","limit":1}"#, 0..1, Some("1")),
            (r#"{"limit":-1}"#, 0..1, Some("1")),
            (r#"{"cursor":"1","limit":0}"#, 1..2, None),
            (r#"{"cursor":"999","limit":1}"#, 2..2, None),
            (r#"{"cursor":"0x10","limit":1}"#, 0..1, Some("1")),
            (r#"{"cursor":1,"limit":1}"#, 1..2, None),
            (r#"{"cursor":1.5,"limit":1}"#, 1..2, None),
            (r#"{"cursor":1e21,"limit":1}"#, 1..2, None),
        ] {
            let (range, cursor) = page(2, &parse_json(raw).unwrap()).unwrap();
            assert_eq!(range, want);
            assert_eq!(cursor.as_deref(), next);
        }
    }
}
