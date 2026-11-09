//! Reducer for the gateway events consumed by the TUI.
//!
//! Values use the shared JS wire representation, retaining property order and
//! UTF-16 code units. The caller must supply events satisfying GatewayEvent.
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub messages: Vec<JsValue>,
    pub activity: Vec<JsValue>,
    pub mode: JsValue,
    pub is_running: bool,
    pub pending_permissions: Vec<JsValue>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            messages: Vec::new(),
            activity: Vec::new(),
            mode: "default".into(),
            is_running: false,
            pending_permissions: Vec::new(),
        }
    }
}

fn field(value: &JsValue, name: &str) -> JsValue {
    value.get(name).cloned().unwrap_or(JsValue::Undefined)
}
fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let mut result = JsObject::new();
    for (name, value) in fields {
        result.insert((*name).into(), value.clone());
    }
    JsValue::Object(result)
}
fn string(value: &JsValue) -> Option<&JsString> {
    value.as_string()
}
fn is(value: &JsValue, expected: &str) -> bool {
    string(value).is_some_and(|s| s.equals_str(expected))
}
fn concat(a: &JsString, b: &JsString) -> JsString {
    JsString::from_utf16(a.units().iter().chain(b.units()).copied().collect())
}
fn text(value: &JsValue) -> JsString {
    string(value).cloned().unwrap_or_default()
}
fn prefixed(prefix: &str, suffix: &JsString) -> JsValue {
    JsValue::String(concat(&prefix.into(), suffix))
}
fn activity(id: JsValue, text: JsValue, status: &str) -> JsValue {
    object(&[("id", id), ("text", text), ("status", status.into())])
}
fn prepend(items: &mut Vec<JsValue>, item: JsValue) {
    items.insert(0, item);
    items.truncate(8);
}
// JSON.stringify(value, null, 2), using the shared JS encoder for escaping,
// number spelling, property ordering, and undefined handling.
pub(super) fn pretty(value: &JsValue) -> Result<Option<String>, pilotdeck_protocol::CodecError> {
    let Some(raw) = stringify_json(value)? else {
        return Ok(None);
    };
    let mut out = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let chars: Vec<char> = raw.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if quoted {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                if !matches!(chars.get(i + 1), Some('}' | ']')) {
                    depth += 1;
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                }
            }
            '}' | ']' => {
                if !matches!(chars.get(i.wrapping_sub(1)), Some('{' | '[')) {
                    depth -= 1;
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                }
                out.push(c);
            }
            ',' => {
                out.push(c);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            ':' => out.push_str(": "),
            _ => out.push(c),
        }
    }
    Ok(Some(out))
}

/// Applies a valid GatewayEvent. A structured output that cannot be encoded
/// returns the codec error rather than claiming it was rendered.
pub fn apply(state: &mut State, event: &JsValue) -> Result<(), pilotdeck_protocol::CodecError> {
    let event_type = field(event, "type");
    if is(&event_type, "turn_started") {
        state.is_running = true;
        state.activity.clear();
        state.pending_permissions.clear();
    } else if is(&event_type, "assistant_text_delta") || is(&event_type, "assistant_thinking_delta")
    {
        let thinking = is(&event_type, "assistant_thinking_delta");
        let delta = text(&field(event, "text"));
        if !delta.is_empty() {
            if state
                .messages
                .last()
                .is_some_and(|m| is(&field(m, "role"), "assistant"))
            {
                let last = state.messages.pop().expect("last message exists");
                let old_text = text(&field(&last, "text"));
                let old_thinking = field(&last, "thinking");
                let new_text = if thinking {
                    old_text
                } else {
                    concat(&old_text, &delta)
                };
                let new_thinking = if thinking {
                    JsValue::String(concat(&text(&old_thinking), &delta))
                } else {
                    old_thinking
                };
                state.messages.push(object(&[
                    ("role", "assistant".into()),
                    ("text", JsValue::String(new_text)),
                    ("thinking", new_thinking),
                ]));
            } else {
                let mut fields = vec![
                    ("role", "assistant".into()),
                    (
                        "text",
                        if thinking {
                            "".into()
                        } else {
                            JsValue::String(delta.clone())
                        },
                    ),
                ];
                if thinking {
                    fields.push(("thinking", JsValue::String(delta.clone())));
                }
                state.messages.push(object(&fields));
            }
        }
        if thinking {
            let length = state
                .messages
                .last()
                .filter(|m| is(&field(m, "role"), "assistant"))
                .map_or(0, |m| text(&field(m, "thinking")).len_utf16());
            prepend(
                &mut state.activity,
                activity(
                    format!("thinking-{length}").as_str().into(),
                    JsValue::String(delta),
                    "info",
                ),
            );
        }
    } else if is(&event_type, "tool_call_started") {
        prepend(
            &mut state.activity,
            activity(field(event, "toolCallId"), field(event, "name"), "running"),
        );
    } else if is(&event_type, "tool_call_finished") {
        let preview = text(&field(event, "resultPreview"));
        let units = preview.units();
        let whitespace = |u: &u16| matches!(*u, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff);
        let start = units
            .iter()
            .position(|u| !whitespace(u))
            .unwrap_or(units.len());
        let end = units
            .iter()
            .rposition(|u| !whitespace(u))
            .map_or(start, |i| i + 1);
        let preview = JsString::from_utf16(units[start..end].to_vec());
        let text = if preview.is_empty() {
            if field(event, "ok").as_bool() == Some(true) {
                "ok".into()
            } else {
                "error".into()
            }
        } else {
            JsValue::String(preview)
        };
        let id = field(event, "toolCallId");
        state.messages.push(object(&[
            ("role", "tool".into()),
            ("text", text),
            ("ok", field(event, "ok")),
            ("toolCallId", id.clone()),
            ("toolName", field(event, "toolName")),
            ("lineCount", field(event, "resultLineCount")),
            ("resultPath", field(event, "resultPath")),
        ]));
        state.activity.retain(|a| field(a, "id") != id);
    } else if is(&event_type, "permission_request") {
        let id = field(event, "requestId");
        let name = field(event, "toolName");
        state.pending_permissions.push(object(&[
            ("requestId", id.clone()),
            ("toolName", name.clone()),
            ("payload", field(event, "payload")),
        ]));
        prepend(
            &mut state.activity,
            activity(id, prefixed("permission: ", &text(&name)), "info"),
        );
    } else if is(&event_type, "structured_output") {
        let rendered = pretty(&field(event, "payload"))?
            .map_or(JsValue::Undefined, |s| JsValue::String(s.into()));
        state
            .messages
            .push(object(&[("role", "system".into()), ("text", rendered)]));
    } else if is(&event_type, "plan_mode_changed") {
        state.mode = field(event, "mode");
    } else if is(&event_type, "turn_completed") || is(&event_type, "error") {
        state.is_running = false;
        state.activity.clear();
        state.pending_permissions.clear();
        if is(&event_type, "error") {
            state.messages.push(object(&[
                ("role", "error".into()),
                ("text", field(event, "message")),
            ]));
        }
    } else if is(&event_type, "tool_result_detail_available") {
        for message in &mut state.messages {
            if is(&field(message, "role"), "tool")
                && field(message, "toolCallId") == field(event, "toolCallId")
            {
                let path = field(event, "resultPath");
                let full = field(event, "fullText");
                let path = if matches!(path, JsValue::Undefined | JsValue::Null) {
                    field(message, "resultPath")
                } else {
                    path
                };
                let full = if matches!(full, JsValue::Undefined | JsValue::Null) {
                    field(message, "fullText")
                } else {
                    full
                };
                if let JsValue::Object(obj) = message {
                    obj.insert("resultPath".into(), path);
                    obj.insert("fullText".into(), full);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    fn array(value: &JsValue) -> &[JsValue] {
        match value {
            JsValue::Array(values) => values,
            _ => panic!("expected array"),
        }
    }
    fn state_value(state: &State) -> JsValue {
        object(&[
            ("messages", JsValue::Array(state.messages.clone())),
            ("activity", JsValue::Array(state.activity.clone())),
            ("mode", state.mode.clone()),
            ("isRunning", JsValue::Bool(state.is_running)),
            (
                "pendingPermissions",
                JsValue::Array(state.pending_permissions.clone()),
            ),
        ])
    }
    #[test]
    fn matches_recorded_typescript_after_each_event() {
        let fixture = parse_json(include_str!("../../tests/fixtures/tui/reducer.json")).unwrap();
        let initial = field(&fixture, "initial");
        let mut state = State {
            messages: array(&field(&initial, "messages")).to_vec(),
            activity: array(&field(&initial, "activity")).to_vec(),
            mode: field(&initial, "mode"),
            is_running: field(&initial, "isRunning").as_bool().unwrap(),
            pending_permissions: array(&field(&initial, "pendingPermissions")).to_vec(),
        };
        let events = field(&fixture, "events");
        let states = field(&fixture, "states");
        assert!(!array(&events).is_empty());
        assert_eq!(array(&events).len(), array(&states).len());
        for (index, (event, expected)) in array(&events).iter().zip(array(&states)).enumerate() {
            apply(&mut state, event).unwrap();
            let actual = stringify_json(&state_value(&state)).unwrap().unwrap();
            let expected = stringify_json(expected).unwrap().unwrap();
            assert_eq!(actual, expected, "event {index}: {event:?}");
        }
    }
}
