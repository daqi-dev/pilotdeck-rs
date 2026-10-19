//! TS `context/compaction/MicroCompactionEngine.ts`: rewrite older compactable
//! tool results in place, keeping the newest `keepLatest` results verbatim.
use super::*;
use pilotdeck_tokenizer::TokenizeError;

pub const COMPACTABLE_TOOL_NAMES: &[&str] = &[
    "read_file",
    "Read",
    "bash",
    "Bash",
    "grep",
    "Grep",
    "glob",
    "Glob",
    "web_search",
    "WebSearch",
    "web_fetch",
    "WebFetch",
    "edit_file",
    "Edit",
    "write_file",
    "Write",
];

pub const MICROCOMPACT_CLEARED: &str = "[Old tool result content compacted]";
pub const MICROCOMPACT_FAILURES_FOLDED: &str = "[Repeated tool failures compacted]";
pub const MICROCOMPACT_RECOVERED_FAILURE_PREFIX: &str = "[Recovered tool error compacted";

fn compacted_marker() -> String {
    format!("{MICROCOMPACT_CLEARED}\n\nPreview:\n")
}

#[derive(Clone, Debug, Default)]
pub struct MicroCompactionEngineOptions {
    pub keep_latest: Option<f64>,
    pub trim_to_bytes: Option<f64>,
    pub trim_to_tokens: Option<f64>,
    pub protected_tool_names: Option<Vec<String>>,
}

pub struct MicroCompactionInput<'a> {
    pub messages: &'a [JsValue],
    /// Reserved for the caller's time-based decision; this path never reads it.
    pub now_ms: Option<f64>,
    /// Reserved for the caller's time-based decision; this path never reads it.
    pub idle_ms: Option<f64>,
    pub trim_to_bytes: Option<f64>,
    pub trim_to_tokens: Option<f64>,
    pub keep_latest: Option<f64>,
    /// `None` uses the engine's set; `Some(None)` disables protection; a list
    /// replaces the engine's set.
    pub protected_tool_names: Option<Option<Vec<String>>>,
}

impl<'a> MicroCompactionInput<'a> {
    pub fn new(messages: &'a [JsValue]) -> Self {
        Self {
            messages,
            now_ms: None,
            idle_ms: None,
            trim_to_bytes: None,
            trim_to_tokens: None,
            keep_latest: None,
            protected_tool_names: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MicroCompactionResult {
    pub messages: Vec<JsValue>,
    pub rewritten: usize,
    pub rewritten_bytes: f64,
    pub tool_call_ids: Vec<JsString>,
    pub applied_trigger: &'static str,
}

pub struct MicroCompactionEngine {
    options: MicroCompactionEngineOptions,
    protected_tool_names: ProtectedNames,
}

impl MicroCompactionEngine {
    pub fn new(options: MicroCompactionEngineOptions) -> Self {
        let protected_tool_names =
            ProtectedNames::from_options(options.protected_tool_names.clone());
        Self {
            options,
            protected_tool_names,
        }
    }

    pub fn apply(
        &self,
        input: &MicroCompactionInput<'_>,
    ) -> Result<MicroCompactionResult, TokenizeError> {
        let trim_to_tokens = input
            .trim_to_tokens
            .or(self.options.trim_to_tokens)
            .unwrap_or_else(|| {
                let bytes = input
                    .trim_to_bytes
                    .or(self.options.trim_to_bytes)
                    .unwrap_or(1536.0);
                js_max(256.0, (bytes / 2.0).floor())
            });
        let keep_latest = input
            .keep_latest
            .or(self.options.keep_latest)
            .unwrap_or(4.0);

        let names_by_call_id = collect_tool_names_by_call_id(input.messages);
        let protected = match &input.protected_tool_names {
            Some(None) => ProtectedNames::empty(),
            Some(Some(names)) => ProtectedNames::from_options(Some(names.clone())),
            None => self.protected_tool_names.clone(),
        };
        let compactable_call_ids = collect_compactable_tool_call_ids(input.messages);
        let tool_result_indices =
            collect_compactable_tool_result_indices(input.messages, &compactable_call_ids);

        if (tool_result_indices.len() as f64) <= keep_latest {
            return Ok(MicroCompactionResult {
                messages: input.messages.to_vec(),
                rewritten: 0,
                rewritten_bytes: 0.0,
                tool_call_ids: Vec::new(),
                applied_trigger: "skipped",
            });
        }

        // `keepLatest == 0` reads past the array in TS, so `rewriteUntil` is
        // NaN and every index satisfies `index > NaN === false`. Otherwise the
        // newest kept result stays verbatim and everything before it is old.
        let kept_index = tool_result_indices.len() as f64 - keep_latest;
        let rewrite_until: Option<i64> = if kept_index.is_finite()
            && kept_index.fract() == 0.0
            && kept_index >= 0.0
            && kept_index < tool_result_indices.len() as f64
        {
            Some(tool_result_indices[kept_index as usize] as i64 - 1)
        } else {
            // A fractional/negative/zero keepLatest reads a non-index array
            // property in TS: `undefined - 1` is NaN, so every index is in
            // the rewrite window.
            None
        };

        let mut rewritten_ids: Vec<JsString> = Vec::new();
        let mut rewritten_bytes = 0.0f64;
        let mut messages = Vec::with_capacity(input.messages.len());

        for (index, message) in input.messages.iter().enumerate() {
            let in_window = rewrite_until.is_none_or(|until| (index as i64) <= until);
            if !in_window || !is_role(message, "user") {
                messages.push(message.clone());
                continue;
            }
            let mut touched = false;
            let mut new_content: Vec<JsValue> = Vec::with_capacity(content(message).len());
            for block in content(message) {
                if !is_block(block, "tool_result") {
                    if is_block(block, "image") || is_block(block, "pdf") {
                        touched = true;
                        if let Some(data) = string_field(block, "data") {
                            rewritten_bytes += data.len_utf16() as f64;
                        }
                        let cleared = if is_block(block, "image") {
                            "[image cleared]"
                        } else {
                            "[document cleared]"
                        };
                        new_content.push(text_block(cleared.into()));
                    } else {
                        new_content.push(block.clone());
                    }
                    continue;
                }
                let Some(tool_call_id) = string_field(block, "toolCallId") else {
                    new_content.push(block.clone());
                    continue;
                };
                if !compactable_call_ids.contains(tool_call_id) {
                    new_content.push(block.clone());
                    continue;
                }
                let flattened = flatten_tool_result_block_text(block);
                let existing_preview = extract_compacted_preview(&flattened);
                let marker = compacted_marker();
                let bounded_content_budget = js_max(
                    64.0,
                    trim_to_tokens - count_tokens(&JsString::from(marker.as_str()))? as f64 - 4.0,
                );
                if let Some(preview) = &existing_preview
                    && (count_tokens(preview)? as f64 <= bounded_content_budget
                        || count_tokens(&flattened)? as f64 <= trim_to_tokens + 32.0)
                {
                    new_content.push(block.clone());
                    continue;
                }
                if is_protected_tool_call_id(tool_call_id, &names_by_call_id, &protected) {
                    new_content.push(block.clone());
                    continue;
                }
                let size = estimate_tool_result_size(block);
                let source = existing_preview.as_ref().unwrap_or(&flattened);
                if existing_preview.is_none() && count_tokens(source)? as f64 <= trim_to_tokens {
                    new_content.push(block.clone());
                    continue;
                }
                touched = true;
                rewritten_ids.push(tool_call_id.clone());
                rewritten_bytes += size as f64;
                let text = compact_tool_result_text(source, trim_to_tokens)?;
                let mut replacement = match block {
                    JsValue::Object(object) => object.clone(),
                    _ => JsObject::new(),
                };
                replacement.insert("content".into(), JsValue::Array(vec![text_block(text)]));
                new_content.push(JsValue::Object(replacement));
            }
            if touched {
                messages.push(replace_content(message, new_content));
            } else {
                messages.push(message.clone());
            }
        }

        Ok(MicroCompactionResult {
            messages,
            rewritten: rewritten_ids.len(),
            rewritten_bytes,
            applied_trigger: if rewritten_ids.is_empty() {
                "skipped"
            } else {
                "time_based"
            },
            tool_call_ids: rewritten_ids,
        })
    }
}

fn is_compactable(name: &JsString) -> bool {
    name.to_utf8()
        .is_ok_and(|name| COMPACTABLE_TOOL_NAMES.contains(&name.as_str()))
}

fn collect_compactable_tool_call_ids(messages: &[JsValue]) -> IdSet {
    let mut ids = IdSet::default();
    for message in messages {
        if !is_role(message, "assistant") {
            continue;
        }
        for block in content(message) {
            if is_block(block, "tool_call")
                && let Some(id) = string_field(block, "id")
                && string_field(block, "name").is_some_and(is_compactable)
            {
                ids.insert(id.clone());
            }
        }
    }
    ids
}

fn collect_compactable_tool_result_indices(
    messages: &[JsValue],
    compactable_call_ids: &IdSet,
) -> Vec<usize> {
    let mut indices = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if !is_role(message, "user") {
            continue;
        }
        let has_compactable = content(message).iter().any(|block| {
            is_block(block, "tool_result")
                && string_field(block, "toolCallId")
                    .is_some_and(|id| compactable_call_ids.contains(id))
        });
        if has_compactable {
            indices.push(index);
        }
    }
    indices
}

fn estimate_tool_result_size(block: &JsValue) -> usize {
    let mut size = 0usize;
    if let JsValue::Array(items) = field(block, "content") {
        for item in items {
            if is_block(item, "text") {
                if let Some(text) = string_field(item, "text") {
                    size += text.len_utf16();
                }
            } else if (is_block(item, "image") || is_block(item, "pdf"))
                && let Some(data) = string_field(item, "data")
            {
                size += data.len_utf16();
            }
        }
    }
    size
}

fn extract_compacted_preview(text: &JsString) -> Option<JsString> {
    let cleared: Vec<u16> = MICROCOMPACT_CLEARED.encode_utf16().collect();
    let trimmed_start = trim_start_units(text.units());
    if trimmed_start.len() < cleared.len() || trimmed_start[..cleared.len()] != cleared[..] {
        return None;
    }
    let marker: Vec<u16> = compacted_marker().encode_utf16().collect();
    let Some(position) = find_units(text.units(), &marker) else {
        return Some(JsString::default());
    };
    let rest = &text.units()[position + marker.len()..];
    Some(trim(&JsString::from_utf16(rest.to_vec())))
}

fn find_units(haystack: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn compact_tool_result_text(
    text: &JsString,
    trim_to_tokens: f64,
) -> Result<JsString, TokenizeError> {
    let normalized = trim(text);
    if normalized.is_empty() {
        return Ok(JsString::from(MICROCOMPACT_CLEARED));
    }
    let marker = compacted_marker();
    let marker_units: Vec<u16> = marker.encode_utf16().collect();
    let content_budget = js_max(
        64.0,
        trim_to_tokens - count_tokens(&JsString::from(marker.as_str()))? as f64 - 4.0,
    );
    let head_budget = js_max(32.0, (content_budget * 0.7).floor());
    let tail_budget = js_max(16.0, content_budget - head_budget);
    let head = take_token_prefix(&normalized, head_budget)?;
    let tail = take_token_suffix(&normalized, tail_budget)?;
    let mut units = marker_units;
    if count_tokens(&normalized)? as f64 <= content_budget {
        units.extend_from_slice(normalized.units());
    } else {
        units.extend_from_slice(head.units());
        units.extend("\n...[truncated]...\n".encode_utf16());
        units.extend_from_slice(tail.units());
    }
    Ok(JsString::from_utf16(units))
}

fn take_token_prefix(text: &JsString, budget: f64) -> Result<JsString, TokenizeError> {
    if count_tokens(text)? as f64 <= budget {
        return Ok(text.clone());
    }
    let units = text.units();
    let mut low = 0usize;
    let mut high = units.len();
    while low < high {
        let mid = (low + high).div_ceil(2);
        let slice = JsString::from_utf16(units[..mid].to_vec());
        if count_tokens(&slice)? as f64 <= budget {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    Ok(trim_end(&JsString::from_utf16(units[..low].to_vec())))
}

fn take_token_suffix(text: &JsString, budget: f64) -> Result<JsString, TokenizeError> {
    if count_tokens(text)? as f64 <= budget {
        return Ok(text.clone());
    }
    let units = text.units();
    let mut low = 0usize;
    let mut high = units.len();
    while low < high {
        let mid = (low + high).div_ceil(2);
        let slice = JsString::from_utf16(units[units.len() - mid..].to_vec());
        if count_tokens(&slice)? as f64 <= budget {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    Ok(trim_start(&JsString::from_utf16(
        units[units.len() - low..].to_vec(),
    )))
}
