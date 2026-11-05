//! `renderApiServerEvent` and the visible-failure predicates it depends on.
//! Text fragments are UTF-16 so a reply keeps lone surrogates the way JS
//! string concatenation does.
use crate::{js_value::to_string_units, runtime::memory::js_trim_units};
use pilotdeck_protocol::{JsString, JsValue};

/// `VISIBLE_FAILURE_STATUS_EVENTS` in `api-server-render.ts` (baseline
/// cc7c4e6). These are `agent_status.event` names, not gateway event types.
const VISIBLE_FAILURE_STATUS_EVENTS: [&str; 17] = [
    "model_empty_response_exhausted",
    "max_turns_reached",
    "max_output_recovery_exhausted",
    "model_request_failed",
    "tool_call_recovery_exhausted",
    "tool_error_loop",
    "lifecycle_blocked",
    "turn_failed",
    "turn_timeout",
    "gateway_submit_failed",
    "session_busy",
    "gateway_bridge_error",
    "gateway_stream_ended_without_completion",
    "channel_submit_failed",
    "subagent_failed",
    "content_filter_stop",
    "unknown_finish_reason",
];

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

/// `isVisibleFailureStatusDetail`: only an explicit `visible: false` hides.
pub fn is_visible_failure_status_detail(detail: Option<&JsValue>) -> bool {
    let Some(detail) = detail else { return false };
    if matches!(detail.get("visible"), Some(JsValue::Bool(false))) {
        return false;
    }
    matches!(
        detail.get("severity").and_then(JsValue::as_string),
        Some(severity) if severity.equals_str("error")
    )
}

/// `isVisibleFailureGatewayEvent`.
pub fn is_visible_failure_gateway_event(event: &JsValue) -> bool {
    event_type(event).as_deref() == Some("agent_status")
        && is_visible_failure_status_detail(event.get("detail"))
}

/// `isVisibleFailureAgentStatusEvent`.
fn is_visible_failure_agent_status_event(event: &JsValue) -> bool {
    if matches!(
        event.get("detail").and_then(|detail| detail.get("visible")),
        Some(JsValue::Bool(false))
    ) {
        return false;
    }
    let name = event
        .get("event")
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
        .unwrap_or_default();
    VISIBLE_FAILURE_STATUS_EVENTS.contains(&name.as_str())
        || is_visible_failure_status_detail(event.get("detail"))
}

/// `visibleStatusMessage`: a non-blank string message wins, else the fallback.
fn visible_status_message(detail: Option<&JsValue>, fallback: &str) -> JsString {
    if let Some(message) = detail
        .and_then(|detail| detail.get("message"))
        .and_then(JsValue::as_string)
    {
        if !js_trim_units(message.units()).is_empty() {
            return message.clone();
        }
    }
    JsString::from(fallback)
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
