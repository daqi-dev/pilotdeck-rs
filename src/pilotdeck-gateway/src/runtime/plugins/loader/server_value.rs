//! Bridge JsValue into the runtime's existing serde server representation.
//! Both the depth check and conversion are iterative. In particular, valid
//! server config is not rejected at serde's default JSON parser depth of 128.
use super::PluginRuntimeError;
use pilotdeck_protocol::JsValue;
use serde_json::Value;
use std::collections::HashMap;

pub(super) fn server_value(raw: &JsValue) -> Result<Value, PluginRuntimeError> {
    // The shared wire traversal bound also keeps downstream serde Value
    // Clone/Drop (owned by the existing runtime) away from unbounded nesting.
    let mut pending = vec![(raw, 0)];
    while let Some((value, depth)) = pending.pop() {
        if matches!(value, JsValue::Array(_) | JsValue::Object(_)) {
            if depth >= crate::js_value::MAX_ARRAY_DEPTH {
                return Err(PluginRuntimeError::new("Maximum call stack size exceeded"));
            }
            match value {
                JsValue::Array(items) => pending.extend(items.iter().map(|v| (v, depth + 1))),
                JsValue::Object(object) => {
                    pending.extend(object.entries().iter().map(|(_, v)| (v, depth + 1)))
                }
                _ => unreachable!(),
            }
        }
    }
    let mut result = Value::Null;
    let mut pending = vec![(raw, &mut result)];
    while let Some((value, slot)) = pending.pop() {
        match value {
            JsValue::Undefined | JsValue::Null => *slot = Value::Null,
            JsValue::Bool(value) => *slot = Value::Bool(*value),
            JsValue::Number(value) => {
                *slot = serde_json::Number::from_f64(*value).map_or(Value::Null, Value::Number)
            }
            JsValue::String(value) => {
                *slot = Value::String(String::from_utf16_lossy(value.units()))
            }
            JsValue::Array(items) => {
                *slot = Value::Array(vec![Value::Null; items.len()]);
                let Value::Array(target) = slot else {
                    unreachable!()
                };
                pending.extend(items.iter().zip(target.iter_mut()));
            }
            JsValue::Object(object) => {
                let mut lookup: HashMap<String, &JsValue> = object
                    .entries()
                    .iter()
                    .map(|(key, value)| (String::from_utf16_lossy(key.units()), value))
                    .collect();
                *slot = Value::Object(
                    lookup
                        .keys()
                        .map(|key| (key.clone(), Value::Null))
                        .collect(),
                );
                let Value::Object(target) = slot else {
                    unreachable!()
                };
                pending.extend(
                    target
                        .iter_mut()
                        .map(|(key, slot)| (lookup.remove(key).expect("server object key"), slot)),
                );
            }
        }
    }
    Ok(result)
}
