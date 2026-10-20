//! TS `context/compaction/toolPairIntegrity.ts`: the one gateway-context port
//! of the tool_call / tool_result pair helpers (pilotdeck-agent's turn.rs keeps
//! its own `ProjectedMessage`/`PortResult` variant for sidecar error fidelity) and the "real user request" predicate.
//! Snip, micro, the full compaction engine and `DefaultContextRuntime`
//! (`prepare.rs`, which imports `isRealUserRequestMessage` from the same TS
//! module) use these; keep no second copy (#342). Public so the TS fixture
//! replay can pin the functions directly.

use super::{
    content, field, is_block, is_role, object, replace_content, starts_with, string_field,
    text_block, trim,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::collections::HashSet;

pub const CONTINUATION_TEXT: &str =
    "[system: the conversation above has been compacted. please continue with the current task.]";

pub const INTERNAL_USER_TEXT_PREFIXES: &[&str] = &[
    "<compact-boundary",
    "<snip-boundary",
    "<memory-context>",
    "<internal-compaction-control",
    "<hook_context",
];

/// JS `Set` with insertion order retained for `Array.from(set)`.
#[derive(Default)]
pub struct IdSet {
    order: Vec<JsString>,
    seen: HashSet<JsString>,
}

impl IdSet {
    pub fn insert(&mut self, id: JsString) {
        if self.seen.insert(id.clone()) {
            self.order.push(id);
        }
    }

    pub fn contains(&self, id: &JsString) -> bool {
        self.seen.contains(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &JsString> {
        self.order.iter()
    }
}

pub fn collect_tool_call_ids(messages: &[JsValue]) -> IdSet {
    let mut ids = IdSet::default();
    for message in messages {
        if !is_role(message, "assistant") {
            continue;
        }
        for block in content(message) {
            if is_block(block, "tool_call")
                && let Some(id) = string_field(block, "id")
            {
                ids.insert(id.clone());
            }
        }
    }
    ids
}

pub fn is_direct_tool_result(block: &JsValue) -> bool {
    is_block(block, "tool_result") || is_block(block, "tool_result_reference")
}

pub fn is_media_reference_with_id(block: &JsValue) -> bool {
    is_block(block, "media_reference")
        && string_field(block, "toolCallId").is_some_and(|id| !id.is_empty())
}

pub fn collect_tool_result_ids(messages: &[JsValue]) -> IdSet {
    let mut ids = IdSet::default();
    for message in messages {
        if !is_role(message, "user") {
            continue;
        }
        for block in content(message) {
            if is_direct_tool_result(block)
                && let Some(id) = string_field(block, "toolCallId")
            {
                ids.insert(id.clone());
            }
        }
    }
    ids
}

/// TS `stripUnpairedToolCalls`: drop tool calls without results, then drop
/// every message with empty content. The trailing `.filter` runs over all
/// roles and also removes messages that already had `content: []` before this
/// pass, not only the ones this pass emptied.
pub fn strip_unpaired_tool_calls(messages: &[JsValue], paired: &IdSet) -> Vec<JsValue> {
    messages
        .iter()
        .map(|message| {
            if !is_role(message, "assistant") {
                return message.clone();
            }
            let blocks = content(message);
            let filtered: Vec<JsValue> = blocks
                .iter()
                .filter(|block| {
                    !is_block(block, "tool_call")
                        || string_field(block, "id").is_some_and(|id| paired.contains(id))
                })
                .cloned()
                .collect();
            if filtered.len() == blocks.len() {
                return message.clone();
            }
            replace_content(message, filtered)
        })
        .filter(|message| !content(message).is_empty())
        .collect()
}

/// TS `stripUnpairedToolResults`: drop tool results / media references whose
/// call is gone, then drop every message with empty content (see
/// `strip_unpaired_tool_calls`).
pub fn strip_unpaired_tool_results(messages: &[JsValue], paired: &IdSet) -> Vec<JsValue> {
    messages
        .iter()
        .map(|message| {
            if !is_role(message, "user") {
                return message.clone();
            }
            let blocks = content(message);
            let filtered: Vec<JsValue> = blocks
                .iter()
                .filter(|block| {
                    (!is_direct_tool_result(block) && !is_media_reference_with_id(block))
                        || string_field(block, "toolCallId").is_some_and(|id| paired.contains(id))
                })
                .cloned()
                .collect();
            if filtered.len() == blocks.len() {
                return message.clone();
            }
            replace_content(message, filtered)
        })
        .filter(|message| !content(message).is_empty())
        .collect()
}

/// TS `ensureTrailingUserMessage`: providers that reject assistant prefill get
/// a plain continuation user message.
pub fn ensure_trailing_user_message(mut messages: Vec<JsValue>) -> Vec<JsValue> {
    if messages.is_empty() {
        return messages;
    }
    if is_role(messages.last().expect("non-empty"), "assistant") {
        messages.push(object([
            ("role", JsValue::String("user".into())),
            (
                "content",
                JsValue::Array(vec![text_block(JsString::from(CONTINUATION_TEXT))]),
            ),
        ]));
    }
    messages
}

pub fn is_real_user_request_message(message: &JsValue) -> bool {
    if !is_role(message, "user") {
        return false;
    }
    if field(message, "metadata").get("synthetic") == Some(&JsValue::Bool(true)) {
        return false;
    }
    content(message).iter().any(|block| {
        if is_direct_tool_result(block) || is_media_reference_with_id(block) {
            return false;
        }
        if !is_block(block, "text") {
            return true;
        }
        let text = string_field(block, "text").map(trim).unwrap_or_default();
        if text.is_empty() {
            return false;
        }
        if text.equals_str(CONTINUATION_TEXT) {
            return false;
        }
        !INTERNAL_USER_TEXT_PREFIXES
            .iter()
            .any(|prefix| starts_with(&text, prefix))
    })
}
