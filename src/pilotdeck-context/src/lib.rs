//! Pure fixed-baseline snip/micro compaction and tool-pair integrity.
//! No model, filesystem, permission or session authority.
mod micro;
mod snip;
pub mod tool_pair;
pub use micro::*;
use pilotdeck_budget as model_cost;
use pilotdeck_budget::coercion;
use pilotdeck_protocol::{
    JsObject, JsString, JsValue,
    model::{
        ContentBlock, ContentBody, ImageBlock, MediaSource, PdfBlock, TextBlock, ThinkingBlock,
        ToolCall, ToolResult, ToolResultContent, ToolResultItem,
    },
};
pub use snip::*;
use std::collections::{HashMap, HashSet};

pub use tool_pair::{
    IdSet, collect_tool_call_ids, collect_tool_result_ids, ensure_trailing_user_message,
    is_direct_tool_result, is_media_reference_with_id, is_real_user_request_message,
    strip_unpaired_tool_calls, strip_unpaired_tool_results,
};

/// TS `DEFAULT_PROTECTED_TOOL_RESULT_NAMES` (`protectedContext.ts`).
pub const DEFAULT_PROTECTED_TOOL_RESULT_NAMES: &[&str] = &[
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

pub static UNDEFINED: JsValue = JsValue::Undefined;

pub fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}

pub fn string_field<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsString> {
    field(value, key).as_string()
}

pub fn role(message: &JsValue) -> Option<&JsString> {
    string_field(message, "role")
}

pub fn is_role(message: &JsValue, expected: &str) -> bool {
    role(message).is_some_and(|role| role.equals_str(expected))
}

pub fn content(message: &JsValue) -> &[JsValue] {
    match field(message, "content") {
        JsValue::Array(items) => items,
        _ => &[],
    }
}

pub fn block_type(block: &JsValue) -> Option<&JsString> {
    string_field(block, "type")
}

pub fn is_block(block: &JsValue, expected: &str) -> bool {
    block_type(block).is_some_and(|kind| kind.equals_str(expected))
}

/// JS `String#trim` on the UTF-16 representation, so lone surrogates survive.
pub fn trim_units(units: &[u16]) -> &[u16] {
    trim_end_units(trim_start_units(units))
}

pub fn trim_start_units(mut units: &[u16]) -> &[u16] {
    while let Some((first, rest)) = units.split_first() {
        if !is_js_whitespace(*first) {
            break;
        }
        units = rest;
    }
    units
}

pub fn trim_end_units(mut units: &[u16]) -> &[u16] {
    while let Some((last, rest)) = units.split_last() {
        if !is_js_whitespace(*last) {
            break;
        }
        units = rest;
    }
    units
}

fn is_js_whitespace(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000D | 0x0020 | 0x00A0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}

pub fn trim_start(value: &JsString) -> JsString {
    JsString::from_utf16(trim_start_units(value.units()).to_vec())
}

pub fn trim_end(value: &JsString) -> JsString {
    JsString::from_utf16(trim_end_units(value.units()).to_vec())
}

pub fn trim(value: &JsString) -> JsString {
    JsString::from_utf16(trim_units(value.units()).to_vec())
}

pub fn starts_with(value: &JsString, prefix: &str) -> bool {
    let prefix: Vec<u16> = prefix.encode_utf16().collect();
    value.units().len() >= prefix.len() && value.units()[..prefix.len()] == prefix[..]
}

pub fn text_block(text: JsString) -> JsValue {
    object([
        ("type", JsValue::String("text".into())),
        ("text", JsValue::String(text)),
    ])
}

pub fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut value = JsObject::new();
    for (key, child) in fields {
        value.insert(key.into(), child);
    }
    JsValue::Object(value)
}

/// TS `protectedToolNameSet`: `undefined` selects the default set, a supplied
/// iterable replaces it verbatim (including the empty set).
#[derive(Clone, Debug)]
pub struct ProtectedNames {
    names: HashSet<String>,
}

impl ProtectedNames {
    pub fn from_options(names: Option<Vec<String>>) -> Self {
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

    pub fn empty() -> Self {
        Self {
            names: HashSet::new(),
        }
    }

    pub fn contains(&self, name: &JsString) -> bool {
        name.to_utf8().is_ok_and(|name| self.names.contains(&name))
    }
}

/// `Map<toolCallId, toolName>` gathered from assistant `tool_call` blocks.
pub fn collect_tool_names_by_call_id(messages: &[JsValue]) -> HashMap<JsString, JsString> {
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

pub fn replace_content(message: &JsValue, blocks: Vec<JsValue>) -> JsValue {
    let mut copy = match message {
        JsValue::Object(object) => object.clone(),
        _ => JsObject::new(),
    };
    copy.insert("content".into(), JsValue::Array(blocks));
    JsValue::Object(copy)
}

/// TS `isToolResultOnly` from `SnipEngine.ts` (media references with a call id
/// also count as tool results).
pub fn is_tool_result_only(message: &JsValue) -> bool {
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
pub fn is_tool_result_only_strict(message: &JsValue) -> bool {
    let blocks = content(message);
    if blocks.is_empty() {
        return false;
    }
    blocks
        .iter()
        .all(|block| is_block(block, "tool_result") || is_block(block, "tool_result_reference"))
}

pub fn has_memory_context(message: &JsValue) -> bool {
    content(message).iter().any(|block| {
        is_block(block, "text")
            && string_field(block, "text")
                .map(trim_start)
                .is_some_and(|text| starts_with(&text, "<memory-context>"))
    })
}

pub fn is_protected_context_message(
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

pub fn is_protected_tool_call_id(
    id: &JsString,
    names_by_call_id: &HashMap<JsString, JsString>,
    protected: &ProtectedNames,
) -> bool {
    names_by_call_id
        .get(id)
        .is_some_and(|name| protected.contains(name))
}

/// TS `flattenToolResultBlockText` (`model/protocol/toolResultContent.ts`).
pub fn flatten_tool_result_block_text(block: &JsValue) -> JsString {
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
                        &crate::coercion::to_string_units(pages).unwrap_or_default(),
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

/// JS `Math.max` / `Math.min`: NaN wins and the ±0 tie has a fixed sign.
pub use pilotdeck_protocol::js_math::{max as js_max, min as js_min};

pub fn js_truthy(value: &JsValue) -> bool {
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
pub fn estimate_messages_tokens(
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

pub fn count_tokens(value: &JsString) -> Result<usize, pilotdeck_tokenizer::TokenizeError> {
    pilotdeck_tokenizer::count_tokens(value)
}

pub mod projection;
pub mod prompt;
pub mod recovery;
pub mod summary;

#[cfg(test)]
mod signed_zero_tests {
    use super::{js_max, js_min};

    const PLUS_ZERO: u64 = 0.0f64.to_bits();
    const MINUS_ZERO: u64 = (-0.0f64).to_bits();

    /// Node v22.22.0: `Math.max(+0,-0)` / `Math.max(-0,+0)` are `+0`;
    /// `Math.min(+0,-0)` / `Math.min(-0,+0)` are `-0`.
    #[test]
    fn js_math_signed_zero_ties_match_ts() {
        for (index, (left, right)) in [(0.0, -0.0), (-0.0, 0.0)].into_iter().enumerate() {
            assert_eq!(
                js_max(left, right).to_bits(),
                PLUS_ZERO,
                "#947 compaction js_max tie {index} ({left}, {right}) must be +0"
            );
            assert_eq!(
                js_min(left, right).to_bits(),
                MINUS_ZERO,
                "#947 compaction js_min tie {index} ({left}, {right}) must be -0"
            );
        }
        assert_eq!(
            js_max(-0.0, -0.0).to_bits(),
            MINUS_ZERO,
            "#947 compaction js_max(-0,-0) must stay -0"
        );
        assert_eq!(
            js_min(0.0, 0.0).to_bits(),
            PLUS_ZERO,
            "#947 compaction js_min(+0,+0) must stay +0"
        );
    }
}

#[cfg(test)]
mod whitespace_boundary_tests {
    #[test]
    fn utf16_whitespace_set_matches_ecmascript_for_every_unit() {
        for unit in 0u16..=u16::MAX {
            let expected = matches!(unit, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff);
            assert_eq!(super::is_js_whitespace(unit), expected, "U+{unit:04X}");
        }
    }
}

#[cfg(test)]
mod truthiness_boundary_tests {
    use super::*;
    #[test]
    fn wire_truthiness_keeps_ecmascript_boundary() {
        for value in [
            JsValue::Undefined,
            JsValue::Null,
            JsValue::Bool(false),
            JsValue::Number(0.0),
            JsValue::Number(-0.0),
            JsValue::Number(f64::NAN),
            JsValue::String("".into()),
        ] {
            assert!(!js_truthy(&value), "{value:?}");
        }
        for value in [
            JsValue::Bool(true),
            JsValue::Number(1.0),
            JsValue::Number(f64::INFINITY),
            JsValue::String("0".into()),
            JsValue::Array(vec![]),
            JsValue::Object(JsObject::new()),
        ] {
            assert!(js_truthy(&value), "{value:?}");
        }
    }
}
