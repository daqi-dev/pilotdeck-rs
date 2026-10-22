//! Preserve manifest object order through the plugin server boundary.
use super::PluginRuntimeError;
use pilotdeck_protocol::JsValue;

pub(super) fn server_value(raw: &JsValue) -> Result<JsValue, PluginRuntimeError> {
    // The shared wire traversal bound also keeps downstream JsValue
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
    Ok(raw.clone())
}
