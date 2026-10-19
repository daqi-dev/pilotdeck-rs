//! `edit_notebook` (baseline `tool/builtin/editNotebook.ts`): locate a cell by
//! id or `cell-N`, replace/insert/delete it and re-serialize the notebook with
//! `JSON.stringify(notebook, null, 1)` semantics.
use super::{
    ensure_write_snapshot_fresh, field, file_history, message_id, object, record_write_snapshot,
    resolve_write_path, schema, string, text, tool_error, validate_write_snapshot_fresh,
    write_text_file,
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{fs, io};

pub struct EditNotebookTool {
    fields: JsObject,
}

impl EditNotebookTool {
    pub fn new() -> Self {
        Self {
            fields: definition(),
        }
    }
}

impl Default for EditNotebookTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for EditNotebookTool {
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

impl ExecutableTool for EditNotebookTool {
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
        ("name", string("edit_notebook")),
        ("aliases", JsValue::Array(vec![string("NotebookEdit")])),
        (
            "description",
            string(
                "Edit a Jupyter notebook (.ipynb) by replacing, inserting, or deleting a specific cell.\n\nUsage:\n- Use this tool for notebook cell edits instead of edit_file.\n- notebook_path may be relative to the current workspace or absolute, but it must resolve inside the workspace.\n- edit_mode defaults to replace and supports replace, insert, or delete.\n- insert requires cell_type and inserts after the referenced cell, or at the beginning when cell_id is omitted.\n- cell_id may be a real notebook cell id or a synthetic index in the form cell-N.\n- You MUST use read_file on the notebook first; stale notebook edits are rejected.",
            ),
        ),
        ("kind", string("filesystem")),
        ("searchHint", string("edit notebook cells in ipynb files")),
        (
            "inputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["notebook_path","new_source"],"additionalProperties":false,"properties":{"notebook_path":{"type":"string","description":"Relative or absolute path to the Jupyter notebook file."},"cell_id":{"type":"string","description":"Notebook cell id, or a synthetic index in the form cell-N."},"new_source":{"type":"string","description":"New source to write to the cell. Still required for delete to preserve legacy parity."},"cell_type":{"type":"string","enum":["code","markdown"],"description":"Cell type for inserts, or an optional replacement type for replace."},"edit_mode":{"type":"string","enum":["replace","insert","delete"],"description":"Notebook edit mode. Defaults to replace."}}}"#,
            )),
        ),
        (
            "outputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["notebook_path","new_source","cell_type","language","edit_mode","original_file","updated_file"],"additionalProperties":false,"properties":{"notebook_path":{"type":"string"},"cell_id":{"type":"string"},"new_source":{"type":"string"},"cell_type":{"type":"string","enum":["code","markdown"]},"language":{"type":"string"},"edit_mode":{"type":"string","enum":["replace","insert","delete"]},"original_file":{"type":"string"},"updated_file":{"type":"string"},"error":{"type":"string"}}}"#,
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

fn input_string(input: &JsValue, key: &str) -> Option<JsString> {
    field(input, key).as_string().cloned()
}

fn is_notebook(path: &JsString) -> bool {
    super::lower(&super::extname(path)) == ".ipynb"
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditMode {
    Replace,
    Insert,
    Delete,
}

impl EditMode {
    fn parse(value: Option<&JsString>) -> Self {
        match value.map(text).as_deref() {
            Some("insert") => Self::Insert,
            Some("delete") => Self::Delete,
            _ => Self::Replace,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Insert => "insert",
            Self::Delete => "delete",
        }
    }
}

fn validate(input: &JsValue, context: &ToolRunContext) -> Result<Option<Vec<JsObject>>, ToolError> {
    let path = input_string(input, "notebook_path").unwrap_or_else(|| JsString::from(""));
    let cell_id = input_string(input, "cell_id").filter(|value| !value.is_empty());
    let edit_mode_value = input_string(input, "edit_mode");
    let edit_mode = EditMode::parse(edit_mode_value.as_ref());
    let cell_type = input_string(input, "cell_type");
    let resolved = match resolve_write_path(&path, context, true, false) {
        Ok(resolved) => resolved,
        Err(error) if error.code == "tool_execution_failed" => return Err(error),
        Err(error) => {
            return Ok(Some(vec![issue("notebook_path", error.message.clone())]));
        }
    };
    if !is_notebook(&resolved.absolute) {
        return Ok(Some(vec![issue(
            "notebook_path",
            "File must be a Jupyter notebook (.ipynb file). Use edit_file for non-notebook files.",
        )]));
    }
    if edit_mode == EditMode::Insert && cell_type.is_none() {
        return Ok(Some(vec![issue(
            "cell_type",
            "cell_type is required when edit_mode is insert.",
        )]));
    }
    match validate_write_snapshot_fresh(context, &resolved.absolute) {
        Ok(_) => {}
        Err(error)
            if error.code == "invalid_tool_input"
                && (error.message.equals_str(
                    "File has not been read yet. Read it first before writing to it.",
                ) || error.message.equals_str(
                    "File has changed since the last read. Read it again before writing to it.",
                )) =>
        {
            return Ok(Some(vec![issue("notebook_path", error.message.clone())]));
        }
        Err(error) => return Err(error),
    }
    let raw = read_notebook_json(&resolved.absolute)?;
    let notebook = parse_notebook(&raw)?;
    let cells = notebook_cells(&notebook)?;
    let Some(cell_id) = cell_id else {
        if edit_mode == EditMode::Insert {
            return Ok(None);
        }
        return Ok(Some(vec![issue(
            "cell_id",
            "cell_id is required unless edit_mode is insert.",
        )]));
    };
    let located = locate_cell_index(&cells, &cell_id);
    if !located.found {
        let can_append_via_replace =
            edit_mode == EditMode::Replace && located.parsed_index == Some(cells.len());
        if !can_append_via_replace {
            return Ok(Some(vec![issue(
                "cell_id",
                format!("Cell {} does not exist in the notebook.", text(&cell_id)),
            )]));
        }
    }
    Ok(None)
}

fn execute(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let path = input_string(input, "notebook_path").unwrap_or_else(|| JsString::from(""));
    let cell_id = input_string(input, "cell_id").filter(|value| !value.is_empty());
    let new_source = input_string(input, "new_source").unwrap_or_else(|| JsString::from(""));
    let edit_mode_value = input_string(input, "edit_mode");
    let requested_mode = EditMode::parse(edit_mode_value.as_ref());
    let requested_cell_type = input_string(input, "cell_type");
    let resolved = resolve_write_path(&path, context, true, false)?;
    if !is_notebook(&resolved.absolute) {
        return Err(tool_error(
            "invalid_tool_input",
            "File must be a Jupyter notebook (.ipynb file). Use edit_file for non-notebook files.",
        ));
    }
    let freshness = ensure_write_snapshot_fresh(context, &resolved.absolute)?;
    let message = message_id(context);
    if let Some(history) = file_history(context) {
        history
            .track_edit(&text(&resolved.absolute), &message)
            .map_err(|error| tool_error("tool_execution_failed", error.to_string()))?;
    }
    let original_content = match freshness.previous_content {
        Some(content) => content,
        None => read_notebook_json(&resolved.absolute)?,
    };
    let mut notebook = parse_notebook(&original_content)?;
    let mut cells = notebook_cells(&notebook)?;
    let located = cell_id
        .as_ref()
        .map(|id| locate_cell_index(&cells, id))
        .unwrap_or(Located {
            found: false,
            index: 0,
            parsed_index: None,
        });
    let mut cell_index = located.index;
    if requested_mode == EditMode::Insert && cell_id.is_some() {
        cell_index += 1;
    }
    let mut effective_mode = requested_mode;
    let mut effective_cell_type = requested_cell_type.clone();
    if effective_mode == EditMode::Replace && cell_index == cells.len() {
        effective_mode = EditMode::Insert;
        if effective_cell_type.is_none() {
            effective_cell_type = Some(JsString::from("code"));
        }
    }
    let language = notebook_language(&notebook);
    let supports_cell_ids = supports_notebook_cell_ids(&notebook);
    let result_cell_id: Option<JsString> = if effective_mode == EditMode::Insert {
        if supports_cell_ids {
            Some(create_notebook_cell_id())
        } else {
            cell_id.clone()
        }
    } else {
        cell_id.clone()
    };

    match effective_mode {
        EditMode::Delete => {
            if cell_index >= cells.len() {
                return Err(missing_cell_error(&cell_id, cell_index));
            }
            cells.remove(cell_index);
        }
        EditMode::Insert => {
            if cell_index > cells.len() {
                return Err(missing_cell_error(&cell_id, cell_index));
            }
            let cell_type = effective_cell_type
                .as_ref()
                .map(text)
                .unwrap_or_else(|| "code".into());
            cells.insert(
                cell_index,
                inserted_cell(&cell_type, &new_source, result_cell_id.as_ref()),
            );
        }
        EditMode::Replace => {
            let Some(JsValue::Object(target)) = cells.get_mut(cell_index) else {
                return Err(missing_cell_error(&cell_id, cell_index));
            };
            set_cell_source(target, &new_source);
            if target
                .get("cell_type")
                .and_then(JsValue::as_string)
                .is_some_and(|kind| kind.equals_str("code"))
            {
                target.insert("execution_count".into(), JsValue::Null);
                target.insert("outputs".into(), JsValue::Array(Vec::new()));
            }
            if let Some(cell_type) = &requested_cell_type
                && target
                    .get("cell_type")
                    .and_then(JsValue::as_string)
                    .is_none_or(|kind| kind != cell_type)
            {
                target.insert("cell_type".into(), string(cell_type.clone()));
            }
        }
    }

    notebook.insert("cells".into(), JsValue::Array(cells.clone()));
    let updated_content = format!("{}\n", pretty_json(&JsValue::Object(notebook.clone())));
    let write = write_text_file(&resolved.absolute, &updated_content)?;
    if let Some(history) = file_history(context) {
        history
            .mark_edit_committed(&text(&resolved.absolute), &message)
            .map_err(|error| tool_error("tool_execution_failed", error.to_string()))?;
    }
    super::invalidate_read_file_state(context, &resolved.absolute);
    record_write_snapshot(
        context,
        &resolved.absolute,
        &updated_content,
        write.mtime_ms,
        None,
    );

    let cell_type = match effective_cell_type {
        Some(value) => text(&value),
        None => infer_result_cell_type(&cells, cell_index, effective_mode).to_string(),
    };
    let mut data = object([
        ("notebook_path", string(resolved.absolute.clone())),
        ("new_source", string(new_source.clone())),
        ("cell_type", string(cell_type.as_str())),
        ("language", string(language.as_str())),
        ("edit_mode", string(effective_mode.as_str())),
        ("original_file", string(original_content.as_str())),
        ("updated_file", string(updated_content.as_str())),
    ]);
    if let Some(cell_id) = &result_cell_id {
        data.insert("cell_id".into(), string(cell_id.clone()));
    }
    // TS object literal places cell_id after notebook_path; rebuild in that
    // order so the wire JSON is byte-identical.
    let mut ordered = JsObject::new();
    for key in [
        "notebook_path",
        "cell_id",
        "new_source",
        "cell_type",
        "language",
        "edit_mode",
        "original_file",
        "updated_file",
    ] {
        if let Some(value) = data.get(key) {
            ordered.insert(key.into(), value.clone());
        }
    }

    let suffix = result_cell_id
        .as_ref()
        .map(|id| format!(" {}", text(id)))
        .unwrap_or_default();
    let result_text = match effective_mode {
        EditMode::Insert => {
            format!(
                "Inserted notebook cell{suffix} in {}.",
                text(&resolved.relative)
            )
        }
        EditMode::Delete => {
            format!(
                "Deleted notebook cell{suffix} from {}.",
                text(&resolved.relative)
            )
        }
        EditMode::Replace => {
            format!(
                "Updated notebook cell{suffix} in {}.",
                text(&resolved.relative)
            )
        }
    };
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(result_text.as_str())),
        ]))],
        data: Some(JsValue::Object(ordered)),
        metadata: Some(object([
            (
                "bytesWritten",
                JsValue::Number(updated_content.len() as f64),
            ),
            ("mtimeMs", JsValue::Number(write.mtime_ms)),
        ])),
        supplemental_messages: None,
    })
}

fn missing_cell_error(cell_id: &Option<JsString>, index: usize) -> ToolError {
    let name = cell_id
        .as_ref()
        .map(text)
        .unwrap_or_else(|| format!("cell-{index}"));
    tool_error(
        "invalid_tool_input",
        format!("Cell {name} does not exist in the notebook."),
    )
}

fn read_notebook_json(path: &JsString) -> Result<String, ToolError> {
    match fs::read(super::os_path(path)) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(tool_error(
            "file_not_found",
            format!("File {} does not exist.", text(path)),
        )),
        Err(error) => Err(tool_error("tool_execution_failed", error.to_string())),
    }
}

fn parse_notebook(raw: &str) -> Result<JsObject, ToolError> {
    let value = parse_json(raw)
        .map_err(|_| tool_error("invalid_tool_input", "Notebook is not valid JSON."))?;
    match &value {
        JsValue::Object(notebook) => Ok(notebook.clone()),
        _ => Err(tool_error(
            "invalid_tool_input",
            "Notebook is not valid JSON.",
        )),
    }
}

fn notebook_cells(notebook: &JsObject) -> Result<Vec<JsValue>, ToolError> {
    match notebook.get("cells") {
        None | Some(JsValue::Null) => Ok(Vec::new()),
        Some(JsValue::Array(cells)) => Ok(cells.clone()),
        Some(_) => Err(ToolError::failed("notebook.cells is not an array.")),
    }
}

#[derive(Clone, Copy)]
struct Located {
    found: bool,
    index: usize,
    parsed_index: Option<usize>,
}

fn locate_cell_index(cells: &[JsValue], cell_id: &JsString) -> Located {
    if let Some(index) = cells.iter().position(|cell| {
        cell.get("id")
            .and_then(JsValue::as_string)
            .is_some_and(|id| id == cell_id)
    }) {
        return Located {
            found: true,
            index,
            parsed_index: None,
        };
    }
    match parse_cell_id(cell_id) {
        Some(parsed) => {
            let index = if parsed > usize::MAX as f64 {
                usize::MAX
            } else {
                parsed as usize
            };
            Located {
                found: (parsed as usize) < cells.len() && parsed >= 0.0,
                index,
                parsed_index: Some(index),
            }
        }
        None => Located {
            found: false,
            index: usize::MAX,
            parsed_index: None,
        },
    }
}

fn parse_cell_id(cell_id: &JsString) -> Option<f64> {
    let value = text(cell_id);
    let digits = value.strip_prefix("cell-")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(digits.parse::<f64>().unwrap_or(f64::INFINITY))
}

fn notebook_number(notebook: &JsObject, key: &str) -> f64 {
    match notebook.get(key) {
        None | Some(JsValue::Null) => 0.0,
        Some(value) => crate::js_value::to_number(value).unwrap_or(f64::NAN),
    }
}

fn supports_notebook_cell_ids(notebook: &JsObject) -> bool {
    let format = notebook_number(notebook, "nbformat");
    let minor = notebook_number(notebook, "nbformat_minor");
    format > 4.0 || (format == 4.0 && minor >= 5.0)
}

fn notebook_language(notebook: &JsObject) -> String {
    notebook
        .get("metadata")
        .and_then(JsValue::as_object)
        .and_then(|metadata| metadata.get("language_info"))
        .and_then(JsValue::as_object)
        .and_then(|language| language.get("name"))
        .and_then(JsValue::as_string)
        .map(text)
        .unwrap_or_else(|| "python".into())
}

fn inserted_cell(cell_type: &str, source: &JsString, id: Option<&JsString>) -> JsValue {
    let mut cell = JsObject::new();
    cell.insert("cell_type".into(), string(cell_type));
    cell.insert(
        "id".into(),
        id.map(|id| string(id.clone()))
            .unwrap_or(JsValue::Undefined),
    );
    cell.insert("source".into(), string(source.clone()));
    cell.insert("metadata".into(), JsValue::Object(JsObject::new()));
    if cell_type == "code" {
        cell.insert("execution_count".into(), JsValue::Null);
        cell.insert("outputs".into(), JsValue::Array(Vec::new()));
    }
    JsValue::Object(cell)
}

fn set_cell_source(cell: &mut JsObject, source: &JsString) {
    cell.insert("source".into(), string(source.clone()));
}

fn create_notebook_cell_id() -> JsString {
    let raw = uuid::Uuid::new_v4().simple().to_string();
    JsString::from(&raw[..13])
}

fn infer_result_cell_type(cells: &[JsValue], index: usize, mode: EditMode) -> &'static str {
    if mode == EditMode::Delete {
        return "code";
    }
    match cells
        .get(index)
        .and_then(|cell| cell.get("cell_type"))
        .and_then(JsValue::as_string)
        .map(text)
        .as_deref()
    {
        Some("markdown") => "markdown",
        _ => "code",
    }
}

// ---------------------------------------------------------------------------
// `JSON.stringify(value, null, 1)` (iterative; no recursive descent).

enum Task<'a> {
    Value(&'a JsValue, usize),
    Key(&'a JsString, usize),
    Indent(usize),
    Raw(String),
}

fn scalar_json(value: &JsValue) -> String {
    match value {
        JsValue::Undefined => "null".into(),
        _ => stringify_json(value)
            .ok()
            .flatten()
            .unwrap_or_else(|| "null".into()),
    }
}

fn pretty_json(value: &JsValue) -> String {
    let mut output = String::new();
    let mut stack = vec![Task::Value(value, 0)];
    while let Some(task) = stack.pop() {
        match task {
            Task::Raw(raw) => output.push_str(&raw),
            Task::Indent(depth) => output.push_str(&" ".repeat(depth)),
            Task::Key(key, depth) => {
                output.push_str(&" ".repeat(depth));
                let encoded = stringify_json(&JsValue::String(key.clone()))
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "\"\"".into());
                output.push_str(&encoded);
                output.push_str(": ");
            }
            Task::Value(value, depth) => match value {
                JsValue::Object(object) => {
                    let mut indexed: Vec<&(JsString, JsValue)> = object
                        .entries()
                        .iter()
                        .filter(|(key, value)| key.array_index().is_some() && !value.is_undefined())
                        .collect();
                    let ordinary: Vec<&(JsString, JsValue)> = object
                        .entries()
                        .iter()
                        .filter(|(key, value)| key.array_index().is_none() && !value.is_undefined())
                        .collect();
                    indexed.sort_by_key(|(key, _)| key.array_index());
                    let ordered: Vec<&(JsString, JsValue)> =
                        indexed.into_iter().chain(ordinary).collect();
                    if ordered.is_empty() {
                        output.push_str("{}");
                        continue;
                    }
                    output.push_str("{\n");
                    stack.push(Task::Raw(format!("\n{}}}", " ".repeat(depth))));
                    for index in (0..ordered.len()).rev() {
                        if index + 1 < ordered.len() {
                            stack.push(Task::Raw(",\n".into()));
                        }
                        stack.push(Task::Value(&ordered[index].1, depth + 1));
                        stack.push(Task::Key(&ordered[index].0, depth + 1));
                    }
                }
                JsValue::Array(items) => {
                    if items.is_empty() {
                        output.push_str("[]");
                        continue;
                    }
                    output.push_str("[\n");
                    stack.push(Task::Raw(format!("\n{}]", " ".repeat(depth))));
                    for index in (0..items.len()).rev() {
                        if index + 1 < items.len() {
                            stack.push(Task::Raw(",\n".into()));
                        }
                        stack.push(Task::Value(&items[index], depth + 1));
                        stack.push(Task::Indent(depth + 1));
                    }
                }
                scalar => output.push_str(&scalar_json(scalar)),
            },
        }
    }
    output
}
