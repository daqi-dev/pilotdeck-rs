//! Durable conversation history for a new turn.
//!
//! `AgentTranscriptProjections.ts:459-217` is the baseline: only completed
//! turns contribute messages; `accepted_input` carries user input, the
//! assistant and tool-result entries carry prior output, and a compact
//! boundary discards everything before its replacement.
use crate::storage::read_transcript_limited;
use pilotdeck_protocol::{JsString, JsValue};
use std::path::Path;

/// Messages the model may see before the new input, in transcript order.
pub fn completed_messages(path: &Path) -> std::io::Result<Vec<JsValue>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let read = read_transcript_limited(path, None)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(project(&read.entries))
}

fn project(entries: &[JsValue]) -> Vec<JsValue> {
    let completed: Vec<&JsString> = entries
        .iter()
        .filter(|entry| kind(entry) == Some("turn_result"))
        .filter_map(|entry| entry.get("turnId").and_then(JsValue::as_string))
        .collect();
    let mut boundary = None;
    for (index, entry) in entries.iter().enumerate() {
        if kind(entry) == Some("control_boundary")
            && entry
                .get("boundary")
                .and_then(|value| value.get("subtype"))
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str("compact_boundary"))
        {
            let replacement = entry
                .get("boundary")
                .and_then(|value| value.get("replacementMessages"))
                .and_then(|value| match value {
                    JsValue::Array(items) => Some(items.as_slice()),
                    _ => None,
                });
            if let Some(replacement) = replacement {
                boundary = Some((index, replacement.to_vec()));
            }
        }
    }
    let mut messages = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if boundary.as_ref().is_some_and(|(at, _)| index < *at) {
            continue;
        }
        let turn = entry.get("turnId").and_then(JsValue::as_string);
        let is_complete = turn.is_some_and(|turn| completed.contains(&turn));
        match kind(entry) {
            Some("accepted_input") => {
                if let Some(JsValue::Array(items)) = entry.get("messages") {
                    messages.extend(items.iter().cloned());
                }
            }
            Some("assistant_message" | "tool_result_message" | "durable_message")
                if is_complete =>
            {
                if let Some(message) = entry.get("message") {
                    messages.push(message.clone());
                }
            }
            _ => {}
        }
    }
    if let Some((_, replacement)) = boundary {
        messages.splice(0..0, replacement);
    }
    messages
}

fn kind(value: &JsValue) -> Option<&str> {
    let text = value.get("type").and_then(JsValue::as_string)?;
    Some(match text.to_utf8() {
        Ok(text) if text == "accepted_input" => "accepted_input",
        Ok(text) if text == "assistant_message" => "assistant_message",
        Ok(text) if text == "tool_result_message" => "tool_result_message",
        Ok(text) if text == "durable_message" => "durable_message",
        Ok(text) if text == "turn_result" => "turn_result",
        Ok(text) if text == "control_boundary" => "control_boundary",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    fn entry(json: &str) -> JsValue {
        parse_json(json).unwrap()
    }

    #[test]
    fn keeps_user_input_and_completed_turn_messages_only() {
        let entries = vec![
            entry(
                r#"{"type":"accepted_input","turnId":"a","messages":[{"role":"user","content":"hi"}]}"#,
            ),
            entry(
                r#"{"type":"assistant_message","turnId":"a","message":{"role":"assistant","content":"done"}}"#,
            ),
            entry(r#"{"type":"turn_result","turnId":"a"}"#),
            entry(
                r#"{"type":"assistant_message","turnId":"b","message":{"role":"assistant","content":"partial"}}"#,
            ),
        ];
        let messages = project(&entries);
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0]
                .get("role")
                .and_then(JsValue::as_string)
                .map(|v| v.to_utf8().unwrap()),
            Some("user".into())
        );
        assert_eq!(
            messages[1]
                .get("content")
                .and_then(JsValue::as_string)
                .map(|v| v.to_utf8().unwrap()),
            Some("done".into())
        );
    }
}
