//! `write_file` (baseline `tool/builtin/writeFile.ts`): read-before-write
//! snapshots, workspace containment, file-history backup and the structured
//! patch / unified diff payload.
use super::{
    ensure_write_snapshot_fresh, field, file_history, message_id, object, record_write_snapshot,
    resolve_write_path, schema, string, text, tool_error, validate_write_snapshot_fresh,
    write_text_file,
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub struct WriteFileTool {
    fields: JsObject,
}

impl WriteFileTool {
    pub fn new() -> Self {
        Self {
            fields: definition(),
        }
    }
}

impl Default for WriteFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for WriteFileTool {
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

impl ExecutableTool for WriteFileTool {
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
        ("name", string("write_file")),
        ("aliases", JsValue::Array(vec![string("Write")])),
        (
            "description",
            string(
                "Writes a UTF-8 text file inside the workspace, or outside the workspace after explicit host permission.\n\nUsage:\n- The file_path parameter may be relative to the current workspace or an absolute path. Paths outside the workspace require explicit user permission before execution.\n- You may create a new file directly; use ls/glob/git status if you need to confirm it does not exist.\n- This tool will overwrite the existing file if there is one at the provided path.\n- You must read an existing file with read_file before writing to it. This tool will fail if an existing file was not read first.\n- If the target file changed after the last read, this tool will fail and you must read it again before writing.\n- Prefer the edit_file tool for modifying existing files. Only use this tool to create new files or for complete rewrites.\n- Prefer this tool for reusable scripts, generators, and parameterized helpers before running them with bash.\n- The returned filePath is always the resolved absolute path.\n- Do not create documentation files (*.md) or README files unless explicitly requested by the User.\n- Only use emojis if the user explicitly requests it. Avoid writing emojis to files unless asked.",
            ),
        ),
        ("kind", string("filesystem")),
        (
            "inputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["file_path","content"],"additionalProperties":false,"properties":{"file_path":{"type":"string","description":"The path to the file to write. It may be relative to the current workspace or absolute. Paths outside the workspace require explicit user permission."},"content":{"type":"string","description":"The content to write to the file."}}}"#,
            )),
        ),
        (
            "outputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["type","filePath","content","structuredPatch","originalFile"],"additionalProperties":false,"properties":{"type":{"type":"string","enum":["create","update"]},"filePath":{"type":"string"},"content":{"type":"string"},"structuredPatch":{"type":"array","items":{"type":"object","required":["oldStart","oldLines","newStart","newLines","lines"],"additionalProperties":false,"properties":{"oldStart":{"type":"integer"},"oldLines":{"type":"integer"},"newStart":{"type":"integer"},"newLines":{"type":"integer"},"lines":{"type":"array","items":{"type":"object","required":["type","text"],"additionalProperties":false,"properties":{"type":{"type":"string","enum":["context","delete","add"]},"text":{"type":"string"}}}}}}},"originalFile":{"type":["string","null"]},"gitDiff":{"type":"object","required":["path","diff"],"additionalProperties":false,"properties":{"path":{"type":"string"},"diff":{"type":"string"}}}}}"#,
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

fn input_path(input: &JsValue) -> JsString {
    field(input, "file_path")
        .as_string()
        .cloned()
        .unwrap_or_else(|| JsString::from(""))
}

fn input_content(input: &JsValue) -> JsString {
    field(input, "content")
        .as_string()
        .cloned()
        .unwrap_or_else(|| JsString::from(""))
}

fn validate(input: &JsValue, context: &ToolRunContext) -> Result<Option<Vec<JsObject>>, ToolError> {
    let path = input_path(input);
    let resolved = match resolve_write_path(&path, context, false, true) {
        Ok(resolved) => resolved,
        // A real filesystem error (ENOTDIR/EACCES/ELOOP) is an execution
        // failure in TS, not a schema issue; only lexical/real containment
        // failures are reported as invalid input here.
        Err(error) if error.code == "tool_execution_failed" => return Err(error),
        Err(error) => return Ok(Some(vec![issue("file_path", error.message.clone())])),
    };
    match validate_write_snapshot_fresh(context, &resolved.absolute) {
        Ok(_) => Ok(None),
        Err(error)
            if error.code == "invalid_tool_input"
                && (error.message.equals_str(
                    "File has not been read yet. Read it first before writing to it.",
                ) || error.message.equals_str(
                    "File has changed since the last read. Read it again before writing to it.",
                )) =>
        {
            Ok(Some(vec![issue("file_path", error.message.clone())]))
        }
        Err(error) => Err(error),
    }
}

fn execute(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let path = input_path(input);
    let content_value = input_content(input);
    let content = String::from_utf16_lossy(content_value.units());
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

    let write = write_text_file(&resolved.absolute, &content)?;
    if let Some(history) = file_history(context) {
        history
            .mark_edit_committed(&text(&resolved.absolute), &message)
            .map_err(|error| tool_error("tool_execution_failed", error.to_string()))?;
    }
    super::invalidate_read_file_state(context, &resolved.absolute);
    record_write_snapshot(context, &resolved.absolute, &content, write.mtime_ms, None);

    let kind = if write.created { "create" } else { "update" };
    let structured_patch = build_structured_patch(freshness.previous_content.as_deref(), &content);
    let unified = build_unified_diff(
        &text(&resolved.relative),
        freshness.previous_content.as_deref(),
        &content,
    );
    let mut data = object([
        ("type", string(kind)),
        ("filePath", string(resolved.absolute.clone())),
        ("content", string(content_value.clone())),
        ("structuredPatch", structured_patch),
        (
            "originalFile",
            freshness
                .previous_content
                .as_ref()
                .map(|previous| string(previous.as_str()))
                .unwrap_or(JsValue::Null),
        ),
    ]);
    if !unified.is_empty() {
        data.insert(
            "gitDiff".into(),
            JsValue::Object(object([
                ("path", string(resolved.relative.clone())),
                ("diff", string(unified.as_str())),
            ])),
        );
    }

    let success = format!(
        "{} {}.",
        if write.created {
            "Created"
        } else {
            "Overwrote"
        },
        text(&resolved.relative)
    );
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(success.as_str())),
        ]))],
        data: Some(JsValue::Object(data)),
        metadata: Some(object([
            ("bytesWritten", JsValue::Number(content.len() as f64)),
            ("mtimeMs", JsValue::Number(write.mtime_ms)),
        ])),
        supplemental_messages: None,
    })
}

fn split_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        Vec::new()
    } else {
        content.split('\n').collect()
    }
}

fn patch_line(kind: &str, line: &str) -> JsValue {
    JsValue::Object(object([("type", string(kind)), ("text", string(line))]))
}

fn build_structured_patch(old: Option<&str>, new: &str) -> JsValue {
    let Some(old) = old else {
        let added = split_lines(new);
        if added.is_empty() {
            return JsValue::Array(Vec::new());
        }
        return JsValue::Array(vec![JsValue::Object(object([
            ("oldStart", JsValue::Number(1.0)),
            ("oldLines", JsValue::Number(0.0)),
            ("newStart", JsValue::Number(1.0)),
            ("newLines", JsValue::Number(added.len() as f64)),
            (
                "lines",
                JsValue::Array(
                    added
                        .into_iter()
                        .map(|line| patch_line("add", line))
                        .collect(),
                ),
            ),
        ]))]);
    };
    if old == new {
        return JsValue::Array(Vec::new());
    }
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let context_lines = 3;
    let old_changed_end = old_lines.len().saturating_sub(suffix);
    let new_changed_end = new_lines.len().saturating_sub(suffix);
    let old_context_start = prefix.saturating_sub(context_lines);
    let new_context_start = prefix.saturating_sub(context_lines);
    let old_context_end = old_changed_end
        .saturating_add(context_lines)
        .min(old_lines.len());
    let new_context_end = new_changed_end
        .saturating_add(context_lines)
        .min(new_lines.len());

    let mut lines = Vec::new();
    for line in &old_lines[old_context_start..prefix] {
        lines.push(patch_line("context", line));
    }
    for line in &old_lines[prefix..old_changed_end] {
        lines.push(patch_line("delete", line));
    }
    for line in &new_lines[prefix..new_changed_end] {
        lines.push(patch_line("add", line));
    }
    for line in &new_lines[new_changed_end..new_context_end] {
        lines.push(patch_line("context", line));
    }
    JsValue::Array(vec![JsValue::Object(object([
        ("oldStart", JsValue::Number((old_context_start + 1) as f64)),
        (
            "oldLines",
            JsValue::Number((old_context_end - old_context_start) as f64),
        ),
        ("newStart", JsValue::Number((new_context_start + 1) as f64)),
        (
            "newLines",
            JsValue::Number((new_context_end - new_context_start) as f64),
        ),
        ("lines", JsValue::Array(lines)),
    ]))])
}

fn format_range(start: usize, length: usize) -> String {
    if length == 0 {
        format!("{start},0")
    } else if length == 1 {
        format!("{start}")
    } else {
        format!("{start},{length}")
    }
}

fn build_unified_diff(path: &str, old: Option<&str>, new: &str) -> String {
    let hunks = build_structured_patch(old, new);
    if old.is_some() && matches!(&hunks, JsValue::Array(items) if items.is_empty()) {
        return String::new();
    }
    let from = if old.is_none() {
        "/dev/null".to_string()
    } else {
        format!("a/{path}")
    };
    let to = format!("b/{path}");
    let body = match &hunks {
        JsValue::Array(items) => items
            .iter()
            .map(|hunk| {
                let old_start = hunk
                    .get("oldStart")
                    .and_then(JsValue::as_number)
                    .unwrap_or(0.0) as usize;
                let old_lines = hunk
                    .get("oldLines")
                    .and_then(JsValue::as_number)
                    .unwrap_or(0.0) as usize;
                let new_start = hunk
                    .get("newStart")
                    .and_then(JsValue::as_number)
                    .unwrap_or(0.0) as usize;
                let new_lines = hunk
                    .get("newLines")
                    .and_then(JsValue::as_number)
                    .unwrap_or(0.0) as usize;
                let header = format!(
                    "@@ -{} +{} @@",
                    format_range(old_start, old_lines),
                    format_range(new_start, new_lines)
                );
                let mut lines = vec![header];
                if let Some(JsValue::Array(items)) = hunk.get("lines") {
                    for line in items {
                        let kind = line
                            .get("type")
                            .and_then(JsValue::as_string)
                            .map(text)
                            .unwrap_or_default();
                        let line_text = line
                            .get("text")
                            .and_then(JsValue::as_string)
                            .map(text)
                            .unwrap_or_default();
                        let prefix = match kind.as_str() {
                            "delete" => "-",
                            "add" => "+",
                            _ => " ",
                        };
                        lines.push(format!("{prefix}{line_text}"));
                    }
                }
                lines.join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    if body.is_empty() {
        format!("--- {from}\n+++ {to}")
    } else {
        format!("--- {from}\n+++ {to}\n{body}")
    }
}
