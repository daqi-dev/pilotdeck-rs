//! Tool-result token/byte budget and spill, plus the message rewrite used by
//! `applyToolResults`.
//!
//! Ported from PilotDeck `src/context/budget/ToolResultBudget.ts` and
//! `src/context/DefaultContextRuntime.ts:427-473`. Token estimates use the
//! shared o200k tokenizer (`pilotdeck-tokenizer`; the same counter behind
//! `model_cost`). JavaScript `String`/`Buffer` semantics come from
//! `crate::js_value` and the explicit UTF-16 unit helpers below.
//!
//! `hydrateReferences` is not ported: the Rust host has no artifact-storage
//! cache yet, so resume hydration stays with the storage owner.
use crate::host::modules::budget::flatten_result;
use crate::js_value::number_string;
use crate::metadata::js_trim;
use crate::runtime::tools::builtin::{basename, dirname, is_absolute, join, normalize, relative};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    fs, io,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const DEFAULT_MAX_RESULT_SIZE_TOKENS: usize = 10_000;
pub const DEFAULT_MAX_RESULT_SIZE_CHARS: usize = 200_000;
pub const PREVIEW_SIZE_BYTES: usize = 12_000;

const EXACT_TOKEN_COUNT_MAX_BYTES: usize = 40_000;
const TOKEN_ESTIMATE_SAMPLE_BYTES: usize = 6_000;

#[derive(Clone, Debug)]
pub struct ToolResultBudgetOptions {
    pub tool_results_dir: PathBuf,
    pub max_result_size_chars: Option<usize>,
    pub max_result_size_tokens: Option<usize>,
    pub preview_bytes: Option<usize>,
}

impl ToolResultBudgetOptions {
    pub fn new(tool_results_dir: impl Into<PathBuf>) -> Self {
        Self {
            tool_results_dir: tool_results_dir.into(),
            max_result_size_chars: None,
            max_result_size_tokens: None,
            preview_bytes: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolBudgetError {
    pub message: String,
}

impl ToolBudgetError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ToolBudgetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug, Clone)]
struct ReplacementRecord {
    tool_call_id: JsString,
    is_error: Option<bool>,
    path: JsString,
    read_file_path: JsString,
    original_bytes: usize,
    preview: JsString,
    mime_type: JsString,
}

#[derive(Default)]
struct State {
    replacements: HashMap<JsString, ReplacementRecord>,
    next_read_file_alias_index: u64,
}

/// Replacement cache and spill writer shared by one context runtime.
pub struct ToolResultBudget {
    max_result_size_chars: usize,
    max_result_size_tokens: usize,
    preview_bytes: usize,
    tool_results_dir: JsString,
    state: Mutex<State>,
}

impl ToolResultBudget {
    pub fn new(options: ToolResultBudgetOptions) -> Self {
        let raw = JsString::from(options.tool_results_dir.to_string_lossy().as_ref());
        let tool_results_dir = if is_absolute(&raw) {
            normalize(&raw)
        } else {
            let cwd = std::env::current_dir()
                .map(|path| JsString::from(path.to_string_lossy().as_ref()))
                .unwrap_or_else(|_| JsString::from("/"));
            join(&cwd, &raw)
        };
        Self {
            max_result_size_chars: options
                .max_result_size_chars
                .unwrap_or(DEFAULT_MAX_RESULT_SIZE_CHARS),
            max_result_size_tokens: options
                .max_result_size_tokens
                .unwrap_or(DEFAULT_MAX_RESULT_SIZE_TOKENS),
            preview_bytes: options.preview_bytes.unwrap_or(PREVIEW_SIZE_BYTES),
            tool_results_dir,
            state: Mutex::new(State {
                replacements: HashMap::new(),
                next_read_file_alias_index: 1,
            }),
        }
    }

    pub fn tool_results_dir(&self) -> &JsString {
        &self.tool_results_dir
    }

    /// `ToolResultBudget.applyToMessage`.
    pub async fn apply_to_message(
        &self,
        message: &JsValue,
        turn_id: Option<&JsValue>,
    ) -> Result<JsValue, ToolBudgetError> {
        require_message(message)?;
        if !is_user_message(message) {
            return Ok(message.clone());
        }
        let content = block_array(message, "content")?;
        let mut primary: Vec<JsValue> = Vec::with_capacity(content.len());
        let mut media_references: Vec<JsValue> = Vec::new();
        let mut modified = false;
        for block in content {
            if !is_block_type(block, "tool_result") {
                primary.push(block.clone());
                continue;
            }
            let (replaced, references, changed) =
                self.maybe_replace_tool_result(block, turn_id).await?;
            modified |= changed || !references.is_empty();
            primary.push(replaced);
            media_references.extend(references);
        }
        if !modified {
            return Ok(message.clone());
        }
        primary.extend(media_references);
        Ok(replace_content(message, primary))
    }

    /// `ToolResultBudget.applyToSupplementalMessage`.
    pub async fn apply_to_supplemental_message(
        &self,
        message: &JsValue,
        tool_call_id: Option<&JsValue>,
        turn_id: Option<&JsValue>,
    ) -> Result<JsValue, ToolBudgetError> {
        require_message(message)?;
        if !is_user_message(message) {
            return Ok(message.clone());
        }
        let content = block_array(message, "content")?;
        let mut rewritten = Vec::with_capacity(content.len());
        let mut modified = false;
        for (index, block) in content.iter().enumerate() {
            let (replaced, changed) = self
                .maybe_replace_media(block, index, tool_call_id, turn_id)
                .await?;
            modified |= changed;
            rewritten.push(replaced);
        }
        if !modified {
            return Ok(message.clone());
        }
        Ok(replace_content(message, rewritten))
    }

    async fn maybe_replace_tool_result(
        &self,
        block: &JsValue,
        turn_id: Option<&JsValue>,
    ) -> Result<(JsValue, Vec<JsValue>, bool), ToolBudgetError> {
        let content = block_array(block, "content")?.to_vec();
        if !content.iter().any(is_tool_result_media_block) {
            let (block, changed) = self.maybe_replace_text_tool_result(block, turn_id).await?;
            return Ok((block, Vec::new(), changed));
        }
        let tool_call_id = block.get("toolCallId");
        let mut rewritten = Vec::with_capacity(content.len());
        let mut references = Vec::new();
        for (index, entry) in content.iter().enumerate() {
            if !is_tool_result_media_block(entry) {
                rewritten.push(entry.clone());
                continue;
            }
            let (replaced, changed) = self
                .maybe_replace_media(entry, index, tool_call_id, turn_id)
                .await?;
            if changed {
                let preview = replaced
                    .get("preview")
                    .cloned()
                    .unwrap_or(JsValue::Undefined);
                rewritten.push(object([("type", "text".into()), ("text", preview)]));
                references.push(replaced);
            } else {
                rewritten.push(entry.clone());
            }
        }
        if references.is_empty() {
            return Ok((block.clone(), Vec::new(), false));
        }
        Ok((replace_content(block, rewritten), references, true))
    }

    async fn maybe_replace_text_tool_result(
        &self,
        block: &JsValue,
        turn_id: Option<&JsValue>,
    ) -> Result<(JsValue, bool), ToolBudgetError> {
        let tool_call_id = tool_call_id_string(block.get("toolCallId"))?;
        let replacement_key = scoped_tool_result_key(&tool_call_id, turn_id)?;
        if let Some(record) = self
            .state
            .lock()
            .unwrap()
            .replacements
            .get(&replacement_key)
            .cloned()
        {
            return Ok((to_reference_block(&record), true));
        }

        let flat = flatten_result(block).map_err(|error| ToolBudgetError::new(error.message))?;
        let flat = flat
            .as_string()
            .cloned()
            .ok_or_else(|| ToolBudgetError::new("flattened tool result is not a string"))?;
        let byte_length = utf8_len(&flat);
        if self.should_inline_text_result(&flat, byte_length)? {
            return Ok((block.clone(), false));
        }

        let is_json = looks_like_json(&flat);
        let extension = if is_json { "json" } else { "txt" };
        let replacement_name = String::from_utf16_lossy(replacement_key.units());
        let path = join(
            &self.tool_results_dir,
            &JsString::from(format!("{replacement_name}.{extension}")),
        );
        self.persist_artifact(&self.tool_results_dir, &path, &flat)
            .await?;
        let read_file_path = self.create_read_file_alias(&path, extension).await?;

        let preview = head_tail_preview(&flat, self.preview_bytes);
        let record = ReplacementRecord {
            tool_call_id,
            is_error: block.get("isError").and_then(JsValue::as_bool),
            path,
            read_file_path,
            original_bytes: byte_length,
            preview,
            mime_type: if is_json {
                JsString::from("application/json")
            } else {
                JsString::from("text/plain")
            },
        };
        self.state
            .lock()
            .unwrap()
            .replacements
            .insert(replacement_key, record.clone());
        Ok((to_reference_block(&record), true))
    }

    fn should_inline_text_result(
        &self,
        text: &JsString,
        byte_length: usize,
    ) -> Result<bool, ToolBudgetError> {
        if byte_length > self.max_result_size_chars {
            return Ok(false);
        }
        Ok(estimate_tokens(text, byte_length)? <= self.max_result_size_tokens)
    }

    async fn create_read_file_alias(
        &self,
        source: &JsString,
        extension: &str,
    ) -> Result<JsString, ToolBudgetError> {
        let (refs_dir, workspace_root) = self.read_file_alias_location();
        loop {
            let index = {
                let mut state = self.state.lock().unwrap();
                let index = state.next_read_file_alias_index.max(1);
                state.next_read_file_alias_index = index + 1;
                index
            };
            let alias = join(
                &refs_dir,
                &JsString::from(format!("result-{index:04}.{extension}")),
            );
            if copy_file_if_absent(&refs_dir, source, &alias)? {
                return Ok(relative(&workspace_root, &alias));
            }
        }
    }

    fn read_file_alias_location(&self) -> (JsString, JsString) {
        let maybe_tool_results_root = dirname(&self.tool_results_dir);
        if basename(&maybe_tool_results_root).equals_str("tool-results") {
            (
                join(&maybe_tool_results_root, &JsString::from("refs")),
                dirname(&dirname(&maybe_tool_results_root)),
            )
        } else {
            (
                join(&self.tool_results_dir, &JsString::from("refs")),
                dirname(&dirname(&self.tool_results_dir)),
            )
        }
    }

    async fn persist_artifact(
        &self,
        root: &JsString,
        path: &JsString,
        content: &JsString,
    ) -> Result<(), ToolBudgetError> {
        write_text_if_absent(root, path, content)?;
        Ok(())
    }

    async fn maybe_replace_media(
        &self,
        block: &JsValue,
        index: usize,
        tool_call_id: Option<&JsValue>,
        turn_id: Option<&JsValue>,
    ) -> Result<(JsValue, bool), ToolBudgetError> {
        let Some(media_type) = block_type(block) else {
            return Ok((block.clone(), false));
        };
        if !matches!(media_type, "image" | "pdf" | "audio") {
            return Ok((block.clone(), false));
        }
        let data = media_data(block)?;
        let original_bytes = block
            .get("bytes")
            .and_then(JsValue::as_number)
            .unwrap_or(utf8_len(&data) as f64);
        let encoded_bytes = utf8_len(&data);
        if encoded_bytes <= self.max_result_size_chars {
            return Ok((block.clone(), false));
        }
        // TS reads `mimeType` only after the size guard, so a small malformed
        // media block stays untouched instead of throwing. PDF's
        // `extensionForMedia` branch returns before `.split`, so a missing
        // mimeType is kept as `undefined` (JSON drops the output key).
        let mime_type = if media_type == "pdf" {
            block.get("mimeType").cloned().unwrap_or(JsValue::Undefined)
        } else {
            JsValue::String(media_mime_type(block)?)
        };

        let extension = extension_for_media(media_type, &mime_type);
        let tool_call_id = tool_call_id_string(tool_call_id)?;
        let scoped_key = scoped_tool_result_key(&tool_call_id, turn_id)?;
        let hash = hash_string(&data);
        let id = format!(
            "{}-{}-{}-{}",
            String::from_utf16_lossy(scoped_key.units()),
            media_type,
            index,
            &hash[..12.min(hash.len())]
        );
        let path = join(
            &self.tool_results_dir,
            &JsString::from(format!("{id}.{extension}")),
        );
        self.persist_artifact(&self.tool_results_dir, &path, &data)
            .await?;

        let preview = media_preview(media_type, &mime_type, original_bytes, block);
        let mut object = JsObject::new();
        object.insert("type".into(), "media_reference".into());
        object.insert("toolCallId".into(), JsValue::String(tool_call_id.clone()));
        object.insert("path".into(), JsValue::String(path));
        object.insert("originalBytes".into(), JsValue::Number(original_bytes));
        object.insert("preview".into(), JsValue::String(preview));
        object.insert("hasMore".into(), JsValue::Bool(true));
        object.insert("mimeType".into(), mime_type);
        object.insert("mediaType".into(), JsValue::String(media_type.into()));
        if media_type == "pdf"
            && let Some(pages) = block.get("pages").filter(|value| !value.is_undefined())
        {
            object.insert("pages".into(), pages.clone());
        }
        if media_type == "image"
            && let Some(detail) = block.get("detail").filter(|value| truthy(value))
        {
            object.insert("detail".into(), detail.clone());
        }
        object.insert("reason".into(), "media_result_too_large".into());
        Ok((JsValue::Object(object), true))
    }
}

fn require_message(message: &JsValue) -> Result<(), ToolBudgetError> {
    match message {
        JsValue::Undefined => Err(ToolBudgetError::new(
            "Cannot read properties of undefined (reading 'role')",
        )),
        JsValue::Null => Err(ToolBudgetError::new(
            "Cannot read properties of null (reading 'role')",
        )),
        _ => Ok(()),
    }
}

fn is_user_message(message: &JsValue) -> bool {
    message
        .get("role")
        .and_then(JsValue::as_string)
        .is_some_and(|role| role.equals_str("user"))
}

fn block_array<'a>(value: &'a JsValue, field: &str) -> Result<&'a [JsValue], ToolBudgetError> {
    match value.get(field) {
        Some(JsValue::Array(items)) => Ok(items),
        Some(_) => Err(ToolBudgetError::new(format!(
            "{field}.map is not a function"
        ))),
        None => Err(ToolBudgetError::new(format!(
            "Cannot read properties of undefined (reading '{field}')"
        ))),
    }
}

fn block_type(value: &JsValue) -> Option<&'static str> {
    let kind = value.get("type").and_then(JsValue::as_string)?;
    [
        "text",
        "image",
        "pdf",
        "audio",
        "tool_result",
        "tool_result_reference",
        "media_reference",
    ]
    .into_iter()
    .find(|name| kind.equals_str(name))
}

fn is_block_type(value: &JsValue, expected: &str) -> bool {
    value
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str(expected))
}

fn is_tool_result_media_block(value: &JsValue) -> bool {
    is_block_type(value, "image") || is_block_type(value, "pdf")
}

fn media_data(block: &JsValue) -> Result<JsString, ToolBudgetError> {
    match block.get("data") {
        Some(JsValue::String(value)) => Ok(value.clone()),
        Some(JsValue::Null) | None => Err(ToolBudgetError::new(
            "The \"string\" argument must be of type string or an instance of Buffer or ArrayBuffer. Received null",
        )),
        Some(JsValue::Undefined) => Err(ToolBudgetError::new(
            "The \"string\" argument must be of type string or an instance of Buffer or ArrayBuffer. Received undefined",
        )),
        Some(_) => Err(ToolBudgetError::new(
            "The \"string\" argument must be of type string or an instance of Buffer or ArrayBuffer. Received an unsupported value",
        )),
    }
}

fn media_mime_type(block: &JsValue) -> Result<JsString, ToolBudgetError> {
    match block.get("mimeType") {
        Some(JsValue::String(value)) => Ok(value.clone()),
        Some(JsValue::Null) => Err(ToolBudgetError::new(
            "Cannot read properties of null (reading 'split')",
        )),
        Some(JsValue::Undefined) | None => Err(ToolBudgetError::new(
            "Cannot read properties of undefined (reading 'split')",
        )),
        Some(_) => Err(ToolBudgetError::new("mimeType.split is not a function")),
    }
}

/// V8's `String.prototype.trim` TypeError, named after `safePathPart`'s
/// parameter (`value`). `null`/`undefined` keep V8's property-read wording.
fn trim_type_error(value: &JsValue) -> ToolBudgetError {
    match value {
        JsValue::Null => ToolBudgetError::new("Cannot read properties of null (reading 'trim')"),
        JsValue::Undefined => {
            ToolBudgetError::new("Cannot read properties of undefined (reading 'trim')")
        }
        _ => ToolBudgetError::new("value.trim is not a function"),
    }
}

fn safe_path_part_value(value: &JsValue) -> Result<String, ToolBudgetError> {
    match value {
        JsValue::String(value) => Ok(safe_path_part(&String::from_utf16_lossy(value.units()))),
        _ => Err(trim_type_error(value)),
    }
}

/// `safePathPart(toolCallId)` is the first read in `scopedToolResultKey`; a
/// missing id is `undefined`, so it fails on `.trim` before `turnId`.
fn tool_call_id_string(value: Option<&JsValue>) -> Result<JsString, ToolBudgetError> {
    match value {
        Some(JsValue::String(value)) => Ok(value.clone()),
        Some(value) => Err(trim_type_error(value)),
        None => Err(trim_type_error(&JsValue::Undefined)),
    }
}

fn replace_content(message: &JsValue, content: Vec<JsValue>) -> JsValue {
    let Some(source) = message.as_object() else {
        return message.clone();
    };
    let mut object = source.clone();
    object.insert("content".into(), JsValue::Array(content));
    JsValue::Object(object)
}

fn to_reference_block(record: &ReplacementRecord) -> JsValue {
    let mut object = JsObject::new();
    object.insert("type".into(), "tool_result_reference".into());
    object.insert(
        "toolCallId".into(),
        JsValue::String(record.tool_call_id.clone()),
    );
    if let Some(is_error) = record.is_error {
        object.insert("isError".into(), JsValue::Bool(is_error));
    }
    object.insert("path".into(), JsValue::String(record.path.clone()));
    object.insert(
        "readFilePath".into(),
        JsValue::String(record.read_file_path.clone()),
    );
    object.insert(
        "originalBytes".into(),
        JsValue::Number(record.original_bytes as f64),
    );
    object.insert("preview".into(), JsValue::String(record.preview.clone()));
    object.insert(
        "hasMore".into(),
        JsValue::Bool(utf8_len(&record.preview) < record.original_bytes),
    );
    object.insert("mimeType".into(), JsValue::String(record.mime_type.clone()));
    object.insert("reason".into(), "tool_result_too_large".into());
    JsValue::Object(object)
}

fn looks_like_json(value: &JsString) -> bool {
    let text = String::from_utf16_lossy(value.units());
    let trimmed = crate::metadata::js_trim_start(&text);
    trimmed.starts_with('{') || trimmed.starts_with('[')
}

/// TS `scopedToolResultKey`: `undefined` means unscoped, but `null` reaches
/// `safePathPart` and throws on `.trim()`. Non-strings fail on `.trim` too.
fn scoped_tool_result_key(
    tool_call_id: &JsString,
    turn_id: Option<&JsValue>,
) -> Result<JsString, ToolBudgetError> {
    let safe_tool_call_id = {
        let value = safe_path_part(&String::from_utf16_lossy(tool_call_id.units()));
        if value.is_empty() {
            "tool-call".to_owned()
        } else {
            value
        }
    };
    let safe_turn_id = match turn_id {
        None | Some(JsValue::Undefined) => None,
        Some(value) => {
            let value = safe_path_part_value(value)?;
            (!value.is_empty()).then_some(value)
        }
    };
    Ok(match safe_turn_id {
        Some(turn) => JsString::from(format!("{turn}-{safe_tool_call_id}")),
        None => JsString::from(safe_tool_call_id),
    })
}

fn safe_path_part(value: &str) -> String {
    let trimmed = js_trim(value);
    let mut output = String::new();
    let mut replacing = false;
    for character in trimmed.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-') {
            output.push(character);
            replacing = false;
        } else if !replacing {
            output.push('-');
            replacing = true;
        }
    }
    output.trim_matches('-').to_owned()
}

fn utf8_bytes(value: &JsString) -> Vec<u8> {
    String::from_utf16_lossy(value.units()).into_bytes()
}

fn utf8_len(value: &JsString) -> usize {
    utf8_bytes(value).len()
}

fn truncate_to_bytes(value: &JsString, max_bytes: usize) -> JsString {
    let bytes = utf8_bytes(value);
    if bytes.len() <= max_bytes {
        return value.clone();
    }
    let mut end = bytes.len().min(max_bytes);
    while end > 0 && bytes[end] & 0b1100_0000 == 0b1000_0000 {
        end -= 1;
    }
    JsString::from(String::from_utf8_lossy(&bytes[..end]).as_ref())
}

fn head_tail_preview(value: &JsString, budget_bytes: usize) -> JsString {
    let total_bytes = utf8_len(value);
    if total_bytes <= budget_bytes {
        return value.clone();
    }
    let half_budget = (budget_bytes / 2).saturating_sub(20);
    if half_budget == 0 {
        return truncate_to_bytes(value, budget_bytes);
    }
    let head = truncate_to_bytes(value, half_budget);
    let tail_start = value.len_utf16() as i64 - (half_budget as i64 * 2);
    let tail = if tail_start > 0 {
        JsString::from_utf16(value.units()[tail_start as usize..].to_vec())
    } else {
        JsString::from("")
    };
    // TS computes this with JS numbers, so overlapping head/tail slices can
    // produce a negative omitted count (Node prints `[-500 bytes omitted]`).
    let omitted = total_bytes as i64 - utf8_len(&head) as i64 - utf8_len(&tail) as i64;
    let mut units = head.units().to_vec();
    units.extend(format!("\n\n... [{omitted} bytes omitted] ...\n\n").encode_utf16());
    units.extend_from_slice(tail.units());
    JsString::from_utf16(units)
}

fn estimate_tokens(text: &JsString, byte_length: usize) -> Result<usize, ToolBudgetError> {
    if byte_length <= EXACT_TOKEN_COUNT_MAX_BYTES {
        return count_tokens(text);
    }
    let samples = sample_text_for_token_estimate(text);
    let mut sampled_bytes = 0usize;
    let mut sampled_tokens = 0usize;
    for sample in &samples {
        sampled_bytes += utf8_len(sample);
        sampled_tokens += count_tokens(sample)?;
    }
    if sampled_bytes == 0 {
        return Ok(0);
    }
    Ok(((sampled_tokens as f64 / sampled_bytes as f64) * byte_length as f64).ceil() as usize)
}

fn sample_text_for_token_estimate(text: &JsString) -> Vec<JsString> {
    let length = text.len_utf16();
    if length <= TOKEN_ESTIMATE_SAMPLE_BYTES * 3 {
        return vec![text.clone()];
    }
    let middle_start = (length - TOKEN_ESTIMATE_SAMPLE_BYTES) / 2;
    vec![
        unit_slice(text, 0, TOKEN_ESTIMATE_SAMPLE_BYTES),
        unit_slice(
            text,
            middle_start,
            middle_start + TOKEN_ESTIMATE_SAMPLE_BYTES,
        ),
        unit_slice(text, length - TOKEN_ESTIMATE_SAMPLE_BYTES, length),
    ]
}

fn unit_slice(value: &JsString, start: usize, end: usize) -> JsString {
    let units = value.units();
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    JsString::from_utf16(units[start..end].to_vec())
}

fn count_tokens(text: &JsString) -> Result<usize, ToolBudgetError> {
    pilotdeck_tokenizer::count_tokens(text)
        .map_err(|error| ToolBudgetError::new(format!("{error:?}")))
}

fn media_preview(
    media_type: &str,
    mime_type: &JsValue,
    original_bytes: f64,
    block: &JsValue,
) -> JsString {
    let mime_type = crate::js_value::display(mime_type).unwrap_or_default();
    let size = format!("{} bytes", number_string(original_bytes));
    if media_type == "pdf" {
        let pages = block
            .get("pages")
            .filter(|value| truthy(value))
            .map(|value| {
                format!(
                    ", {} pages",
                    crate::js_value::display(value).unwrap_or_default()
                )
            })
            .unwrap_or_default();
        JsString::from(format!(
            "[PDF omitted from memory: {mime_type}, {size}{pages}]"
        ))
    } else {
        JsString::from(format!(
            "[{media_type} omitted from memory: {mime_type}, {size}]"
        ))
    }
}

fn extension_for_media(media_type: &str, mime_type: &JsValue) -> String {
    if media_type == "pdf" {
        return "pdf.b64".into();
    }
    let mime_type = mime_type
        .as_string()
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default();
    let sub_type = mime_type.split('/').nth(1).unwrap_or_default();
    let filtered: String = sub_type
        .to_lowercase()
        .chars()
        .filter(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '+' | '-')
        })
        .collect();
    if filtered.is_empty() {
        format!("{media_type}.b64")
    } else {
        format!("{filtered}.b64")
    }
}

fn hash_string(value: &JsString) -> String {
    let mut hash: u32 = 2_166_136_261;
    for unit in value.units() {
        hash ^= u32::from(*unit);
        hash = hash.wrapping_mul(16_777_619);
    }
    format!("{hash:x}")
}

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}

fn os_path(value: &JsString) -> PathBuf {
    PathBuf::from(String::from_utf16_lossy(value.units()))
}

fn within(root: &JsString, candidate: &JsString) -> bool {
    let relative = relative(root, candidate);
    let text = String::from_utf16_lossy(relative.units());
    text.is_empty() || (!text.starts_with("..") && !text.starts_with('/'))
}

fn permission_denied(syscall: &str, path: &JsString) -> ToolBudgetError {
    io_error(
        io::Error::from_raw_os_error(libc::EACCES),
        syscall,
        Some(&path_text(path)),
    )
}

fn path_text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

fn io_error(error: io::Error, syscall: &str, path: Option<&str>) -> ToolBudgetError {
    let message = crate::node_io::diagnostic(&error, syscall, path)
        .map(|(_, message)| message)
        .unwrap_or_else(|| error.to_string());
    ToolBudgetError::new(message)
}

/// Create every missing directory with Node's recursive-mkdir mode (`0700`).
fn create_directories(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Confine every spill write to `root` and reject symlinked components below it.
fn ensure_directory(
    root: &JsString,
    target: &JsString,
) -> Result<(PathBuf, PathBuf), ToolBudgetError> {
    if !within(root, target) {
        return Err(permission_denied("mkdir", target));
    }
    let root_path = os_path(root);
    let target_path = os_path(target);
    match fs::symlink_metadata(&root_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(permission_denied("mkdir", root));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(io_error(
                io::Error::from_raw_os_error(libc::EEXIST),
                "mkdir",
                Some(&path_text(root)),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_directories(&root_path)
                .map_err(|error| io_error(error, "mkdir", Some(&path_text(root))))?;
        }
        Err(error) => {
            return Err(io_error(error, "mkdir", Some(&path_text(root))));
        }
    }
    let canonical_root = fs::canonicalize(&root_path)
        .map_err(|error| io_error(error, "mkdir", Some(&path_text(root))))?;
    let Ok(relative_target) = target_path.strip_prefix(&root_path) else {
        return Err(permission_denied("mkdir", target));
    };
    let mut current = root_path.clone();
    for component in relative_target.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(permission_denied("mkdir", target));
                }
                if !metadata.is_dir() {
                    let errno = if current == target_path {
                        libc::EEXIST
                    } else {
                        libc::ENOTDIR
                    };
                    return Err(io_error(
                        io::Error::from_raw_os_error(errno),
                        "mkdir",
                        Some(&path_text(target)),
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_directories(&current)
                    .map_err(|error| io_error(error, "mkdir", Some(&path_text(target))))?;
            }
            Err(error) => {
                return Err(io_error(error, "mkdir", Some(&path_text(target))));
            }
        }
    }
    let canonical_target = fs::canonicalize(&target_path)
        .map_err(|error| io_error(error, "mkdir", Some(&path_text(target))))?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err(permission_denied("mkdir", target));
    }
    Ok((canonical_root, canonical_target))
}

// Keep the canonical paths captured by containment validation: resolving again
// after the test hook (or a real replacement) would trust the attacker's link.
fn create_spill_file(checked: &(PathBuf, PathBuf), path: &JsString) -> io::Result<fs::File> {
    #[cfg(all(test, unix))]
    BEFORE_SPILL_OPEN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path);
        }
    });
    #[cfg(unix)]
    let opened = crate::project_files::create_new_beneath(
        &checked.0,
        &checked.1.join(
            os_path(path)
                .file_name()
                .ok_or(io::ErrorKind::InvalidInput)?,
        ),
    );
    // Non-Unix retains the previous path-based exclusive create behavior.
    #[cfg(not(unix))]
    let opened = {
        let _ = checked;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(os_path(path))
    };
    opened
}

fn spill_open_error(
    error: io::Error,
    path: &JsString,
    syscall: &str,
    io_path: &str,
) -> ToolBudgetError {
    #[cfg(unix)]
    if crate::project_files::nofollow_loop(&error) || error.raw_os_error() == Some(libc::ENOTDIR) {
        return permission_denied("mkdir", &dirname(path));
    }
    #[cfg(not(unix))]
    let _ = path;
    io_error(error, syscall, Some(io_path))
}

#[cfg(all(test, unix))]
type BeforeSpillOpen = Box<dyn FnMut(&JsString)>;
#[cfg(all(test, unix))]
thread_local! {
    static BEFORE_SPILL_OPEN: std::cell::RefCell<Option<BeforeSpillOpen>> = const { std::cell::RefCell::new(None) };
}

fn write_text_if_absent(
    root: &JsString,
    path: &JsString,
    content: &JsString,
) -> Result<bool, ToolBudgetError> {
    let parent = dirname(path);
    let checked = ensure_directory(root, &parent)?;
    let bytes = utf8_bytes(content);
    match create_spill_file(&checked, path) {
        Ok(mut file) => {
            file.write_all(&bytes)
                .map_err(|error| io_error(error, "write", Some(&path_text(path))))?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(spill_open_error(error, path, "open", &path_text(path))),
    }
}

fn copy_file_if_absent(
    root: &JsString,
    source: &JsString,
    destination: &JsString,
) -> Result<bool, ToolBudgetError> {
    let checked = ensure_directory(root, &dirname(destination))?;
    let pair = format!("{}' -> '{}", path_text(source), path_text(destination));
    let source_path = os_path(source);
    // `create_new` protects the destination; the source needs the same check
    // because a pre-planted spill-path symlink or hard link would otherwise be
    // read through and copied outside the spill tree. Open with O_NOFOLLOW and
    // fstat the open handle so there is no lstat->read window and no second
    // directory entry to the inode (nlink > 1 is a hard link). O_NONBLOCK keeps
    // a pre-planted FIFO from blocking the open before the type check.
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(&source_path) {
        Ok(file) => file,
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(permission_denied("copyfile", source));
        }
        Err(error) => return Err(io_error(error, "copyfile", Some(&pair))),
    };
    let metadata = file
        .metadata()
        .map_err(|error| io_error(error, "copyfile", Some(&pair)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(permission_denied("copyfile", source));
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_file() {
        return Err(permission_denied("copyfile", source));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| io_error(error, "copyfile", Some(&pair)))?;
    match create_spill_file(&checked, destination) {
        Ok(mut file) => {
            file.write_all(&bytes)
                .map_err(|error| io_error(error, "copyfile", Some(&pair)))?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(spill_open_error(error, destination, "copyfile", &pair)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    struct ResetSpillHook;
    #[cfg(unix)]
    impl Drop for ResetSpillHook {
        fn drop(&mut self) {
            BEFORE_SPILL_OPEN.with(|hook| *hook.borrow_mut() = None);
        }
    }

    #[cfg(unix)]
    fn raced_destination(copy: bool) {
        use crate::test_scratch::Scratch;
        use std::{cell::Cell, os::unix::fs::symlink, rc::Rc};
        let scratch = Scratch::new("spill-toctou");
        let outside = Scratch::new("spill-toctou-outside");
        let root_path = scratch.path().join("spill");
        let intermediate = root_path.join("middle");
        let parent = intermediate.join("leaf");
        fs::create_dir_all(&parent).unwrap();
        fs::create_dir(outside.path().join("leaf")).unwrap();
        let source = root_path.join("source.txt");
        fs::write(&source, b"payload").unwrap();
        let root = JsString::from(root_path.to_str().unwrap());
        let path = JsString::from(parent.join("payload.txt").to_str().unwrap());
        let fired = Rc::new(Cell::new(false));
        let fired_hook = fired.clone();
        let target = path.clone();
        let outside_path = outside.path().to_owned();
        BEFORE_SPILL_OPEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |path| {
                if path != &target {
                    return;
                }
                assert!(!fired_hook.replace(true), "hook must fire exactly once");
                fs::rename(&intermediate, intermediate.with_extension("saved")).unwrap();
                symlink(&outside_path, &intermediate).unwrap();
            }));
        });
        let _reset = ResetSpillHook;
        write_text_if_absent(
            &root,
            &JsString::from(root_path.join("unrelated.txt").to_str().unwrap()),
            &JsString::from("unrelated"),
        )
        .unwrap();
        assert!(!fired.get(), "hook must ignore unrelated target paths");
        let result = if copy {
            copy_file_if_absent(&root, &JsString::from(source.to_str().unwrap()), &path)
        } else {
            write_text_if_absent(&root, &path, &JsString::from("payload"))
        };
        assert!(fired.get(), "target-filtered hook did not run");
        assert!(
            !outside.path().join("leaf/payload.txt").exists(),
            "raced open wrote outside the spill root"
        );
        assert_eq!(
            fs::read_dir(outside.path().join("leaf")).unwrap().count(),
            0
        );
        // The same symlink presented before containment must have exactly the
        // same public diagnostic as the replacement between check and open.
        let preexisting_error = ensure_directory(&root, &dirname(&path)).unwrap_err();
        assert_eq!(result.unwrap_err().message, preexisting_error.message);
    }

    #[cfg(unix)]
    #[test]
    fn spill_write_rejects_directory_swap_after_check() {
        raced_destination(false);
    }

    #[cfg(unix)]
    #[test]
    fn spill_alias_rejects_directory_swap_after_check() {
        raced_destination(true);
    }

    #[test]
    fn byte_and_preview_helpers_follow_node() {
        let lone = JsString::from_utf16(vec![0xd83d]);
        let pair = JsString::from_utf16(vec![0xd83d, 0xde00]);
        assert_eq!(utf8_len(&lone), 3);
        assert_eq!(utf8_len(&pair), 4);
        assert_eq!(
            String::from_utf16_lossy(truncate_to_bytes(&pair, 3).units()),
            ""
        );
        assert_eq!(
            safe_path_part("  ../a b\u{4e2d}?  "),
            "..-a-b-".trim_matches('-').to_owned()
        );
    }

    #[test]
    fn key_and_hash_keep_utf16_code_units() {
        let turn = JsValue::String(JsString::from("turn 2"));
        assert_eq!(
            scoped_tool_result_key(&JsString::from("call/1"), Some(&turn)).unwrap(),
            JsString::from("turn-2-call-1")
        );
        assert_eq!(
            scoped_tool_result_key(&JsString::from("call/1"), None).unwrap(),
            JsString::from("call-1")
        );
        assert_eq!(
            scoped_tool_result_key(&JsString::from("call/1"), Some(&JsValue::Null))
                .unwrap_err()
                .message,
            "Cannot read properties of null (reading 'trim')"
        );
        assert_eq!(
            scoped_tool_result_key(&JsString::from("call/1"), Some(&JsValue::Number(1.0)))
                .unwrap_err()
                .message,
            "value.trim is not a function"
        );
        assert_eq!(
            tool_call_id_string(None).unwrap_err().message,
            "Cannot read properties of undefined (reading 'trim')"
        );
        assert_eq!(
            tool_call_id_string(Some(&JsValue::Number(1.0)))
                .unwrap_err()
                .message,
            "value.trim is not a function"
        );
        assert_eq!(
            safe_path_part_value(&JsValue::Null).unwrap_err().message,
            "Cannot read properties of null (reading 'trim')"
        );
        assert_eq!(safe_path_part("///"), "");
        assert_eq!(hash_string(&JsString::from("abc")), "1a47e90b");
        assert_eq!(hash_string(&JsString::from("😀")), "cb31c4b8");
        assert_eq!(hash_string(&JsString::from("中文")), "5132b5ad");
        assert_eq!(hash_string(&JsString::from("a😀b")), "8fca8501");
    }
}
