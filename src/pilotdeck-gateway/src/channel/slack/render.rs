//! `slack-render.ts`: turn-event fragments for the Slack message text. The
//! TS source is byte-identical to `sms-render.ts` apart from the function
//! name (baseline `cc7c4e6`), so the port mirrors `channel/sms/render.rs`
//! case for case.
use pilotdeck_protocol::{JsString, JsValue};

use crate::channel::sms::{join_lines, js_string_of};

fn concat(parts: &[&[u16]]) -> JsString {
    let mut units = Vec::new();
    for part in parts {
        units.extend_from_slice(part);
    }
    JsString::from_utf16(units)
}

fn event_field<'a>(event: &'a JsValue, name: &str) -> Option<&'a JsValue> {
    match event.get(name) {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => None,
        Some(value) => Some(value),
    }
}

/// `renderSlackEvent(event)`: `None` is the TS `undefined` (the caller's
/// `fragment != null` drops it); `Some("")` is the empty-string fragments.
pub(crate) fn render_slack_event(event: &JsValue) -> Option<JsString> {
    let kind = event.get("type").and_then(JsValue::as_string)?;
    if kind.equals_str("assistant_text_delta") {
        // `event.text` may be any wire value; `+=` string-concatenation coerces.
        return event_field(event, "text").map(js_string_of);
    }
    if kind.equals_str("assistant_thinking_delta") || kind.equals_str("tool_call_started") {
        return Some(JsString::from(""));
    }
    if kind.equals_str("tool_call_finished") {
        if !crate::channel::sms::truthy(
            &event_field(event, "ok")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ) {
            // `event.toolName ?? event.toolCallId` — both nullish becomes
            // the template's "undefined".
            if crate::mutants::on("slack-render-failure-fragment") {
                return Some(JsString::from(""));
            }
            let name = event_field(event, "toolName")
                .or_else(|| event_field(event, "toolCallId"))
                .map(js_string_of)
                .unwrap_or_else(|| JsString::from("undefined"));
            return Some(concat(&[
                "\n⚠️ ".encode_utf16().collect::<Vec<_>>().as_slice(),
                name.units(),
                " failed\n".encode_utf16().collect::<Vec<_>>().as_slice(),
            ]));
        }
        return Some(JsString::from(""));
    }
    if kind.equals_str("elicitation_request") {
        let mut lines: Vec<JsString> = Vec::new();
        let questions = crate::channel::sms::elicitation::as_array(event.get("questions"));
        crate::channel::sms::elicitation::push_question_lines(&mut lines, questions, false);
        if lines.is_empty() {
            return Some(JsString::from(""));
        }
        return Some(concat(&[
            "\n".encode_utf16().collect::<Vec<_>>().as_slice(),
            &join_lines(&lines),
            "\n".encode_utf16().collect::<Vec<_>>().as_slice(),
        ]));
    }
    if kind.equals_str("error") {
        let message = event_field(event, "message")
            .map(js_string_of)
            .unwrap_or_else(|| JsString::from("undefined"));
        return Some(concat(&[
            "\n❌ ".encode_utf16().collect::<Vec<_>>().as_slice(),
            message.units(),
            "\n".encode_utf16().collect::<Vec<_>>().as_slice(),
        ]));
    }
    None
}
