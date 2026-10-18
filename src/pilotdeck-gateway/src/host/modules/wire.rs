//! Payload readers matching sidecarDefaultModuleDispatcher.ts:474-485.
use crate::host::{HostError, ModuleError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub fn string_field<'a>(
    value: Option<&'a JsObject>,
    field: &str,
) -> Result<&'a JsString, HostError> {
    value
        .and_then(|object| object.get(field))
        .and_then(JsValue::as_string)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(field, "a non-empty string"))
}
pub fn optional_string_field<'a>(
    value: Option<&'a JsObject>,
    field: &str,
) -> Result<Option<&'a JsString>, HostError> {
    if missing(value.and_then(|object| object.get(field))) {
        return Ok(None);
    }
    string_field(value, field).map(Some)
}
pub fn boolean_field(value: Option<&JsObject>, field: &str) -> Result<bool, HostError> {
    value
        .and_then(|object| object.get(field))
        .and_then(JsValue::as_bool)
        .ok_or_else(|| invalid(field, "a boolean"))
}
pub fn positive_finite(value: Option<&JsValue>, field: &str) -> Result<f64, HostError> {
    value
        .and_then(JsValue::as_number)
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| invalid(field, "positive"))
}
pub fn optional_non_negative_finite(
    value: Option<&JsValue>,
    field: &str,
) -> Result<Option<f64>, HostError> {
    if missing(value) {
        return Ok(None);
    }
    value
        .and_then(JsValue::as_number)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(Some)
        .ok_or_else(|| invalid(field, "non-negative"))
}
fn missing(value: Option<&JsValue>) -> bool {
    value.is_none_or(JsValue::is_undefined)
}
fn invalid(field: &str, expected: &str) -> HostError {
    ModuleError::new(format!("Sidecar payload field {field} must be {expected}.")).into()
}
/// Preserve JavaScript UTF-16 strings, even lone surrogates, in diagnostic payloads.
pub(super) fn named_error(prefix: &str, name: &JsString, suffix: &str) -> HostError {
    let units = prefix
        .encode_utf16()
        .chain(name.units().iter().copied())
        .chain(suffix.encode_utf16())
        .collect();
    ModuleError::new(JsString::from_utf16(units)).into()
}
