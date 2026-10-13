//! Shared boundary comparison and projection helpers for tool consumers.
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub(crate) fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in fields {
        result.insert(key.into(), value);
    }
    JsValue::Object(result)
}
pub(crate) fn optional_string(value: Option<&JsString>) -> JsValue {
    value.map_or(JsValue::Undefined, |v| JsValue::String(v.clone()))
}
/// Exact captured data comparison, preserving signed zero and non-finite numbers.
/// This deliberately is not stringify equality (which omits Undefined).
pub(crate) fn same_value(a: &JsValue, b: &JsValue) -> bool {
    let mut pending = vec![(a, b)];
    while let Some((a, b)) = pending.pop() {
        match (a, b) {
            (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => (),
            (JsValue::Bool(a), JsValue::Bool(b)) if a == b => (),
            (JsValue::Number(a), JsValue::Number(b))
                if a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()) => {}
            (JsValue::String(a), JsValue::String(b)) if a == b => (),
            (JsValue::Array(a), JsValue::Array(b)) if a.len() == b.len() => {
                pending.extend(a.iter().zip(b))
            }
            (JsValue::Object(a), JsValue::Object(b)) if a.len() == b.len() => {
                for (key, value) in a.entries() {
                    let Some(other) = b.get_key(key) else {
                        return false;
                    };
                    pending.push((value, other));
                }
            }
            _ => return false,
        }
    }
    true
}

/// Check a normalized schema projection without deleting admitted extensions.
pub(crate) fn matches_fields(actual: &JsValue, expected: &JsValue) -> bool {
    let mut pending = vec![(actual, expected)];
    while let Some((actual, expected)) = pending.pop() {
        if let JsValue::Object(fields) = expected {
            let Some(actual) = actual.as_object() else {
                return false;
            };
            for (key, expected) in fields.entries() {
                match actual.get_key(key) {
                    None if expected.is_undefined() => (),
                    Some(value)
                        if key.equals_str("input")
                            || key.equals_str("raw")
                            || key.equals_str("metadata")
                            || key.equals_str("data")
                            || key.equals_str("value") =>
                    {
                        if !same_value(value, expected) {
                            return false;
                        }
                    }
                    Some(value) => pending.push((value, expected)),
                    None => return false,
                }
            }
        } else if !same_value(actual, expected) {
            return false;
        }
    }
    true
}
