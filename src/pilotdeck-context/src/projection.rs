//! Shared original context projection: maxMessages and tool-pair repair.
use crate::{is_direct_tool_result, is_media_reference_with_id, is_tool_result_only, object};
use pilotdeck_protocol::{JsString, JsValue};
use std::collections::HashSet;
#[derive(Debug, Clone)]
pub struct ProjectionError {
    pub message: String,
}
impl ProjectionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
pub struct MaxMessages {
    pub value: f64,
    pub raw: String,
}

pub struct Warning {
    pub code: &'static str,
    pub message: String,
}
pub struct Projection {
    pub messages: Vec<JsValue>,
    pub dropped_count: f64,
    pub warnings: Vec<Warning>,
}
pub fn project(
    messages: &[JsValue],
    max_messages: Option<&MaxMessages>,
) -> Result<Projection, ProjectionError> {
    let mut warnings = Vec::new();
    let mut projected = messages.to_vec();
    let mut dropped_count = 0.0;
    if let Some(max) = max_messages
        && projected.len() as f64 > max.value
        && !has_compaction_checkpoint(&projected)
    {
        let (kept, dropped) = tool_pair_safe_truncate(projected, max)?;
        projected = kept;
        dropped_count = dropped;
        if dropped > 0.0 {
            warnings.push(Warning {
                code: "context_truncated",
                message: format!(
                    "Truncated {} message(s) to respect maxMessages={}.",
                    pilotdeck_protocol::number::number_to_string(dropped),
                    max.raw
                ),
            });
        }
    }
    projected = repair_tool_result_pairing(projected, &mut warnings);
    Ok(Projection {
        messages: projected,
        dropped_count,
        warnings,
    })
}

fn tool_pair_safe_truncate(
    messages: Vec<JsValue>,
    max: &MaxMessages,
) -> Result<(Vec<JsValue>, f64), ProjectionError> {
    let length = messages.len();
    if length as f64 <= max.value || max.value.is_nan() {
        return Ok((messages, 0.0));
    }
    // `messages.length - maxMessages` in JS. A fractional cut only reaches the
    // `messages[cutIndex]` read when it is below the array length (positive
    // limit); that read is `undefined` and `.content` throws, as in TS.
    let cut = length as f64 - max.value;
    if cut < length as f64 {
        if cut.fract() != 0.0 {
            return Err(ProjectionError::new(
                "Cannot read properties of undefined (reading 'content')",
            ));
        }
        let mut cut = cut as usize;
        while cut < length && is_tool_result_only(&messages[cut]) {
            cut += 1;
        }
        return Ok((messages[cut..].to_vec(), cut as f64));
    }
    // `slice(cutIndex)` with a cut at or past the end is empty. JS keeps the
    // unclamped cut as `droppedCount` (including `Infinity` for -Infinity).
    Ok((Vec::new(), cut))
}

fn repair_tool_result_pairing(messages: Vec<JsValue>, warnings: &mut Vec<Warning>) -> Vec<JsValue> {
    let mut output = Vec::new();
    let mut pending: Vec<JsString> = Vec::new();
    let mut visible = HashSet::new();
    for message in &messages {
        if is_assistant(message) {
            for id in collect_tool_call_ids(message) {
                visible.insert(id);
            }
        }
    }
    for message in messages {
        if is_assistant(&message) {
            if !pending.is_empty() {
                inject_placeholder_results(&pending, &mut output, warnings);
                pending.clear();
            }
            pending = collect_tool_call_ids(&message);
            output.push(message);
            continue;
        }
        let pending_set: HashSet<JsString> = pending.iter().cloned().collect();
        let mut seen: Vec<JsString> = Vec::new();
        let mut media_refs: Vec<JsString> = Vec::new();
        for block in message_content(&message) {
            if is_direct_tool_result(block) {
                if let Some(id) = object_string(block, "toolCallId")
                    && !seen.contains(id)
                {
                    seen.push(id.clone());
                }
            } else if let Some(id) = media_reference_id(block)
                && !media_refs.contains(id)
            {
                media_refs.push(id.clone());
            }
        }
        let has_orphans = seen.iter().any(|id| !pending_set.contains(id))
            || media_refs.iter().any(|id| !visible.contains(id));
        let mut cleaned = message.clone();
        if has_orphans {
            let mut kept = Vec::new();
            for block in message_content(&message) {
                let direct_orphan = is_direct_tool_result(block)
                    && object_string(block, "toolCallId")
                        .is_some_and(|id| !pending_set.contains(id));
                let media_orphan =
                    media_reference_id(block).is_some_and(|id| !visible.contains(id));
                if direct_orphan || media_orphan {
                    warn_orphan(block, warnings);
                } else {
                    kept.push(block.clone());
                }
            }
            if kept.is_empty() {
                for id in &seen {
                    pending.retain(|pending_id| pending_id != id);
                }
                continue;
            }
            if let JsValue::Object(object) = &mut cleaned {
                object.insert("content".into(), JsValue::Array(kept));
            }
        }
        pending.retain(|id| !seen.contains(id));
        output.push(cleaned);
    }
    if !pending.is_empty() {
        inject_placeholder_results(&pending, &mut output, warnings);
    }
    output
}

fn inject_placeholder_results(
    ids: &[JsString],
    output: &mut Vec<JsValue>,
    warnings: &mut Vec<Warning>,
) {
    let blocks = ids
        .iter()
        .map(|id| {
            object(vec![
                ("type", JsValue::String("tool_result".into())),
                ("toolCallId", JsValue::String(id.clone())),
                (
                    "content",
                    JsValue::Array(vec![object(vec![
                        ("type", JsValue::String("text".into())),
                        ("text", JsValue::String("[result truncated]".into())),
                    ])]),
                ),
            ])
        })
        .collect();
    output.push(object(vec![
        ("role", JsValue::String("user".into())),
        ("content", JsValue::Array(blocks)),
    ]));
    for id in ids {
        warnings.push(Warning {
            code: "tool_result_injected",
            message: format!(
                "Injected placeholder tool_result for unmatched tool_call {}.",
                String::from_utf16_lossy(id.units())
            ),
        });
    }
}
fn warn_orphan(block: &JsValue, warnings: &mut Vec<Warning>) {
    let id = object_string(block, "toolCallId")
        .map(|id| String::from_utf16_lossy(id.units()))
        .unwrap_or_else(|| "undefined".into());
    warnings.push(Warning {
        code: "tool_result_orphaned",
        message: format!("tool_result {id} has no matching tool_call — removed."),
    });
}

fn has_compaction_checkpoint(messages: &[JsValue]) -> bool {
    messages.len() >= 2
        && message_has_text_prefix(&messages[0], "user", "<compact-boundary")
        && message_has_text_prefix(
            &messages[1],
            "assistant",
            "[CONTEXT COMPACTION - REFERENCE ONLY]",
        )
}
fn message_has_text_prefix(message: &JsValue, role: &str, prefix: &str) -> bool {
    is_role(message, role)
        && message_content(message).iter().any(|block| {
            is_block_type(block, "text")
                && object_string(block, "text").is_some_and(|text| js_starts_with(text, prefix))
        })
}
fn js_starts_with(value: &JsString, prefix: &str) -> bool {
    let prefix: Vec<u16> = prefix.encode_utf16().collect();
    value.units().starts_with(&prefix)
}

fn collect_tool_call_ids(message: &JsValue) -> Vec<JsString> {
    message_content(message)
        .iter()
        .filter(|block| is_block_type(block, "tool_call"))
        .filter_map(|block| object_string(block, "id").cloned())
        .collect()
}
fn media_reference_id(block: &JsValue) -> Option<&JsString> {
    is_media_reference_with_id(block)
        .then(|| object_string(block, "toolCallId"))
        .flatten()
}
fn is_assistant(message: &JsValue) -> bool {
    is_role(message, "assistant")
}
fn is_role(message: &JsValue, role: &str) -> bool {
    message
        .as_object()
        .and_then(|message| message.get("role"))
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str(role))
}
fn is_block_type(block: &JsValue, kind: &str) -> bool {
    block
        .as_object()
        .and_then(|block| block.get("type"))
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str(kind))
}
fn object_string<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsString> {
    value
        .as_object()
        .and_then(|value| value.get(key))
        .and_then(JsValue::as_string)
}
fn message_content(message: &JsValue) -> &[JsValue] {
    static EMPTY: [JsValue; 0] = [];
    message
        .as_object()
        .and_then(|message| message.get("content"))
        .and_then(|content| match content {
            JsValue::Array(content) => Some(content.as_slice()),
            _ => None,
        })
        .unwrap_or(&EMPTY)
}
