//! Model-free context compaction, ported from the fixed TS baseline
//! `context/compaction/SnipEngine.ts` and `MicroCompactionEngine.ts`.
//!
//! Both engines work on the canonical wire `JsValue` messages that the context
//! runtime hands to the model. Snip prunes middle turns at user/assistant
//! boundaries and injects `<snip-boundary .../>`; micro rewrites old
//! compactable tool results in place. Neither calls a model, so both are pure
//! functions of the message list plus their options.
//!
//! Token estimates go through the already-ported `model_cost` block counter,
//! which uses the shared o200k tokenizer; no second estimator lives here.
//! `String(value)`, JSON serialization and trimming keep the JS semantics from
//! `crate::js_value` / `crate::metadata`.

mod micro;
mod snip;

pub use micro::{
    COMPACTABLE_TOOL_NAMES, MICROCOMPACT_CLEARED, MICROCOMPACT_FAILURES_FOLDED,
    MICROCOMPACT_RECOVERED_FAILURE_PREFIX, MicroCompactionEngine, MicroCompactionEngineOptions,
    MicroCompactionInput, MicroCompactionResult,
};
pub use snip::{
    SnipCallOptions, SnipEngine, SnipEngineOptions, SnipResult, create_snip_boundary,
    is_snip_boundary_message, project_snipped_view,
};

use crate::model_cost;
use pilotdeck_protocol::{
    JsObject, JsString, JsValue,
    model::{
        ContentBlock, ContentBody, ImageBlock, MediaSource, PdfBlock, TextBlock, ThinkingBlock,
        ToolCall, ToolResult, ToolResultContent, ToolResultItem,
    },
};
use std::collections::{HashMap, HashSet};

/// TS `DEFAULT_PROTECTED_TOOL_RESULT_NAMES` (`protectedContext.ts`).
pub(crate) const DEFAULT_PROTECTED_TOOL_RESULT_NAMES: &[&str] = &[
    "read_skill",
    "ReadSkill",
    "ask_user_question",
    "AskUserQuestion",
    "todo_write",
    "TodoWrite",
    "structured_output",
    "StructuredOutput",
    "agent",
    "Agent",
    "Task",
    "task_create",
    "TaskCreate",
    "task_list",
    "TaskList",
    "task_output",
    "TaskOutput",
    "task_wait",
    "TaskWait",
    "task_stop",
    "TaskStop",
];

pub(crate) const CONTINUATION_TEXT: &str =
    "[system: the conversation above has been compacted. please continue with the current task.]";

pub(crate) const INTERNAL_USER_TEXT_PREFIXES: &[&str] = &[
    "<compact-boundary",
    "<snip-boundary",
    "<memory-context>",
    "<internal-compaction-control",
    "<hook_context",
];

pub(crate) static UNDEFINED: JsValue = JsValue::Undefined;

pub(crate) fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}

pub(crate) fn string_field<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsString> {
    field(value, key).as_string()
}

pub(crate) fn role(message: &JsValue) -> Option<&JsString> {
    string_field(message, "role")
}

pub(crate) fn is_role(message: &JsValue, expected: &str) -> bool {
    role(message).is_some_and(|role| role.equals_str(expected))
}

pub(crate) fn content(message: &JsValue) -> &[JsValue] {
    match field(message, "content") {
        JsValue::Array(items) => items,
        _ => &[],
    }
}

pub(crate) fn block_type(block: &JsValue) -> Option<&JsString> {
    string_field(block, "type")
}

pub(crate) fn is_block(block: &JsValue, expected: &str) -> bool {
    block_type(block).is_some_and(|kind| kind.equals_str(expected))
}

/// JS `String#trim` on the UTF-16 representation, so lone surrogates survive.
pub(crate) fn trim_units(units: &[u16]) -> &[u16] {
    trim_end_units(trim_start_units(units))
}

pub(crate) fn trim_start_units(mut units: &[u16]) -> &[u16] {
    while let Some((first, rest)) = units.split_first() {
        if !is_js_whitespace(*first) {
            break;
        }
        units = rest;
    }
    units
}

pub(crate) fn trim_end_units(mut units: &[u16]) -> &[u16] {
    while let Some((last, rest)) = units.split_last() {
        if !is_js_whitespace(*last) {
            break;
        }
        units = rest;
    }
    units
}

fn is_js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0009..=0x000d
            | 0x0020
            | 0x00a0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

pub(crate) fn trim_start(value: &JsString) -> JsString {
    JsString::from_utf16(trim_start_units(value.units()).to_vec())
}

pub(crate) fn trim_end(value: &JsString) -> JsString {
    JsString::from_utf16(trim_end_units(value.units()).to_vec())
}

pub(crate) fn trim(value: &JsString) -> JsString {
    JsString::from_utf16(trim_units(value.units()).to_vec())
}

pub(crate) fn starts_with(value: &JsString, prefix: &str) -> bool {
    let prefix: Vec<u16> = prefix.encode_utf16().collect();
    value.units().len() >= prefix.len() && value.units()[..prefix.len()] == prefix[..]
}

pub(crate) fn text_block(text: JsString) -> JsValue {
    object([
        ("type", JsValue::String("text".into())),
        ("text", JsValue::String(text)),
    ])
}

pub(crate) fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut value = JsObject::new();
    for (key, child) in fields {
        value.insert(key.into(), child);
    }
    JsValue::Object(value)
}

/// TS `protectedToolNameSet`: `undefined` selects the default set, a supplied
/// iterable replaces it verbatim (including the empty set).
#[derive(Clone, Debug)]
pub(crate) struct ProtectedNames {
    names: HashSet<String>,
}

impl ProtectedNames {
    pub(crate) fn from_options(names: Option<Vec<String>>) -> Self {
        match names {
            Some(names) => Self {
                names: names.into_iter().collect(),
            },
            None => Self {
                names: DEFAULT_PROTECTED_TOOL_RESULT_NAMES
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect(),
            },
        }
    }

    pub(crate) fn empty() -> Self {
        Self {
            names: HashSet::new(),
        }
    }

    pub(crate) fn contains(&self, name: &JsString) -> bool {
        name.to_utf8().is_ok_and(|name| self.names.contains(&name))
    }
}

/// `Map<toolCallId, toolName>` gathered from assistant `tool_call` blocks.
pub(crate) fn collect_tool_names_by_call_id(messages: &[JsValue]) -> HashMap<JsString, JsString> {
    let mut names = HashMap::new();
    for message in messages {
        if !is_role(message, "assistant") {
            continue;
        }
        for block in content(message) {
            if is_block(block, "tool_call")
                && let Some(id) = string_field(block, "id")
                && let Some(name) = string_field(block, "name")
            {
                names.insert(id.clone(), name.clone());
            }
        }
    }
    names
}

/// JS `Set` with insertion order retained for `Array.from(set)`.
#[derive(Default)]
pub(crate) struct IdSet {
    order: Vec<JsString>,
    seen: HashSet<JsString>,
}

impl IdSet {
    pub(crate) fn insert(&mut self, id: JsString) {
        if self.seen.insert(id.clone()) {
            self.order.push(id);
        }
    }

    pub(crate) fn contains(&self, id: &JsString) -> bool {
        self.seen.contains(id)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &JsString> {
        self.order.iter()
    }
}

pub(crate) fn collect_tool_call_ids(messages: &[JsValue]) -> IdSet {
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

pub(crate) fn is_direct_tool_result(block: &JsValue) -> bool {
    is_block(block, "tool_result") || is_block(block, "tool_result_reference")
}

pub(crate) fn is_media_reference_with_id(block: &JsValue) -> bool {
    is_block(block, "media_reference")
        && string_field(block, "toolCallId").is_some_and(|id| !id.is_empty())
}

pub(crate) fn collect_tool_result_ids(messages: &[JsValue]) -> IdSet {
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
pub(crate) fn strip_unpaired_tool_calls(messages: &[JsValue], paired: &IdSet) -> Vec<JsValue> {
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
pub(crate) fn strip_unpaired_tool_results(messages: &[JsValue], paired: &IdSet) -> Vec<JsValue> {
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

pub(crate) fn replace_content(message: &JsValue, blocks: Vec<JsValue>) -> JsValue {
    let mut copy = match message {
        JsValue::Object(object) => object.clone(),
        _ => JsObject::new(),
    };
    copy.insert("content".into(), JsValue::Array(blocks));
    JsValue::Object(copy)
}

/// TS `ensureTrailingUserMessage`: providers that reject assistant prefill get
/// a plain continuation user message.
pub(crate) fn ensure_trailing_user_message(mut messages: Vec<JsValue>) -> Vec<JsValue> {
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

/// TS `isToolResultOnly` from `SnipEngine.ts` (media references with a call id
/// also count as tool results).
pub(crate) fn is_tool_result_only(message: &JsValue) -> bool {
    let blocks = content(message);
    if blocks.is_empty() {
        return false;
    }
    blocks.iter().all(|block| {
        is_block(block, "tool_result")
            || is_block(block, "tool_result_reference")
            || is_media_reference_with_id(block)
    })
}

/// TS `isToolResultOnly` from `protectedContext.ts` (no media references).
pub(crate) fn is_tool_result_only_strict(message: &JsValue) -> bool {
    let blocks = content(message);
    if blocks.is_empty() {
        return false;
    }
    blocks
        .iter()
        .all(|block| is_block(block, "tool_result") || is_block(block, "tool_result_reference"))
}

pub(crate) fn is_real_user_request_message(message: &JsValue) -> bool {
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

pub(crate) fn has_memory_context(message: &JsValue) -> bool {
    content(message).iter().any(|block| {
        is_block(block, "text")
            && string_field(block, "text")
                .map(trim_start)
                .is_some_and(|text| starts_with(&text, "<memory-context>"))
    })
}

pub(crate) fn is_protected_context_message(
    message: &JsValue,
    protected: &ProtectedNames,
    names_by_call_id: &HashMap<JsString, JsString>,
) -> bool {
    if has_memory_context(message) {
        return true;
    }
    content(message).iter().any(|block| {
        if is_block(block, "tool_call")
            && string_field(block, "name").is_some_and(|name| protected.contains(name))
        {
            return true;
        }
        if (is_block(block, "tool_result") || is_block(block, "tool_result_reference"))
            && string_field(block, "toolCallId").is_some_and(|id| {
                names_by_call_id
                    .get(id)
                    .is_some_and(|name| protected.contains(name))
            })
        {
            return true;
        }
        false
    })
}

pub(crate) fn is_protected_tool_call_id(
    id: &JsString,
    names_by_call_id: &HashMap<JsString, JsString>,
    protected: &ProtectedNames,
) -> bool {
    names_by_call_id
        .get(id)
        .is_some_and(|name| protected.contains(name))
}

/// TS `flattenToolResultBlockText` (`model/protocol/toolResultContent.ts`).
pub(crate) fn flatten_tool_result_block_text(block: &JsValue) -> JsString {
    let items = match field(block, "content") {
        JsValue::Array(items) => items.as_slice(),
        _ => return JsString::default(),
    };
    let mut units: Vec<u16> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        match block_type(item)
            .and_then(|kind| kind.to_utf8().ok())
            .as_deref()
        {
            Some("text") => {
                if let Some(text) = string_field(item, "text") {
                    units.extend_from_slice(text.units());
                }
            }
            Some("image") => {
                let mime = string_field(item, "mimeType").cloned().unwrap_or_default();
                let data = string_field(item, "data").cloned().unwrap_or_default();
                units.extend("[Image: ".encode_utf16());
                units.extend_from_slice(mime.units());
                units.extend(", ".encode_utf16());
                if string_field(item, "source").is_some_and(|source| source.equals_str("url")) {
                    units.extend_from_slice(data.units());
                } else {
                    units.extend(data.len_utf16().to_string().encode_utf16());
                    units.extend(" base64 characters".encode_utf16());
                }
                units.push(u16::from(b']'));
            }
            Some("pdf") => {
                let mime = string_field(item, "mimeType").cloned().unwrap_or_default();
                let data = string_field(item, "data").cloned().unwrap_or_default();
                units.extend("[PDF: ".encode_utf16());
                units.extend_from_slice(mime.units());
                units.extend(", ".encode_utf16());
                units.extend(data.len_utf16().to_string().encode_utf16());
                units.extend(" base64 characters".encode_utf16());
                let pages = field(item, "pages");
                if js_truthy(pages) {
                    units.extend(", ".encode_utf16());
                    units.extend_from_slice(
                        &crate::js_value::to_string_units(pages).unwrap_or_default(),
                    );
                    units.extend(" pages".encode_utf16());
                }
                units.push(u16::from(b']'));
            }
            _ => {}
        }
    }
    JsString::from_utf16(units)
}

/// JS `Math.max`: NaN wins, unlike Rust's `f64::max`.
pub(crate) fn js_max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else {
        left.max(right)
    }
}

/// JS `Math.min`: NaN wins, unlike Rust's `f64::min`.
pub(crate) fn js_min(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else {
        left.min(right)
    }
}

pub(crate) fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

/// TS `TokenBudgetManager.estimateMessagesTokens` over wire messages. Each
/// block is projected onto the typed canonical model and counted by the
/// shared `model_cost` implementation (same o200k tokenizer, 4-token message
/// overhead, fixed multimedia placeholder).
pub(crate) fn estimate_messages_tokens(
    messages: &[JsValue],
) -> Result<usize, pilotdeck_tokenizer::TokenizeError> {
    let mut total = 0usize;
    for message in messages {
        total += model_cost::PER_MESSAGE_OVERHEAD;
        for block in content(message) {
            if let Some(block) = estimate_block(block) {
                total += model_cost::estimate_block_tokens(&block)?;
            }
        }
    }
    Ok(total)
}

fn estimate_block(block: &JsValue) -> Option<ContentBlock> {
    let body = match block_type(block)
        .and_then(|kind| kind.to_utf8().ok())
        .as_deref()
    {
        Some("text") => ContentBody::Text(TextBlock {
            text: string_field(block, "text").cloned().unwrap_or_default(),
            block_id: None,
        }),
        Some("thinking") => ContentBody::Thinking(ThinkingBlock {
            text: string_field(block, "text").cloned().unwrap_or_default(),
            block_id: None,
            signature: None,
            reasoning_content: string_field(block, "reasoningContent").cloned(),
        }),
        Some("image" | "pdf" | "audio" | "media_reference") => ContentBody::Image(ImageBlock {
            source: MediaSource::Base64,
            data: JsString::default(),
            mime_type: JsString::default(),
            bytes: None,
            detail: None,
        }),
        Some("tool_call") => ContentBody::ToolCall(ToolCall {
            timeline: None,
            id: JsString::default(),
            name: string_field(block, "name").cloned().unwrap_or_default(),
            input: field(block, "input").clone(),
            raw: None,
        }),
        Some("tool_result") => ContentBody::ToolResult(ToolResult {
            tool_call_id: string_field(block, "toolCallId")
                .cloned()
                .unwrap_or_default(),
            content: tool_result_items(block),
            is_error: None,
            raw: None,
        }),
        Some("tool_result_reference") => ContentBody::Text(TextBlock {
            text: string_field(block, "preview").cloned().unwrap_or_default(),
            block_id: None,
        }),
        _ => return None,
    };
    Some(ContentBlock {
        timeline: None,
        body,
    })
}

fn tool_result_items(block: &JsValue) -> Vec<ToolResultItem> {
    let items = match field(block, "content") {
        JsValue::Array(items) => items.as_slice(),
        _ => return Vec::new(),
    };
    items
        .iter()
        .filter_map(|item| {
            let body = match block_type(item)
                .and_then(|kind| kind.to_utf8().ok())
                .as_deref()
            {
                Some("text") => ToolResultContent::Text(TextBlock {
                    text: string_field(item, "text").cloned().unwrap_or_default(),
                    block_id: None,
                }),
                Some("image") => ToolResultContent::Image(ImageBlock {
                    source: if string_field(item, "source")
                        .is_some_and(|source| source.equals_str("url"))
                    {
                        MediaSource::Url
                    } else {
                        MediaSource::Base64
                    },
                    data: string_field(item, "data").cloned().unwrap_or_default(),
                    mime_type: string_field(item, "mimeType").cloned().unwrap_or_default(),
                    bytes: None,
                    detail: None,
                }),
                Some("pdf") => ToolResultContent::Pdf(PdfBlock {
                    data: string_field(item, "data").cloned().unwrap_or_default(),
                    bytes: 0.0,
                    pages: field(item, "pages").as_number(),
                }),
                _ => return None,
            };
            Some(ToolResultItem {
                timeline: None,
                body,
            })
        })
        .collect()
}

pub(crate) fn count_tokens(value: &JsString) -> Result<usize, pilotdeck_tokenizer::TokenizeError> {
    pilotdeck_tokenizer::count_tokens(value)
}
