//! `renderApiServerEvent` and the visible-failure predicates it depends on.
//! Text fragments are UTF-16 so a reply keeps lone surrogates the way JS
//! string concatenation does.
use crate::{
    channel::agent_status::{
        is_visible_failure_agent_status_event, is_visible_failure_status_detail, status_message,
    },
    js_value::to_string_units,
};
use pilotdeck_protocol::{JsString, JsValue};

pub fn event_type(event: &JsValue) -> Option<String> {
    event
        .get("type")
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
}

pub fn js_string_of(value: &JsValue) -> JsString {
    match to_string_units(value) {
        Ok(units) => JsString::from_utf16(units),
        Err(_) => JsString::default(),
    }
}

fn truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => false,
        Some(JsValue::Bool(value)) => *value,
        Some(JsValue::Number(number)) => *number != 0.0 && !number.is_nan(),
        Some(JsValue::String(text)) => !text.is_empty(),
        Some(JsValue::Array(_)) | Some(JsValue::Object(_)) => true,
    }
}

fn is_nullish(value: Option<&JsValue>) -> bool {
    matches!(value, None | Some(JsValue::Undefined) | Some(JsValue::Null))
}

/// `isVisibleFailureGatewayEvent`.
pub fn is_visible_failure_gateway_event(event: &JsValue) -> bool {
    event_type(event).as_deref() == Some("agent_status")
        && is_visible_failure_status_detail(event.get("detail"))
}

/// `visibleStatusMessage`: a non-blank string message wins, else the fallback.
fn visible_status_message(detail: Option<&JsValue>, fallback: &str) -> JsString {
    status_message(detail).unwrap_or_else(|| JsString::from(fallback))
}

fn wrap_error(message: &JsString) -> JsString {
    let mut units = "\nError: ".encode_utf16().collect::<Vec<_>>();
    units.extend_from_slice(message.units());
    units.extend_from_slice(&"\n".encode_utf16().collect::<Vec<_>>());
    JsString::from_utf16(units)
}

fn tool_line(prefix: &str, name: &JsString, suffix: &str) -> JsString {
    let mut units = prefix.encode_utf16().collect::<Vec<_>>();
    units.extend_from_slice(name.units());
    units.extend_from_slice(&suffix.encode_utf16().collect::<Vec<_>>());
    JsString::from_utf16(units)
}

/// `renderApiServerEvent`: `None` means "no fragment" (JS `undefined`).
pub fn render(event: &JsValue) -> Option<JsString> {
    match event_type(event)?.as_str() {
        "assistant_text_delta" => event.get("text").and_then(JsValue::as_string).cloned(),
        "assistant_thinking_delta" => Some(JsString::default()),
        "tool_call_started" => {
            let name = event
                .get("name")
                .map(js_string_of)
                .unwrap_or_else(|| JsString::from("undefined"));
            Some(tool_line("\n[", &name, " running]\n"))
        }
        "tool_call_finished" => {
            let name = if is_nullish(event.get("toolName")) {
                event
                    .get("toolCallId")
                    .map(js_string_of)
                    .unwrap_or_else(|| JsString::from("undefined"))
            } else {
                event.get("toolName").map(js_string_of).unwrap_or_default()
            };
            Some(tool_line(
                "\n[",
                &name,
                if truthy(event.get("ok")) {
                    " done]\n"
                } else {
                    " failed]\n"
                },
            ))
        }
        "agent_status" => {
            if !is_visible_failure_agent_status_event(event) {
                return None;
            }
            let message = visible_status_message(
                event.get("detail"),
                "Agent execution stopped before producing a complete response.",
            );
            Some(wrap_error(&message))
        }
        "error" => {
            let message = event
                .get("message")
                .map(js_string_of)
                .unwrap_or_else(|| JsString::from("undefined"));
            Some(wrap_error(&message))
        }
        _ => None,
    }
}
