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
    path::PathBuf,
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
    state: std::sync::Arc<Mutex<State>>,
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
            state: std::sync::Arc::new(Mutex::new(State {
                replacements: HashMap::new(),
                next_read_file_alias_index: 1,
            })),
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
        let is_error = block.get("isError").and_then(JsValue::as_bool);
        let replacement_name = String::from_utf16_lossy(replacement_key.units());
        let path = join(
            &self.tool_results_dir,
            &JsString::from(format!("{replacement_name}.{extension}")),
        );
        // #637 fix-r1 (P0-1): the whole remainder — spill write, alias copy,
        // preview/record build and cache registration — is ONE blocking task
        // behind a single await, so the caller observes either the fully
        // persisted state or nothing at all. A spawn_blocking task is never
        // aborted once spawned, so dropping this future at the await cannot
        // strand a written spill without its alias and cached record: the
        // sequence always runs to completion, exactly like TS where an
        // abandoned async function still executes every step (measured:
        // `_tmp/20261025-issue-637/fix-r1/ts-abandoned-async.log`); only the
        // caller stops observing the result. Callers can still drop between
        // two tool-result blocks — those blocks are simply not spilled yet,
        // and a retry re-runs them.
        let (refs_dir, workspace_root) = self.read_file_alias_location();
        #[cfg(unix)]
        let spill_root = workspace_root.clone();
        #[cfg(not(unix))]
        let spill_root = self.tool_results_dir.clone();
        let state = self.state.clone();
        let preview_bytes = self.preview_bytes;
        let record = tokio::task::spawn_blocking(move || {
            write_text_if_absent(&spill_root, &path, &flat)?;
            let read_file_path =
                create_read_file_alias_in(&state, &refs_dir, &workspace_root, &path, extension)?;
            let preview = head_tail_preview(&flat, preview_bytes);
            let record = ReplacementRecord {
                tool_call_id,
                is_error,
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
            state
                .lock()
                .unwrap()
                .replacements
                .insert(replacement_key, record.clone());
            #[cfg(all(test, unix))]
            fire_spill_hook(&AFTER_SPILL_REGISTER, &record.path);
            Ok(record)
        })
        .await
        .map_err(|error| ToolBudgetError::new(error.to_string()))??;
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

    /// Test-only standalone alias copy with the same offload shape: production
    /// aliases are created inside the combined text-spill blocking task (the
    /// single-await rule in `maybe_replace_text_tool_result`), so only the
    /// TOCTOU tests exercise this entry point directly.
    #[cfg(all(test, unix))]
    async fn create_read_file_alias(
        &self,
        source: &JsString,
        extension: &str,
    ) -> Result<JsString, ToolBudgetError> {
        let (refs_dir, workspace_root) = self.read_file_alias_location();
        let state = self.state.clone();
        let source = source.clone();
        let extension = extension.to_owned();
        // Same #637 offload for the alias's full copy (source read plus
        // exclusive destination write); the checked paths stay lexical
        // inside the blocking task so no second resolution is added.
        tokio::task::spawn_blocking(move || {
            create_read_file_alias_in(&state, &refs_dir, &workspace_root, &source, &extension)
        })
        .await
        .map_err(|error| ToolBudgetError::new(error.to_string()))?
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
        path: &JsString,
        content: &JsString,
    ) -> Result<(), ToolBudgetError> {
        #[cfg(unix)]
        let (_, project) = self.read_file_alias_location();
        #[cfg(not(unix))]
        let project = self.tool_results_dir.clone();
        // #637: the spill write (canonicalize, mkdir walk, exclusive open and
        // write) is blocking fs work. TS's spill port awaits `node:fs/promises`
        // (`ToolResultSpillPort.ts`), which runs off the event loop; keep tokio
        // workers equally free, matching `instructions.rs`. Contents, file
        // names and the error mapping are unchanged. A spawn_blocking task is
        // never aborted once spawned: dropping this future leaves the write
        // completed but its media reference unobserved — the same state TS
        // leaves when an abandoned async function finishes its write
        // unobserved. Text-result spills use the combined single-await task
        // in `maybe_replace_text_tool_result` instead, which also registers
        // the replacement before the caller can be dropped.
        let (root, path, content) = (project, path.clone(), content.clone());
        tokio::task::spawn_blocking(move || write_text_if_absent(&root, &path, &content))
            .await
            .map_err(|error| ToolBudgetError::new(error.to_string()))?
            .map(|_| ())
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
        self.persist_artifact(&path, &data).await?;

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
#[cfg(not(unix))]
fn create_directories(path: &std::path::Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Only resolve the trusted project root. Keep every descendant lexical so
/// `.pilotdeck`, `tool-results`, session and refs are all checked with NOFOLLOW.
#[cfg(unix)]
fn ensure_directory(
    root: &JsString,
    target: &JsString,
) -> Result<(PathBuf, PathBuf), ToolBudgetError> {
    if !within(root, target) {
        return Err(permission_denied("mkdir", target));
    }
    let root_path = os_path(root);
    if fs::symlink_metadata(&root_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(permission_denied("mkdir", target));
    }
    let project = fs::canonicalize(&root_path)
        .map_err(|error| io_error(error, "mkdir", Some(&path_text(target))))?;
    let target_path = os_path(target);
    let relative = target_path
        .strip_prefix(&root_path)
        .map_err(|_| permission_denied("mkdir", target))?;
    let directory = project.join(relative);
    #[cfg(test)]
    fire_spill_hook(&BEFORE_SPILL_MKDIR, target);
    crate::project_files::create_directories_beneath(&project, &directory).map_err(|error| {
        // A refused link in the mkdir walk keeps the same public diagnostic as
        // the pre-existing-link case (see `spill_open_error` for the open leg).
        #[cfg(unix)]
        if crate::project_files::nofollow_loop(&error) {
            return permission_denied("mkdir", target);
        }
        io_error(error, "mkdir", Some(&path_text(target)))
    })?;
    Ok((project, directory))
}

// Non-Unix keeps the existing path-based directory creation behavior.
#[cfg(not(unix))]
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

// Keep the checked project root and lexical descendant path: resolving any
// descendant again after a replacement would trust the attacker's link.
fn create_spill_file(checked: &(PathBuf, PathBuf), path: &JsString) -> io::Result<fs::File> {
    #[cfg(all(test, unix))]
    {
        fire_spill_hook(&BEFORE_SPILL_OPEN, path);
        fire_spill_hook(&SLOW_SPILL_OPEN, path);
    }
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

// Test hooks are process-global, not thread-local: since #637 the spill
// write and alias copy run on blocking-pool threads, so the hook must be
// visible from any tokio worker or blocking thread. Hook tests serialize on
// SPILL_HOOK_ORDER (installations would otherwise silently overwrite each
// other) and every installation filters by path so another test's concurrent
// spill write never triggers a foreign replacement. The slow-I/O probe is
// the exception (#637 fix-r1, P1-2): it installs into its own SLOW_SPILL_OPEN
// registry without holding SPILL_HOOK_ORDER, so a whole-run heartbeat test
// never pauses the TOCTOU/panic hook tests — the two families only share the
// fire sites, not the installation lock.
#[cfg(all(test, unix))]
type SpillHook = std::sync::Arc<dyn Fn(&JsString) + Send + Sync>;
/// Mid-write probe (#637 fix-r1, P0-2): fires between the two write legs,
/// after the head bytes reached the disk. A matching hook either injects a
/// descriptor write error or panics.
#[cfg(all(test, unix))]
type SpillWriteHook = std::sync::Arc<dyn Fn(&JsString) -> Option<io::Error> + Send + Sync>;
#[cfg(all(test, unix))]
static BEFORE_SPILL_MKDIR: Mutex<Option<SpillHook>> = Mutex::new(None);
#[cfg(all(test, unix))]
static BEFORE_SPILL_OPEN: Mutex<Option<SpillHook>> = Mutex::new(None);
#[cfg(all(test, unix))]
static MID_SPILL_WRITE: Mutex<Option<SpillWriteHook>> = Mutex::new(None);
#[cfg(all(test, unix))]
static AFTER_SPILL_REGISTER: Mutex<Option<SpillHook>> = Mutex::new(None);
#[cfg(all(test, unix))]
static SLOW_SPILL_OPEN: Mutex<Option<SpillHook>> = Mutex::new(None);
#[cfg(all(test, unix))]
static SPILL_HOOK_ORDER: Mutex<()> = Mutex::new(());

#[cfg(all(test, unix))]
fn fire_spill_hook(registry: &Mutex<Option<SpillHook>>, path: &JsString) {
    // The hook call runs outside the registry lock, so two concurrent spills
    // can both be inside a hook (e.g. the slow-I/O probe's stall) without
    // serializing each other on the lock. Only installation is serialized;
    // see the registry comment above for the order-lock split.
    let Some(hook) = registry.lock().unwrap().clone() else {
        return;
    };
    hook(path);
}

/// #637 fix-r1 (P0-2) write-midpoint probe; see `SpillWriteHook`.
#[cfg(all(test, unix))]
fn mid_spill_write_probe(path: &JsString) -> Option<io::Error> {
    // Same clone-outside-lock rule as `fire_spill_hook`.
    let hook = MID_SPILL_WRITE.lock().unwrap().clone()?;
    hook(path)
}

/// #637 test probe: controlled slow I/O on the real spill path. Every spill
/// open (spill write and alias copy destination) under `root` first blocks
/// for `delay` inside the original I/O path, then the real exclusive open and
/// write still run — the delay stands in for a slow filesystem device, it
/// does not replace the real calls. `fires()` counts the stalled opens.
#[cfg(all(test, unix))]
pub(crate) struct SlowSpillIo {
    fires: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(all(test, unix))]
pub(crate) fn slow_spill_io_under(
    root: &std::path::Path,
    delay: std::time::Duration,
) -> SlowSpillIo {
    let prefix = String::from(root.to_string_lossy());
    let fires = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stalled = fires.clone();
    let hook: SpillHook = std::sync::Arc::new(move |path: &JsString| {
        if !String::from_utf16_lossy(path.units()).starts_with(&prefix) {
            return;
        }
        stalled.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(delay);
    });
    // Own registry, no SPILL_HOOK_ORDER (P1-2): installing the probe must not
    // serialize the TOCTOU/panic hook tests for the probe's whole lifetime.
    SLOW_SPILL_OPEN.lock().unwrap().replace(hook);
    SlowSpillIo { fires }
}

#[cfg(all(test, unix))]
impl SlowSpillIo {
    pub(crate) fn fires(&self) -> usize {
        self.fires.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(all(test, unix))]
impl Drop for SlowSpillIo {
    fn drop(&mut self) {
        *SLOW_SPILL_OPEN.lock().unwrap() = None;
    }
}

/// #637 fix-r1 (P0-2): an exclusively created spill file that is not fully
/// written must not stay behind — the next write on the same path swallows
/// the exclusive-open EEXIST into `created:false` and would serve the
/// truncated bytes as the spilled result. TS's `writeFile(wx)` keeps the
/// partial file (measured: a real ENOSPC on a 5 MB volume left 512 KB on
/// disk and the retry returned `created:false`,
/// `_tmp/20261025-issue-637/fix-r1/ts-midwrite-failure.log`), so the
/// cleanup is a registered divergence
/// (`specs/gateway-parity/diffs/A7.4-TOOLBUDGET-SPILL-PARTIAL-FILE-001`).
/// The guard also fires when a panic unwinds between the write legs; the
/// cleanup itself is best-effort, and a failed cleanup leaves exactly TS's
/// behavior behind.
struct ExclusiveSpill {
    file: fs::File,
    checked: (PathBuf, PathBuf),
    os_path: PathBuf,
    complete: bool,
}

impl ExclusiveSpill {
    fn new(file: fs::File, checked: &(PathBuf, PathBuf), path: &JsString) -> Self {
        Self {
            file,
            checked: checked.clone(),
            os_path: os_path(path),
            complete: false,
        }
    }

    /// One leg of the spilled payload. Node's `writeFile`/`copyFile` stream
    /// the payload in chunks; two legs write the same bytes with the same
    /// error mapping, and the midpoint probe between them can fault the
    /// write after the head reached the disk.
    fn write_leg(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)
    }

    fn finish(&mut self) {
        self.complete = true;
    }

    /// Remove through the same checked root, so a raced-in link cannot
    /// redirect the cleanup outside the spill tree; on failure the partial
    /// file stays behind exactly like TS.
    #[cfg(unix)]
    fn remove_partial(&self) {
        if let Some(name) = self.os_path.file_name()
            && let Ok(directory) =
                crate::project_files::open_directory_beneath(&self.checked.0, &self.checked.1)
        {
            let _ = crate::project_files::unlink_at(&directory, std::path::Path::new(name));
        }
    }

    #[cfg(not(unix))]
    fn remove_partial(&self) {
        let _ = fs::remove_file(&self.os_path);
    }
}

impl Drop for ExclusiveSpill {
    fn drop(&mut self) {
        if !self.complete {
            self.remove_partial();
        }
    }
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
        Ok(file) => {
            let mut spill = ExclusiveSpill::new(file, &checked, path);
            // The TS spill writes `writeFile(path, content, { flag: "wx" })`:
            // a descriptor write error has no path, unlike an open error.
            let (head, tail) = bytes.split_at(bytes.len() / 2);
            spill
                .write_leg(head)
                .map_err(|error| io_error(error, "write", None))?;
            #[cfg(all(test, unix))]
            if let Some(error) = mid_spill_write_probe(path) {
                return Err(io_error(error, "write", None));
            }
            spill
                .write_leg(tail)
                .map_err(|error| io_error(error, "write", None))?;
            spill.finish();
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
        Ok(file) => {
            let mut spill = ExclusiveSpill::new(file, &checked, destination);
            let (head, tail) = bytes.split_at(bytes.len() / 2);
            spill
                .write_leg(head)
                .map_err(|error| io_error(error, "copyfile", Some(&pair)))?;
            #[cfg(all(test, unix))]
            if let Some(error) = mid_spill_write_probe(destination) {
                return Err(io_error(error, "copyfile", Some(&pair)));
            }
            spill
                .write_leg(tail)
                .map_err(|error| io_error(error, "copyfile", Some(&pair)))?;
            spill.finish();
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(spill_open_error(error, destination, "copyfile", &pair)),
    }
}

/// `ToolResultBudget.createReadFileAlias`'s loop, runnable inside a blocking
/// task: an existing alias destination is not an error (TS's
/// `copyFile(COPYFILE_EXCL)` EEXIST is caught by `copyFileIfAbsent` and the
/// loop advances to the next index); the index counter and the copy share one
/// state lock so concurrent spills never reuse an index.
fn create_read_file_alias_in(
    state: &Mutex<State>,
    refs_dir: &JsString,
    workspace_root: &JsString,
    source: &JsString,
    extension: &str,
) -> Result<JsString, ToolBudgetError> {
    loop {
        let index = {
            let mut state = state.lock().unwrap();
            let index = state.next_read_file_alias_index.max(1);
            state.next_read_file_alias_index = index + 1;
            index
        };
        let alias = join(
            refs_dir,
            &JsString::from(format!("result-{index:04}.{extension}")),
        );
        #[cfg(unix)]
        let root = workspace_root.clone();
        #[cfg(not(unix))]
        let root = refs_dir.clone();
        let copied = copy_file_if_absent(&root, source, &alias)?;
        if copied {
            return Ok(relative(workspace_root, &alias));
        }
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
            *BEFORE_SPILL_OPEN.lock().unwrap() = None;
            *BEFORE_SPILL_MKDIR.lock().unwrap() = None;
            *MID_SPILL_WRITE.lock().unwrap() = None;
            *AFTER_SPILL_REGISTER.lock().unwrap() = None;
        }
    }

    #[cfg(unix)]
    fn raced_destination(copy: bool) {
        use crate::test_scratch::Scratch;
        use std::{
            os::unix::fs::symlink,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };
        let _order = SPILL_HOOK_ORDER.lock().unwrap();
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
        let fired = Arc::new(AtomicBool::new(false));
        let fired_hook = fired.clone();
        let target = path.clone();
        let outside_path = outside.path().to_owned();
        BEFORE_SPILL_OPEN
            .lock()
            .unwrap()
            .replace(std::sync::Arc::new(move |path: &JsString| {
                if path != &target {
                    return;
                }
                assert!(
                    !fired_hook.swap(true, Ordering::SeqCst),
                    "hook must fire exactly once"
                );
                fs::rename(&intermediate, intermediate.with_extension("saved")).unwrap();
                symlink(&outside_path, &intermediate).unwrap();
            }));
        let _reset = ResetSpillHook;
        write_text_if_absent(
            &root,
            &JsString::from(root_path.join("unrelated.txt").to_str().unwrap()),
            &JsString::from("unrelated"),
        )
        .unwrap();
        assert!(
            !fired.load(Ordering::SeqCst),
            "hook must ignore unrelated target paths"
        );
        let result = if copy {
            copy_file_if_absent(&root, &JsString::from(source.to_str().unwrap()), &path)
        } else {
            write_text_if_absent(&root, &path, &JsString::from("payload"))
        };
        assert!(
            fired.load(Ordering::SeqCst),
            "target-filtered hook did not run"
        );
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

    /// Child half of `spill_write_reports_the_write_stage_like_node`.
    #[cfg(unix)]
    #[test]
    #[ignore = "spawned by spill_write_reports_the_write_stage_like_node"]
    fn spill_child_over_file_size_limit() {
        let Ok(path) = std::env::var(crate::test_write_stage::CHILD_PATH_ENV) else {
            panic!(
                "{} not set; spawn via spill_write_reports_the_write_stage_like_node",
                crate::test_write_stage::CHILD_PATH_ENV
            );
        };
        crate::test_write_stage::ignore_sigxfsz_like_gateway();
        let dir = std::path::Path::new(&path).parent().unwrap();
        let root = JsString::from(dir.to_str().unwrap());
        let error = write_text_if_absent(
            &root,
            &JsString::from(path.as_str()),
            &JsString::from("a".repeat(4096)),
        )
        .expect_err("a spill write past RLIMIT_FSIZE must fail");
        println!("WRITE_ERROR={}", error.message);
    }

    /// The spill write is TS's `writeFile(path, content, { flag: "wx" })`:
    /// a descriptor write error must read like Node's `EFBIG: file too
    /// large, write` with no path (node v22.22.0,
    /// `_tmp/20261025-issue-656/node-probe/write-stages.out`), not the open
    /// label with the spill path.
    #[cfg(unix)]
    #[test]
    fn spill_write_reports_the_write_stage_like_node() {
        let scratch = crate::test_scratch::Scratch::new("spill-write-stage");
        let target = scratch.path().join("spill.txt");
        let line = crate::test_write_stage::child_write_error(
            "runtime::context::tool_budget::tests::spill_child_over_file_size_limit",
            1,
            &target,
        );
        assert_eq!(line, "EFBIG: file too large, write");
    }

    #[cfg(unix)]
    #[test]
    fn spill_alias_rejects_directory_swap_after_check() {
        raced_destination(true);
    }

    /// #637 error path: a panic inside the offloaded blocking write surfaces
    /// as the caller's `ToolBudgetError` (the JoinError mapping), not as an
    /// aborting worker, and the diagnostic channel stays the one
    /// `apply_tool_results` already uses.
    #[cfg(unix)]
    #[tokio::test]
    // The order guard must stay held for the whole awaited spill: it is what
    // keeps this test's process-global hook installation exclusive.
    #[allow(clippy::await_holding_lock)]
    async fn spill_blocking_panic_becomes_persistence_error() {
        use crate::test_scratch::Scratch;
        let _order = SPILL_HOOK_ORDER.lock().unwrap();
        let scratch = Scratch::new("spill-panic");
        let prefix = scratch.path().to_string_lossy().into_owned();
        BEFORE_SPILL_OPEN
            .lock()
            .unwrap()
            .replace(std::sync::Arc::new(move |path: &JsString| {
                if String::from_utf16_lossy(path.units()).starts_with(&prefix) {
                    panic!("spill device broke");
                }
            }));
        let _reset = ResetSpillHook;
        let budget = ToolResultBudget::new(ToolResultBudgetOptions {
            tool_results_dir: scratch.path().join("spill"),
            max_result_size_chars: Some(1),
            max_result_size_tokens: Some(100),
            preview_bytes: None,
        });
        let error = budget
            .persist_artifact(
                &JsString::from(scratch.path().join("spill/call.txt").to_str().unwrap()),
                &JsString::from("payload"),
            )
            .await
            .unwrap_err();
        assert!(error.message.contains("panicked"), "{}", error.message);
        assert!(
            !scratch.path().join("spill/call.txt").exists(),
            "a panicked write must not report success"
        );
    }

    /// #637 fix-r1 (P0-2): the panic above fires before the open, so "file
    /// does not exist" is trivially true there. Here the panic lands between
    /// the two write legs — the head bytes are already on disk — and the
    /// partial file must still not survive as a valid-looking spill.
    #[cfg(unix)]
    #[tokio::test]
    // Same order-guard-across-await pattern as the panic test above.
    #[allow(clippy::await_holding_lock)]
    async fn spill_write_panic_midway_removes_partial_file() {
        use crate::test_scratch::Scratch;
        let _order = SPILL_HOOK_ORDER.lock().unwrap();
        let scratch = Scratch::new("spill-panic-mid");
        let prefix = scratch.path().to_string_lossy().into_owned();
        MID_SPILL_WRITE
            .lock()
            .unwrap()
            .replace(std::sync::Arc::new(move |path: &JsString| {
                if String::from_utf16_lossy(path.units()).starts_with(&prefix) {
                    panic!("spill device broke mid-write");
                }
                None
            }));
        let _reset = ResetSpillHook;
        let budget = ToolResultBudget::new(ToolResultBudgetOptions {
            tool_results_dir: scratch.path().join("spill"),
            max_result_size_chars: Some(1),
            max_result_size_tokens: Some(100),
            preview_bytes: None,
        });
        let error = budget
            .persist_artifact(
                &JsString::from(scratch.path().join("spill/call.txt").to_str().unwrap()),
                &JsString::from("0123456789"),
            )
            .await
            .unwrap_err();
        assert!(error.message.contains("panicked"), "{}", error.message);
        assert!(
            !scratch.path().join("spill/call.txt").exists(),
            "a half-written spill file must not survive a mid-write panic"
        );
    }

    /// #637 fix-r1 (P0-2): a descriptor write failure midway leaves no
    /// half-written spill behind, and the retry writes the full content
    /// instead of swallowing the truncated file as `created:false`. TS keeps
    /// the partial file and its retry returns `created:false` (measured:
    /// `_tmp/20261025-issue-637/fix-r1/ts-midwrite-failure.log`); the
    /// divergence is registered in
    /// `specs/gateway-parity/diffs/A7.4-TOOLBUDGET-SPILL-PARTIAL-FILE-001`.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn spill_write_failure_midway_cleans_up_and_retries() {
        use crate::test_scratch::Scratch;
        use std::sync::atomic::{AtomicBool, Ordering};
        let _order = SPILL_HOOK_ORDER.lock().unwrap();
        let scratch = Scratch::new("spill-write-fail");
        let prefix = scratch.path().to_string_lossy().into_owned();
        let fired = std::sync::Arc::new(AtomicBool::new(false));
        let injected = fired.clone();
        MID_SPILL_WRITE
            .lock()
            .unwrap()
            .replace(std::sync::Arc::new(move |path: &JsString| {
                if String::from_utf16_lossy(path.units()).starts_with(&prefix)
                    && !injected.swap(true, Ordering::SeqCst)
                {
                    // The same descriptor error the TS probe hit on a full
                    // 5 MB volume; node prints `ENOSPC: no space left on
                    // device, write` without a path for a write failure.
                    return Some(io::Error::from_raw_os_error(libc::ENOSPC));
                }
                None
            }));
        let _reset = ResetSpillHook;
        let budget = ToolResultBudget::new(ToolResultBudgetOptions {
            tool_results_dir: scratch.path().join("spill"),
            max_result_size_chars: Some(1),
            max_result_size_tokens: Some(100),
            preview_bytes: None,
        });
        let path = JsString::from(scratch.path().join("spill/call.txt").to_str().unwrap());
        let error = budget
            .persist_artifact(&path, &JsString::from("0123456789"))
            .await
            .unwrap_err();
        assert_eq!(
            error.message, "ENOSPC: no space left on device, write",
            "the mid-write failure keeps TS's descriptor-error wording"
        );
        assert!(
            !scratch.path().join("spill/call.txt").exists(),
            "a half-written spill file must not survive a mid-write failure"
        );
        // The next write of the same target starts from a clean exclusive
        // create instead of adopting the truncated file.
        budget
            .persist_artifact(&path, &JsString::from("0123456789"))
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(scratch.path().join("spill/call.txt")).unwrap(),
            "0123456789"
        );
    }

    /// #637 fix-r1 (P0-1): dropping the caller's future at the combined spill
    /// task's single await must leave exactly TS's abandoned-async state: the
    /// blocking task still runs every step (spill write, alias copy, cache
    /// registration), so a retry hits the replacement cache and returns the
    /// same reference — no orphaned spill, no EEXIST, no second alias
    /// (measured TS: `_tmp/20261025-issue-637/fix-r1/ts-abandoned-async.log`).
    #[cfg(unix)]
    #[tokio::test]
    // The order guard must stay held across the awaits: it keeps this test's
    // process-global hook installations exclusive (same pattern as above).
    #[allow(clippy::await_holding_lock)]
    async fn spill_text_result_completes_after_caller_drop() {
        use crate::test_scratch::Scratch;
        use std::{
            sync::{
                Arc, Condvar, Mutex as StdMutex,
                atomic::{AtomicBool, Ordering},
            },
            time::Duration,
        };

        let _order = SPILL_HOOK_ORDER.lock().unwrap();
        let scratch = Scratch::new("spill-drop");
        let project = scratch.path().join("proj/.pilotdeck/tool-results");
        let spill_file = project.join("session/call-1.txt");
        let alias_file = project.join("refs/result-0001.txt");
        let prefix = scratch.path().to_string_lossy().into_owned();
        // The unix spill root is the project directory above `tool-results`
        // (`read_file_alias_location`), and like a real project it must
        // already exist when the spill starts.
        fs::create_dir_all(scratch.path().join("proj")).unwrap();

        // The first spill open parks inside the blocking task; the alias's
        // open passes through so the whole sequence can finish after release.
        let parked = Arc::new(AtomicBool::new(false));
        let released = Arc::new((StdMutex::new(false), Condvar::new()));
        let gate_parked = parked.clone();
        let gate_release = released.clone();
        BEFORE_SPILL_OPEN
            .lock()
            .unwrap()
            .replace(std::sync::Arc::new(move |path: &JsString| {
                if !String::from_utf16_lossy(path.units()).starts_with(&prefix) {
                    return;
                }
                if gate_parked.swap(true, Ordering::SeqCst) {
                    return;
                }
                let (lock, cvar) = &*gate_release;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = cvar.wait(open).unwrap();
                }
            }));
        // Completion signal: fires after the replacement-cache insert, so
        // the retry below never races the abandoned task's registration.
        let registered = Arc::new(AtomicBool::new(false));
        let signal = registered.clone();
        let register_prefix = scratch.path().to_string_lossy().into_owned();
        AFTER_SPILL_REGISTER
            .lock()
            .unwrap()
            .replace(std::sync::Arc::new(move |path: &JsString| {
                if String::from_utf16_lossy(path.units()).starts_with(&register_prefix) {
                    signal.store(true, Ordering::SeqCst);
                }
            }));
        let _reset = ResetSpillHook;

        let text = "x".repeat(200_001);
        let budget = ToolResultBudget::new(ToolResultBudgetOptions {
            tool_results_dir: project.join("session"),
            max_result_size_chars: Some(1),
            max_result_size_tokens: Some(100),
            preview_bytes: None,
        });
        let message = object([
            ("role", "user".into()),
            (
                "content",
                JsValue::Array(vec![object([
                    ("type", "tool_result".into()),
                    ("toolCallId", "call-1".into()),
                    (
                        "content",
                        JsValue::Array(vec![object([
                            ("type", "text".into()),
                            ("text", JsValue::String(JsString::from(text.clone()))),
                        ])]),
                    ),
                ])]),
            ),
        ]);
        // Park the blocking task inside the gated open, then abandon the
        // caller exactly at its single await: the block scope below drops
        // the suspended future while the write is still parked inside the
        // gate (`tokio::pin!`'s Pin<&mut> itself is not the owner).
        let gate_armed = parked.clone();
        {
            let spilled = budget.apply_to_message(&message, None);
            tokio::pin!(spilled);
            tokio::select! {
                result = &mut spilled => panic!("spill completed before the gate: {result:?}"),
                _ = tokio::time::timeout(Duration::from_secs(10), async {
                    while !gate_armed.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                }) => {}
            }
            assert!(!spill_file.exists(), "the gate fires before the open");
        }
        {
            let (lock, cvar) = &*released;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        let finished = registered.clone();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !finished.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("abandoned spill task must still complete");

        // Disk state: the abandoned run wrote both the spill and the alias.
        assert_eq!(fs::read_to_string(&spill_file).unwrap(), text);
        assert_eq!(fs::read_to_string(&alias_file).unwrap(), text);
        // The retry is served from the registered replacement: same path and
        // alias, and no second alias index is consumed.
        let retried = budget.apply_to_message(&message, None).await.unwrap();
        let JsValue::Array(content) = retried.get("content").unwrap() else {
            panic!("content");
        };
        assert_eq!(
            content[0].get("type"),
            Some(&"tool_result_reference".into())
        );
        assert_eq!(
            content[0]
                .get("path")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
            spill_file.to_str().unwrap()
        );
        assert_eq!(
            content[0]
                .get("readFilePath")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
            ".pilotdeck/tool-results/refs/result-0001.txt"
        );
        let aliases = fs::read_dir(project.join("refs"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("result-"))
            })
            .count();
        assert_eq!(aliases, 1, "the retry must not create a second alias");
    }

    /// #637 fix-r1 (P2-3 对拍): a pre-existing alias destination is skipped for
    /// the next index without surfacing an error — TS's
    /// `copyFile(COPYFILE_EXCL)` EEXIST is caught by `copyFileIfAbsent` and
    /// `createReadFileAlias`'s loop continues (measured:
    /// `_tmp/20261025-issue-637/fix-r1/ts-alias-eexist.log`). This is the
    /// retry state a dropped caller leaves behind when the alias was written
    /// but the reference never observed.
    #[cfg(unix)]
    #[tokio::test]
    async fn spill_alias_eexist_advances_to_next_index_like_ts() {
        use crate::test_scratch::Scratch;
        let scratch = Scratch::new("spill-alias-eexist");
        let project = scratch.path().join("proj/.pilotdeck/tool-results");
        fs::create_dir_all(project.join("refs")).unwrap();
        fs::write(project.join("refs/result-0001.txt"), "older tenant").unwrap();
        let text = "y".repeat(200_001);
        let budget = ToolResultBudget::new(ToolResultBudgetOptions {
            tool_results_dir: project.join("session"),
            max_result_size_chars: Some(1),
            max_result_size_tokens: Some(100),
            preview_bytes: None,
        });
        let message = object([
            ("role", "user".into()),
            (
                "content",
                JsValue::Array(vec![object([
                    ("type", "tool_result".into()),
                    ("toolCallId", "call-1".into()),
                    (
                        "content",
                        JsValue::Array(vec![object([
                            ("type", "text".into()),
                            ("text", JsValue::String(JsString::from(text.clone()))),
                        ])]),
                    ),
                ])]),
            ),
        ]);
        let replaced = budget.apply_to_message(&message, None).await.unwrap();
        let JsValue::Array(content) = replaced.get("content").unwrap() else {
            panic!("content");
        };
        // No error surfaced: the reference points at the next free index and
        // the pre-existing file keeps its own content.
        assert_eq!(
            content[0]
                .get("readFilePath")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
            ".pilotdeck/tool-results/refs/result-0002.txt"
        );
        assert_eq!(
            fs::read_to_string(project.join("refs/result-0001.txt")).unwrap(),
            "older tenant"
        );
        assert_eq!(
            fs::read_to_string(project.join("refs/result-0002.txt")).unwrap(),
            text
        );
    }

    #[cfg(unix)]
    // Same as `spill_blocking_panic_becomes_persistence_error`: the order
    // guard intentionally spans the awaited spill under test.
    #[allow(clippy::await_holding_lock)]
    async fn project_destination(copy: bool, component: &str, race: Option<bool>) {
        use crate::test_scratch::Scratch;
        use std::{
            os::unix::fs::symlink,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };
        let _order = SPILL_HOOK_ORDER.lock().unwrap();
        let scratch = Scratch::new("spill-project");
        let outside = Scratch::new("spill-project-outside");
        let project = scratch.path().join("project");
        let project_text = project.to_string_lossy().into_owned();
        fs::create_dir(&project).unwrap();
        let spill = project.join(".pilotdeck/tool-results/session");
        let refs = project.join(".pilotdeck/tool-results/refs");
        let parent = if copy { &refs } else { &spill };
        let changed = project.join(component);
        // Supply the exact destination shape in the attacker's tree, so a
        // vulnerable open writes a file rather than failing for missing dirs.
        let outside_parent = outside.path().join(parent.strip_prefix(&changed).unwrap());
        fs::create_dir_all(&outside_parent).unwrap();
        let outside_file = outside_parent.join(if copy { "result-0001.txt" } else { "call.txt" });
        fs::create_dir_all(changed.parent().unwrap()).unwrap();
        let source = project.join("source.txt");
        fs::write(&source, b"payload").unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let fired_hook = fired.clone();
        let outside_path = outside.path().to_owned();
        let replace = move |path: &JsString| {
            // The hooks are process-global: ignore other tests' spill writes.
            if !String::from_utf16_lossy(path.units()).starts_with(&project_text) {
                return;
            }
            assert!(
                !fired_hook.swap(true, Ordering::SeqCst),
                "hook must fire exactly once"
            );
            if changed.exists() {
                fs::rename(&changed, changed.with_extension("saved")).unwrap();
            }
            symlink(&outside_path, &changed).unwrap();
        };
        if let Some(before_mkdir) = race {
            if !before_mkdir {
                fs::create_dir_all(parent).unwrap();
            }
            if before_mkdir {
                BEFORE_SPILL_MKDIR
                    .lock()
                    .unwrap()
                    .replace(std::sync::Arc::new(replace));
            } else {
                BEFORE_SPILL_OPEN
                    .lock()
                    .unwrap()
                    .replace(std::sync::Arc::new(replace));
            }
        } else {
            // Preexisting-link case: no hook, the replacement is already in
            // place; the path argument only has to pass this test's filter.
            replace(&JsString::from(parent.to_str().unwrap()));
        }
        let _reset = ResetSpillHook;
        let budget = ToolResultBudget::new(ToolResultBudgetOptions {
            tool_results_dir: spill.clone(),
            max_result_size_chars: Some(1),
            max_result_size_tokens: Some(100),
            preview_bytes: None,
        });
        let result = if copy {
            budget
                .create_read_file_alias(&JsString::from(source.to_str().unwrap()), "txt")
                .await
                .map(|_| ())
        } else {
            budget
                .persist_artifact(
                    &JsString::from(parent.join("call.txt").to_str().unwrap()),
                    &JsString::from("payload"),
                )
                .await
        };
        assert!(fired.load(Ordering::SeqCst), "replacement hook did not run");
        assert!(!outside_file.exists(), "spill wrote outside project");
        let error = result.unwrap_err();
        assert!(error.message.contains("EACCES"), "{}", error.message);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spill_project_rejects_preexisting_upstream_links() {
        for copy in [false, true] {
            for component in [".pilotdeck", ".pilotdeck/tool-results"] {
                project_destination(copy, component, None).await;
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spill_project_rejects_upstream_swap_before_directory_creation() {
        for copy in [false, true] {
            for component in [".pilotdeck", ".pilotdeck/tool-results"] {
                project_destination(copy, component, Some(true)).await;
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spill_project_rejects_upstream_and_root_swap_before_file_open() {
        for copy in [false, true] {
            for component in [
                ".pilotdeck",
                ".pilotdeck/tool-results",
                if copy {
                    ".pilotdeck/tool-results/refs"
                } else {
                    ".pilotdeck/tool-results/session"
                },
            ] {
                project_destination(copy, component, Some(false)).await;
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn spill_project_mkdir_swap_has_no_outside_directory_side_effect() {
        use crate::test_scratch::Scratch;
        use std::{
            os::unix::fs::symlink,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };
        let _order = SPILL_HOOK_ORDER.lock().unwrap();
        let project = Scratch::new("spill-project-mkdir");
        let outside = Scratch::new("spill-project-mkdir-outside");
        let changed = project.path().join(".pilotdeck");
        let project_text = project.path().to_string_lossy().into_owned();
        let external = outside.path().to_owned();
        let fired = Arc::new(AtomicBool::new(false));
        let hook_fired = fired.clone();
        BEFORE_SPILL_MKDIR
            .lock()
            .unwrap()
            .replace(Arc::new(move |path: &JsString| {
                // The hooks are process-global: ignore other tests' spill writes.
                if !String::from_utf16_lossy(path.units()).starts_with(&project_text) {
                    return;
                }
                assert!(
                    !hook_fired.swap(true, Ordering::SeqCst),
                    "hook must fire exactly once"
                );
                symlink(&external, &changed).unwrap();
            }));
        let _reset = ResetSpillHook;
        let result = write_text_if_absent(
            &JsString::from(project.path().to_str().unwrap()),
            &JsString::from(
                project
                    .path()
                    .join(".pilotdeck/tool-results/session/call.txt")
                    .to_str()
                    .unwrap(),
            ),
            &JsString::from("payload"),
        );
        assert!(fired.load(Ordering::SeqCst));
        assert!(
            !outside.path().join("tool-results").exists(),
            "mkdir escaped project"
        );
        assert!(result.unwrap_err().message.contains("EACCES"));
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
