//! `edit_file` (baseline `tool/builtin/editFile.ts`): exact-string replacement,
//! quote normalization, markdown-aware trailing-whitespace trimming and the
//! read-before-write snapshot guard.
use super::{
    ensure_write_snapshot_fresh, field, file_history, message_id, object, read_text_file,
    record_write_snapshot, resolve_write_path, schema, string, text, tool_error, truthy,
    validate_write_snapshot_fresh, write_text_file,
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext, js_space},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub struct EditFileTool {
    fields: JsObject,
}

impl EditFileTool {
    pub fn new() -> Self {
        Self {
            fields: definition(),
        }
    }
}

impl Default for EditFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for EditFileTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}

impl ExecutableTool for EditFileTool {
    fn validate_input(
        &self,
        input: JsValue,
        context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move { validate(&input, &context) })
    }

    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move { execute(&input, &context) })
    }
}

fn definition() -> JsObject {
    object([
        ("name", string("edit_file")),
        ("aliases", JsValue::Array(vec![string("Edit")])),
        (
            "description",
            string(
                "Edit a workspace text file, or an outside-workspace text file after explicit host permission, by replacing an exact string match.\n\nUsage:\n- You may create a new file directly by setting old_string to an empty string.\n- You must read an existing target file with read_file before editing it. This tool will reject edits to existing files that have not been read in this session.\n- old_string must exactly match the file content character-by-character, including indentation. Copy old_string directly from read_file output without adding or removing spaces.\n- Use this tool for targeted changes to an existing file.\n- old_string must appear in the target file unless you are creating a new file with old_string: \"\".\n- If old_string is not unique, either provide a more specific old_string or set replace_all to update every occurrence.\n- Use replace_all when renaming or replacing repeated text across the same file.\n- Paths outside the workspace require explicit user permission before execution.",
            ),
        ),
        ("kind", string("filesystem")),
        (
            "inputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["file_path","old_string","new_string"],"additionalProperties":false,"properties":{"file_path":{"type":"string","description":"Relative or absolute path of the file to edit. Paths outside the workspace require explicit user permission."},"old_string":{"type":"string","description":"The exact substring to find and replace. It must appear in the target file."},"new_string":{"type":"string","description":"The replacement string that will replace old_string."},"replace_all":{"type":"boolean","description":"When true, replace all occurrences of old_string. Defaults to false, which requires old_string to be unique."}}}"#,
            )),
        ),
    ])
}

fn issue(path: &str, message: impl Into<JsString>) -> JsObject {
    object([
        ("path", string(path)),
        ("code", string("invalid_schema")),
        ("message", string(message)),
    ])
}

fn input_string(input: &JsValue, key: &str) -> JsString {
    field(input, key)
        .as_string()
        .cloned()
        .unwrap_or_else(|| JsString::from(""))
}

fn is_notebook(path: &JsString) -> bool {
    super::lower(&super::extname(path)) == ".ipynb"
}

fn validate(input: &JsValue, context: &ToolRunContext) -> Result<Option<Vec<JsObject>>, ToolError> {
    let path = input_string(input, "file_path");
    let old = input_string(input, "old_string");
    let new = input_string(input, "new_string");
    let resolved = match resolve_write_path(&path, context, false, true) {
        Ok(resolved) => resolved,
        Err(error) if error.code == "tool_execution_failed" => return Err(error),
        Err(error) => return Ok(Some(vec![issue("file_path", error.message.clone())])),
    };
    if is_notebook(&resolved.absolute) {
        return Ok(Some(vec![issue(
            "file_path",
            "File is a Jupyter notebook. Use edit_notebook to edit this file.",
        )]));
    }
    if !old.is_empty() && old == new {
        return Ok(Some(vec![issue(
            "new_string",
            "old_string and new_string must differ.",
        )]));
    }
    let exists = match validate_write_snapshot_fresh(context, &resolved.absolute) {
        Ok(exists) => exists,
        Err(error) if is_read_state_error(&error) => {
            return Ok(Some(vec![issue("file_path", error.message.clone())]));
        }
        Err(error) => return Err(error),
    };
    if !exists {
        if old.is_empty() {
            return Ok(None);
        }
        return Ok(Some(vec![issue(
            "file_path",
            format!("File {} does not exist.", text(&path)),
        )]));
    }
    if !old.is_empty() {
        return Ok(None);
    }
    if read_text_file(&resolved.absolute)?.is_empty() {
        return Ok(None);
    }
    Ok(Some(vec![issue(
        "old_string",
        "old_string may be empty only when creating a new file or writing to an empty file.",
    )]))
}

fn is_read_state_error(error: &ToolError) -> bool {
    error.code == "invalid_tool_input"
        && (error
            .message
            .equals_str("File has not been read yet. Read it first before writing to it.")
            || error.message.equals_str(
                "File has changed since the last read. Read it again before writing to it.",
            ))
}

fn execute(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let path = input_string(input, "file_path");
    let old = input_string(input, "old_string");
    let new = input_string(input, "new_string");
    let replace_all = truthy(field(input, "replace_all"));
    let allowed_outside = context
        .fields
        .get("currentPermissionDecision")
        .and_then(|decision| decision.get("type"))
        .and_then(JsValue::as_string)
        .is_some_and(|kind| kind.equals_str("allow"));
    let resolved = resolve_write_path(&path, context, false, allowed_outside)?;
    let freshness = ensure_write_snapshot_fresh(context, &resolved.absolute)?;

    let message = message_id(context);
    if let Some(history) = file_history(context) {
        history
            .track_edit(&text(&resolved.absolute), &message)
            .map_err(|error| tool_error("tool_execution_failed", error.to_string()))?;
    }

    let content = freshness.previous_content.clone().unwrap_or_default();
    let old_text = String::from_utf16_lossy(old.units());
    let new_text = String::from_utf16_lossy(new.units());
    let mut occurrences = 0usize;
    let next_content = if old_text.is_empty() {
        if freshness.exists && !content.is_empty() {
            return Err(tool_error(
                "invalid_tool_input",
                "old_string may be empty only when creating a new file or writing to an empty file.",
            ));
        }
        new_text
    } else {
        let (normalized_old, normalized_new) =
            normalize_edit_input(&text(&resolved.absolute), &old_text, &new_text);
        let actual = find_actual_string(&content, &normalized_old).ok_or_else(|| {
            tool_error(
                "invalid_tool_input",
                format!("String to replace not found in file.\nString: {old_text}"),
            )
        })?;
        occurrences = count_occurrences(&content, &actual);
        if occurrences > 1 && !replace_all {
            return Err(tool_error(
                "invalid_tool_input",
                format!(
                    "Found {occurrences} matches of old_string. Set replace_all to true to replace all occurrences, or provide a more specific old_string."
                ),
            ));
        }
        if replace_all {
            content
                .split(actual.as_str())
                .collect::<Vec<_>>()
                .join(&normalized_new)
        } else {
            let position = content
                .find(actual.as_str())
                .expect("actual string was found above");
            let prefix = &content[..position];
            let suffix = &content[position + actual.len()..];
            format!(
                "{prefix}{}{suffix}",
                crate::js_value::expand_replacement(
                    &normalized_new,
                    actual.as_str(),
                    prefix,
                    suffix
                )
            )
        }
    };

    let write = write_text_file(&resolved.absolute, &next_content)?;
    if let Some(history) = file_history(context) {
        history
            .mark_edit_committed(&text(&resolved.absolute), &message)
            .map_err(|error| tool_error("tool_execution_failed", error.to_string()))?;
    }
    super::invalidate_read_file_state(context, &resolved.absolute);
    record_write_snapshot(
        context,
        &resolved.absolute,
        &next_content,
        write.mtime_ms,
        None,
    );

    let replacements = if old_text.is_empty() {
        0
    } else if replace_all {
        occurrences
    } else {
        1
    };
    let suffix = if replacements > 0 {
        format!(" ({replacements} replacement).")
    } else {
        ".".to_string()
    };
    let success = format!(
        "{} {}{suffix}",
        if write.created { "Created" } else { "Updated" },
        text(&resolved.relative)
    );
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(success.as_str())),
        ]))],
        data: Some(JsValue::Object(object([
            ("filePath", string(resolved.relative.clone())),
            ("replacements", JsValue::Number(replacements as f64)),
            (
                "changed",
                JsValue::Bool(write.created || next_content != content),
            ),
        ]))),
        metadata: Some(object([
            ("bytesWritten", JsValue::Number(next_content.len() as f64)),
            ("mtimeMs", JsValue::Number(write.mtime_ms)),
        ])),
        supplemental_messages: None,
    })
}

fn normalize_quotes(value: &str) -> String {
    value
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{201C}', '\u{201D}'], "\"")
}

fn trim_end_js(value: &str) -> &str {
    value.trim_end_matches(js_space)
}

/// `stripTrailingWhitespace`: split on the three line terminators, trim only
/// the non-separator pieces, and re-join unchanged.
fn strip_trailing_whitespace(value: &str) -> String {
    let mut output = String::new();
    let mut rest = value;
    while let Some(index) = rest.find(['\r', '\n']) {
        let (line, tail) = rest.split_at(index);
        output.push_str(trim_end_js(line));
        if let Some(after) = tail.strip_prefix("\r\n") {
            output.push_str("\r\n");
            rest = after;
        } else {
            output.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    output.push_str(trim_end_js(rest));
    output
}

fn normalize_edit_input(absolute: &str, old: &str, new: &str) -> (String, String) {
    let lower = absolute.to_lowercase();
    let markdown = lower.ends_with(".md") || lower.ends_with(".mdx");
    (
        old.to_string(),
        if markdown {
            new.to_string()
        } else {
            strip_trailing_whitespace(new)
        },
    )
}

/// `findActualString`: exact match first, then curly-quote-normalized match
/// mapped back to the original string by UTF-16 index.
fn find_actual_string(content: &str, search: &str) -> Option<String> {
    if content.contains(search) {
        return Some(search.to_string());
    }
    let normalized_search = normalize_quotes(search);
    let normalized_file = normalize_quotes(content);
    let file_units: Vec<u16> = normalized_file.encode_utf16().collect();
    let search_units: Vec<u16> = normalized_search.encode_utf16().collect();
    if search_units.is_empty() || search_units.len() > file_units.len() {
        return None;
    }
    let index = file_units
        .windows(search_units.len())
        .position(|window| window == search_units.as_slice())?;
    let original: Vec<u16> = content.encode_utf16().collect();
    let end = index
        .saturating_add(search.encode_utf16().count())
        .min(original.len());
    Some(String::from_utf16_lossy(&original[index..end]))
}

fn count_occurrences(value: &str, search: &str) -> usize {
    if search.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while let Some(index) = value[start..].find(search) {
        count += 1;
        start += index + search.len();
        if start > value.len() {
            break;
        }
    }
    count
}
