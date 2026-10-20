//! Original budget usage shortcut and deferred response-text accounting.
//! No tokenizer, ledger mutation or Host effects occur here.
use crate::ports::{PortError, PortResult};
use crate::usage_fallback::{BudgetUsageContinuation, BudgetUsageInput, BudgetUsageSelection};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};

fn undefined() -> &'static JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    &UNDEFINED
}
fn property<'a>(value: &'a JsValue, name: &str) -> PortResult<&'a JsValue> {
    match value {
        JsValue::Null | JsValue::Undefined => Err(exception(
            value,
            &format!(
                "Cannot read properties of {} (reading '{name}')",
                if matches!(value, JsValue::Null) {
                    "null"
                } else {
                    "undefined"
                }
            ),
        )),
        _ => Ok(value.get(name).unwrap_or(undefined())),
    }
}
fn exception(value: &JsValue, message: &str) -> PortError {
    PortError::SourceException {
        message: message.into(),
        boundary: BoundarySnapshot::capture(value),
    }
}
fn finite(value: &JsValue) -> bool {
    matches!(value, JsValue::Number(n) if n.is_finite())
}
fn provider_usage(value: &JsValue) -> bool {
    // Primitive boxing has no finite numeric enumerable own values. A dense
    // array's index values are enumerable just as an ordinary object's keys.
    match value {
        JsValue::Object(o) => o.entries().iter().any(|(_, value)| finite(value)),
        JsValue::Array(a) => a.iter().any(finite),
        _ => false,
    }
}
fn label(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(text))
}
fn number_text(value: f64) -> JsString {
    pilotdeck_protocol::number::number_to_string(value).into()
}
fn join_text(value: &JsValue) -> PortResult<JsString> {
    enum Step<'a> {
        Value(&'a JsValue, bool),
        Comma,
    }
    let mut pending = vec![Step::Value(value, false)];
    let mut text = Vec::new();
    while let Some(step) = pending.pop() {
        match step {
            Step::Comma => text.push(b',' as u16),
            Step::Value(value, nested) => match value {
                JsValue::Undefined | JsValue::Null if nested => (),
                JsValue::Undefined => text.extend("undefined".encode_utf16()),
                JsValue::Null => text.extend("null".encode_utf16()),
                JsValue::Bool(b) => text.extend(if *b { "true" } else { "false" }.encode_utf16()),
                JsValue::Number(n) => text.extend_from_slice(number_text(*n).units()),
                JsValue::String(s) => text.extend_from_slice(s.units()),
                JsValue::Array(values) => {
                    for (i, value) in values.iter().enumerate().rev() {
                        pending.push(Step::Value(value, true));
                        if i > 0 {
                            pending.push(Step::Comma);
                        }
                    }
                }
                JsValue::Object(o) => {
                    // All represented own values are noncallable. If toString
                    // shadows the ordinary native method, default valueOf
                    // returns an object and Source cannot produce a primitive.
                    if o.contains_key("toString") {
                        return Err(exception(value, "Cannot convert object to primitive value"));
                    }
                    text.extend("[object Object]".encode_utf16());
                }
            },
        }
    }
    Ok(JsString::from_utf16(text))
}
pub(crate) fn select(input: BudgetUsageInput<'_>) -> PortResult<BudgetUsageSelection> {
    let original_usage = input.usage.cloned();
    if input.usage.is_some_and(|u| provider_usage(u.value())) {
        return Ok(BudgetUsageSelection::Original(original_usage));
    }
    let content = property(input.assistant_message.value(), "content")?;
    let JsValue::Array(blocks) = content else {
        return Err(match content {
            JsValue::Null | JsValue::Undefined => property(content, "map").unwrap_err(),
            _ => exception(content, "assistantMessage.content.map is not a function"),
        });
    };
    // Complete Source .map first. In particular a later null block or tool
    // serialization exception precedes an earlier text object's join failure.
    let mut mapped = Vec::with_capacity(blocks.len());
    for block in blocks {
        let kind = property(block, "type")?;
        if label(kind, "text") || label(kind, "thinking") {
            mapped.push(property(block, "text")?.clone());
        } else if label(kind, "tool_call") {
            let raw = property(block, "input")?;
            if matches!(raw, JsValue::String(_)) {
                mapped.push(raw.clone());
            } else {
                let empty = JsValue::Object(JsObject::new());
                let raw = if matches!(raw, JsValue::Null | JsValue::Undefined) {
                    &empty
                } else {
                    raw
                };
                let text = stringify_json(raw).map_err(|error| exception(raw, &error.message))?;
                mapped.push(text.map_or(JsValue::Undefined, |text| JsValue::String(text.into())));
            }
        } else {
            mapped.push(JsValue::from(""));
        }
    }
    let mut units = Vec::new();
    for (i, value) in mapped.iter().enumerate() {
        if i > 0 {
            units.push(b'\n' as u16);
        }
        if !matches!(value, JsValue::Null | JsValue::Undefined) {
            units.extend_from_slice(join_text(value)?.units());
        }
    }
    Ok(BudgetUsageSelection::NeedsOutputEstimate(
        BudgetUsageContinuation {
            response_text: JsString::from_utf16(units),
            original_usage,
            estimated_input_tokens: input.estimated_input_tokens,
        },
    ))
}
pub(crate) fn resolve(
    continuation: BudgetUsageContinuation,
    output_tokens: u64,
) -> Option<BoundarySnapshot> {
    if continuation.estimated_input_tokens.is_none() && output_tokens == 0 {
        return continuation.original_usage;
    }
    let mut usage = JsObject::new();
    if let Some(input) = continuation.estimated_input_tokens {
        usage.insert("inputTokens".into(), JsValue::Number(input));
    }
    if output_tokens > 0 {
        usage.insert("outputTokens".into(), JsValue::Number(output_tokens as f64));
    }
    usage.insert(
        "totalTokens".into(),
        JsValue::Number(continuation.estimated_input_tokens.unwrap_or(0.0) + output_tokens as f64),
    );
    Some(BoundarySnapshot::capture(&JsValue::Object(usage)))
}
