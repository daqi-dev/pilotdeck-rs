//! `read_file` (baseline `tool/builtin/readFile.ts`): text pagination,
//! line numbers, plan/BOM handling, notebook rendering, binary and image/PDF
//! classification, dedup state and write snapshots. Image and PDF attachment
//! require the host's multimodal encoder, which this read-only slice never
//! fabricates; without a multimodal declaration the baseline itself returns a
//! text notice, and that branch is implemented here verbatim.
use super::{
    ResolvedPath, StatKind, cwd as context_cwd, extname, failed, field, is_absolute, join,
    js_value_string, lower, normalize, number, object, os_path, resolve_workspace_path, schema,
    stat, string, text, tool_error, truthy,
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use sha2::{Digest, Sha256};

const MAX_PDF_PAGES_PER_REQUEST: u64 = 20;
const LARGE_TEXT_AUTO_PAGE_BYTES: usize = 200_000;
const SAFE_TEXT_BUDGET_BYTES: usize = 80_000;
const MAX_TEXT_TOKENS: usize = 25_000;
const OVERSIZED_LINE_PREVIEW_BYTES: usize = 20_000;
const DEFAULT_LARGE_TEXT_PREVIEW_LINES: usize = 2_000;
const FILE_UNCHANGED_STUB: &str = "File unchanged since the last read. Refer to the earlier read_file result instead of re-reading it.";

const BINARY_EXTENSIONS: &[&str] = &[
    ".exe", ".dll", ".so", ".dylib", ".class", ".jar", ".bin", ".dat", ".zip", ".gz", ".tar",
    ".7z", ".rar", ".doc", ".docx", ".ppt", ".pptx", ".xls", ".xlsx", ".odt", ".ods", ".odp",
    ".pages", ".key", ".numbers", ".rp", ".mp3", ".mp4", ".mov", ".avi", ".wav", ".ogg", ".woff",
    ".woff2", ".ttf", ".otf", ".xlsm", ".xlt", ".xltx", ".xltm", ".docm", ".dot", ".dotx", ".dotm",
    ".pptm", ".pot", ".potx", ".potm", ".pps", ".ppsx", ".ppsm",
];

const BLOCKED_DEVICE_PATHS: &[&str] = &[
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/full",
    "/dev/stdin",
    "/dev/tty",
    "/dev/console",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/fd/0",
    "/dev/fd/1",
    "/dev/fd/2",
];

const WINDOWS_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub struct ReadFileTool {
    fields: JsObject,
}

impl ReadFileTool {
    pub fn new() -> Self {
        Self {
            fields: definition(),
        }
    }
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for ReadFileTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}

impl ExecutableTool for ReadFileTool {
    fn validate_input(
        &self,
        input: JsValue,
        context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move { Ok(validate(&input, &context)) })
    }

    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move { execute(&input, &context) })
    }
}

fn definition() -> JsObject {
    object([
        ("name", string("read_file")),
        ("aliases", JsValue::Array(vec![string("Read")])),
        (
            "description",
            string(
                "Reads a file from the current workspace. You can access workspace files directly by using this tool.\n\
If the User provides a path to a file, assume that path is valid as long as it resolves inside the current workspace. It is okay to read a file that does not exist; an error will be returned.\n\n\
Usage:\n\
- The file_path parameter may be a workspace-relative path or an absolute path, but it must resolve inside the current workspace\n\
- If the user asks you to send or share a file, use send_attachment instead of read_file; do not inspect arbitrary binary files before sending them\n\
- By default, offset is 1 and the tool reads from the beginning of the file\n\
- You can optionally specify offset and limit (especially handy for long files), but it's recommended to read the whole file by not providing these parameters\n\
- Results are returned using cat -n format, with line numbers starting at 1\n\
- For image files, screenshots, extracted video frames, and PDFs, use this tool to inspect the visual/document content directly\n\
- When the current model supports image input, image files are returned as model-visible image content\n\
- When the current model supports PDF input, small PDF files are returned as model-visible PDF content. When specific PDF pages are requested, pages are rendered as model-visible images if the model supports image input\n\
- Do not manually base64-encode local images/PDFs or route them through another vision/document API unless read_file reports that the current model cannot read that content\n\
- When the current model does not support the required modality, reading the file returns a text notice explaining that the current model cannot read it\n\
- For large PDFs, provide the pages parameter to validate specific page ranges (e.g., pages: \"1-5\"). Maximum 20 pages per request\n\
- This tool can read Jupyter notebooks (.ipynb files) and returns a text rendering of notebook cells and outputs\n\
- This tool can only read files, not directories\n\
- If you read a file that exists but has empty contents you will receive a system reminder warning in place of file contents\n\
- If a previous tool result says it was persisted, truncated, or preview-only, read the file_path shown in that notice with read_file",
            ),
        ),
        ("kind", string("filesystem")),
        (
            "inputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["file_path"],"additionalProperties":false,"properties":{"file_path":{"type":"string","description":"The relative or absolute path to the file to read. The path must resolve inside the current workspace."},"offset":{"type":"integer","description":"The 1-based line number to start reading from. Only provide if the file is too large to read at once."},"limit":{"type":"integer","description":"The number of lines to read. Only provide if the file is too large to read at once."},"pages":{"type":"string","description":"Page range for PDF files (e.g., \"1-5\", \"3\", \"10-20\"). Only applicable to PDF files. Maximum 20 pages per request."}}}"#,
            )),
        ),
        ("maxResultBytes", JsValue::Number(200_000.0)),
    ])
}

fn validate(input: &JsValue, context: &ToolRunContext) -> Option<Vec<JsObject>> {
    let mut issues = Vec::new();
    if let Some(offset) = number(field(input, "offset"))
        && offset < 1.0
    {
        issues.push(object([
            ("path", string("offset")),
            ("code", string("invalid_schema")),
            (
                "message",
                string("offset must be a 1-based line number (>= 1)."),
            ),
        ]));
    }
    if let Some(limit) = number(field(input, "limit"))
        && limit < 0.0
    {
        issues.push(object([
            ("path", string("limit")),
            ("code", string("invalid_schema")),
            (
                "message",
                string("limit must be greater than or equal to 0."),
            ),
        ]));
    }
    let pages = field(input, "pages");
    if let Some(value) = pages.as_string()
        && text(value).trim().is_empty()
    {
        // TS drops an empty `pages` from the validation copy. The runtime
        // ignores a custom validator's returned input, so only the issue list
        // is observable here.
    }
    if let Some(value) = pages.as_string()
        && !text(value).trim().is_empty()
    {
        match parse_pdf_page_range(&text(value)) {
            None => issues.push(object([
                ("path", string("pages")),
                ("code", string("invalid_schema")),
                (
                    "message",
                    string("pages must use formats like \"1-5\" or \"3\"."),
                ),
            ])),
            Some(range) if range.1 - range.0 + 1 > MAX_PDF_PAGES_PER_REQUEST => {
                issues.push(object([
                    ("path", string("pages")),
                    ("code", string("invalid_schema")),
                    (
                        "message",
                        string(format!(
                            "pages exceeds the maximum of {MAX_PDF_PAGES_PER_REQUEST} pages per request."
                        )),
                    ),
                ]));
            }
            Some(_) => {}
        }
    }
    if let Some(path) = field(input, "file_path").as_string() {
        let absolute = absolute_input(path, context);
        if is_blocked_device_path(&text(&absolute)) {
            issues.push(object([
                ("path", string("file_path")),
                ("code", string("invalid_schema")),
                (
                    "message",
                    string("device files that block or stream infinitely are not readable."),
                ),
            ]));
        }
        if has_binary_extension(&absolute) {
            issues.push(object([
                ("path", string("file_path")),
                ("code", string("invalid_schema")),
                (
                    "message",
                    string("binary files are not supported by read_file. Use the relevant document, spreadsheet, or presentation skill for Office files; convert archives or other binary files to a supported text, PDF, or image format before inspection. Use send_attachment/send_file only when the user wants this file sent back through the current channel."),
                ),
            ]));
        }
    }
    (!issues.is_empty()).then_some(issues)
}

fn execute(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let input_path = field(input, "file_path")
        .as_string()
        .cloned()
        .ok_or_else(|| tool_error("invalid_tool_input", "file_path must be a string."))?;
    let allow_outside = context
        .fields
        .get("currentPermissionDecision")
        .and_then(|decision| decision.get("type"))
        .and_then(JsValue::as_string)
        .is_some_and(|kind| kind.equals_str("allow"));
    let resolved = resolve_workspace_path(&input_path, context, true, allow_outside)?;
    let context_stat = stat(&resolved.absolute)?;
    let kind = classify(&resolved.absolute);
    let read_state_key = read_state_key(&resolved.absolute, kind, input);
    if read_state_get(context, &read_state_key)
        .as_ref()
        .and_then(|previous| number(field(previous, "mtimeMs")))
        == Some(context_stat.mtime_ms)
    {
        return Ok(ToolOutput {
            content: vec![JsValue::Object(object([
                ("type", string("text")),
                ("text", string(FILE_UNCHANGED_STUB)),
            ]))],
            supplemental_messages: None,
            data: Some(JsValue::Object(object([
                ("filePath", string(resolved.relative.clone())),
                ("kind", string(kind.as_str())),
                ("unchanged", JsValue::Bool(true)),
            ]))),
            metadata: Some(object([("unchanged", JsValue::Bool(true))])),
        });
    }

    match kind {
        ReadKind::Image => image(input, context, &resolved, &context_stat, &read_state_key),
        ReadKind::Pdf => pdf(input, context, &resolved, &context_stat, &read_state_key),
        ReadKind::Notebook => notebook(input, context, &resolved, &context_stat, &read_state_key),
        ReadKind::Text => text_file(input, context, &resolved, &context_stat, &read_state_key),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadKind {
    Text,
    Image,
    Pdf,
    Notebook,
}
impl ReadKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Pdf => "pdf",
            Self::Notebook => "notebook",
        }
    }
}

fn classify(path: &JsString) -> ReadKind {
    let extension = lower(&extname(path));
    if matches!(
        extension.as_str(),
        ".png" | ".jpg" | ".jpeg" | ".gif" | ".webp"
    ) {
        ReadKind::Image
    } else if extension == ".pdf" {
        ReadKind::Pdf
    } else if extension == ".ipynb" {
        ReadKind::Notebook
    } else {
        ReadKind::Text
    }
}

fn image_mime(path: &JsString) -> Option<&'static str> {
    match lower(&extname(path)).as_str() {
        ".png" => Some("image/png"),
        ".jpg" | ".jpeg" => Some("image/jpeg"),
        ".gif" => Some("image/gif"),
        ".webp" => Some("image/webp"),
        _ => None,
    }
}

fn has_binary_extension(path: &JsString) -> bool {
    let extension = lower(&extname(path));
    BINARY_EXTENSIONS.contains(&extension.as_str())
        && !matches!(
            extension.as_str(),
            ".png" | ".jpg" | ".jpeg" | ".gif" | ".webp" | ".pdf"
        )
}

fn is_blocked_device_path(path: &str) -> bool {
    if BLOCKED_DEVICE_PATHS.contains(&path) {
        return true;
    }
    if path.starts_with("/proc/")
        && (path.ends_with("/fd/0") || path.ends_with("/fd/1") || path.ends_with("/fd/2"))
    {
        return true;
    }
    if path.starts_with("\\\\.\\") || path.starts_with("\\\\?\\") {
        let device = path[4..]
            .split(['\\', '/'])
            .next()
            .unwrap_or_default()
            .to_uppercase();
        if device.starts_with("PHYSICALDRIVE")
            || (device.len() == 2
                && device.ends_with(':')
                && device.as_bytes()[0].is_ascii_uppercase())
            || WINDOWS_DEVICE_NAMES.contains(&device.as_str())
        {
            return true;
        }
    }
    let base = super::basename(&JsString::from(path));
    let name = text(&base);
    let stem = name
        .rsplit_once('.')
        .map_or(name.as_str(), |(stem, _)| stem);
    WINDOWS_DEVICE_NAMES.contains(&stem.to_uppercase().as_str())
}

fn parse_pdf_page_range(value: &str) -> Option<(u64, u64)> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (first, last) = match trimmed.split_once('-') {
        Some((first, last)) => (first.trim(), last.trim()),
        None => (trimmed, trimmed),
    };
    if first.is_empty()
        || last.is_empty()
        || !first.bytes().all(|b| b.is_ascii_digit())
        || !last.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let first: u64 = first.parse().ok()?;
    let last: u64 = last.parse().ok()?;
    if first < 1 || last < first {
        return None;
    }
    Some((first, last))
}

fn absolute_input(path: &JsString, context: &ToolRunContext) -> JsString {
    if is_absolute(path) {
        normalize(path)
    } else {
        join(&context_cwd(context), path)
    }
}

fn read_state_key(path: &JsString, kind: ReadKind, input: &JsValue) -> JsString {
    // Path identity stays in the original UTF-16 units; key assembly never
    // round-trips through a lossy `String`.
    let offset = number(field(input, "offset"))
        .and_then(|value| js_value_string(&JsValue::Number(value)).ok())
        .unwrap_or_else(|| JsString::from("1"));
    let limit = number(field(input, "limit"))
        .and_then(|value| js_value_string(&JsValue::Number(value)).ok())
        .unwrap_or_else(|| JsString::from("all"));
    let pages = field(input, "pages")
        .as_string()
        .cloned()
        .unwrap_or_else(|| JsString::from(""));
    let mut units = path.units().to_vec();
    for part in [
        JsString::from("::"),
        JsString::from(kind.as_str()),
        JsString::from("::"),
        offset,
        JsString::from("::"),
        limit,
        JsString::from("::"),
    ] {
        units.extend_from_slice(part.units());
    }
    units.extend_from_slice(pages.units());
    JsString::from_utf16(units)
}

fn read_state_get(context: &ToolRunContext, key: &JsString) -> Option<JsValue> {
    context
        .host
        .files
        .read_file_state
        .lock()
        .unwrap()
        .get_key(key)
        .cloned()
}

fn read_state_set(context: &ToolRunContext, key: JsString, value: JsValue) {
    context
        .host
        .files
        .read_file_state
        .lock()
        .unwrap()
        .insert(key, value);
}

fn image(
    _input: &JsValue,
    context: &ToolRunContext,
    resolved: &ResolvedPath,
    context_stat: &super::Stat,
    _read_state_key: &JsString,
) -> Result<ToolOutput, ToolError> {
    let Some(mime_type) = image_mime(&resolved.absolute) else {
        return Err(tool_error(
            "invalid_tool_input",
            format!("Unsupported image type: {}.", text(&resolved.relative)),
        ));
    };
    let multimodal = context.fields.get("modelMultimodal");
    let supports_image = multimodal
        .and_then(|value| value.get("input"))
        .map(|value| match value {
            JsValue::Array(items) => items.iter().any(|item| {
                item.as_string()
                    .is_some_and(|value| value.equals_str("image"))
            }),
            _ => false,
        })
        .unwrap_or(false);
    if !supports_image {
        return Ok(ToolOutput {
            content: vec![JsValue::Object(object([
                ("type", string("text")),
                (
                    "text",
                    string(format!(
                        "[Image file: {}, {} bytes, {mime_type}. Current model does not support image input.]",
                        text(&resolved.relative),
                        context_stat.size
                    )),
                ),
            ]))],
            data: Some(JsValue::Object(object([
                ("filePath", string(resolved.relative.clone())),
                ("kind", string("image")),
                ("modelSupportsImage", JsValue::Bool(false)),
            ]))),
            ..ToolOutput::default()
        });
    }
    // The baseline attaches model-visible pixels only after decoding and a
    // multi-pass sharp compression. That encoder is not part of the gateway's
    // dependency set, so report the same recoverable decode-failure notice
    // instead of fabricating image bytes.
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            (
                "text",
                string(format!(
                    "[Image file: {}, {} bytes, {mime_type}. Image decoding failed, so it was not attached as model-visible image content. Diagnostic: image processing is not available in this runtime.]",
                    text(&resolved.relative),
                    context_stat.size
                )),
            ),
        ]))],
        data: Some(JsValue::Object(object([
            ("filePath", string(resolved.relative.clone())),
            ("kind", string("image")),
            ("mimeType", string(mime_type)),
            ("bytes", JsValue::Number(context_stat.size as f64)),
            ("imageDecodeFailed", JsValue::Bool(true)),
            (
                "error",
                string("image processing is not available in this runtime"),
            ),
        ]))),
        ..ToolOutput::default()
    })
}

fn pdf(
    input: &JsValue,
    context: &ToolRunContext,
    resolved: &ResolvedPath,
    context_stat: &super::Stat,
    read_state_key: &JsString,
) -> Result<ToolOutput, ToolError> {
    let _ = read_state_key;
    let multimodal = context.fields.get("modelMultimodal");
    let supports = |kind: &str| {
        multimodal
            .and_then(|value| value.get("input"))
            .map(|value| match value {
                JsValue::Array(items) => items
                    .iter()
                    .any(|item| item.as_string().is_some_and(|value| value.equals_str(kind))),
                _ => false,
            })
            .unwrap_or(false)
    };
    let supports_pdf = supports("pdf");
    let supports_image = supports("image");
    let pages = field(input, "pages")
        .as_string()
        .map(text)
        .filter(|value| !value.trim().is_empty());
    if let Some(value) = &pages
        && parse_pdf_page_range(value).is_none()
    {
        return Err(tool_error(
            "invalid_tool_input",
            format!("Invalid PDF page range: {value}."),
        ));
    }
    let page_suffix = "";
    if let Some(_value) = &pages {
        if !supports_image {
            return Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", string("text")),
                    (
                        "text",
                        string(format!(
                            "[PDF file: {}, {} bytes{page_suffix}. Current model does not support image input; cannot render requested pages.]",
                            text(&resolved.relative),
                            context_stat.size
                        )),
                    ),
                ]))],
                data: Some(JsValue::Object(object([
                    ("filePath", string(resolved.relative.clone())),
                    ("kind", string("pdf")),
                    ("modelSupportsImage", JsValue::Bool(false)),
                ]))),
                ..ToolOutput::default()
            });
        }
        return Err(tool_error(
            "result_too_large",
            "PDF page rendering is not available in this runtime.",
        ));
    }
    if !supports_pdf || context_stat.size > 3 * 1024 * 1024 {
        if !supports_image {
            return Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", string("text")),
                    (
                        "text",
                        string(format!(
                            "[PDF file: {}, {} bytes{page_suffix}. Current model does not support PDF input or image input.]",
                            text(&resolved.relative),
                            context_stat.size
                        )),
                    ),
                ]))],
                data: Some(JsValue::Object(object([
                    ("filePath", string(resolved.relative.clone())),
                    ("kind", string("pdf")),
                    ("modelSupportsPdf", JsValue::Bool(false)),
                    ("modelSupportsImage", JsValue::Bool(false)),
                ]))),
                ..ToolOutput::default()
            });
        }
        return Err(tool_error(
            "result_too_large",
            "PDF page rendering is not available in this runtime.",
        ));
    }
    Err(tool_error(
        "result_too_large",
        "PDF document blocks are not available in this runtime.",
    ))
}

fn notebook(
    input: &JsValue,
    context: &ToolRunContext,
    resolved: &ResolvedPath,
    context_stat: &super::Stat,
    read_state_key: &JsString,
) -> Result<ToolOutput, ToolError> {
    let bytes = std::fs::read(os_path(&resolved.absolute))
        .map_err(|error| failed(error, "open", &resolved.absolute))?;
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let parsed = pilotdeck_protocol::parse_json(&raw).map_err(|error| {
        let mut tool = tool_error(
            "invalid_tool_input",
            format!("Notebook {} is not valid JSON.", text(&resolved.absolute)),
        );
        tool.details = Some(object([("cause", string(error.to_string()))]));
        tool
    })?;
    let notebook = parsed.as_object();
    let cells: &[JsValue] = notebook
        .and_then(|value| value.get("cells"))
        .and_then(|value| match value {
            JsValue::Array(cells) => Some(cells.as_slice()),
            _ => None,
        })
        .unwrap_or_default();
    let text = render_notebook(cells)?;
    let offset = number(field(input, "offset")).unwrap_or(1.0);
    let limit = number(field(input, "limit"));
    let ranged = slice_rendered_text(&text, offset, limit);
    let numbered = render_numbered_lines(&ranged.lines, ranged.start_line);
    ensure_token_budget(&numbered, &resolved.relative, None)?;
    read_state_set(
        context,
        read_state_key.clone(),
        read_state_value(context_stat.mtime_ms, ReadKind::Notebook, input),
    );
    record_write_snapshot(
        context,
        &resolved.absolute,
        &raw,
        context_stat.mtime_ms,
        number(field(input, "offset")),
        number(field(input, "limit")),
    );
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(numbered)),
        ]))],
        data: Some(JsValue::Object(object([
            ("filePath", string(resolved.relative.clone())),
            ("kind", string("notebook")),
            ("startLine", JsValue::Number(ranged.start_line as f64)),
            ("endLine", JsValue::Number(ranged.end_line as f64)),
            ("totalLines", JsValue::Number(ranged.total_lines as f64)),
            ("truncated", JsValue::Bool(ranged.truncated)),
            ("cellCount", JsValue::Number(cells.len() as f64)),
        ]))),
        metadata: Some(object([("truncated", JsValue::Bool(ranged.truncated))])),
        supplemental_messages: None,
    })
}

fn text_file(
    input: &JsValue,
    context: &ToolRunContext,
    resolved: &ResolvedPath,
    context_stat: &super::Stat,
    read_state_key: &JsString,
) -> Result<ToolOutput, ToolError> {
    let offset = number(field(input, "offset")).unwrap_or(1.0);
    let input_limit = number(field(input, "limit"));
    let effective_limit = input_limit.or_else(|| {
        (context_stat.size as usize > LARGE_TEXT_AUTO_PAGE_BYTES)
            .then_some(DEFAULT_LARGE_TEXT_PREVIEW_LINES as f64)
    });
    let mut ranged = read_in_range(
        &resolved.absolute,
        offset,
        effective_limit,
        context_stat.size,
    )?;
    let mut rendered =
        render_readable_range(&ranged.content, ranged.start_line, ranged.total_lines);
    let mut auto_paged = input_limit.is_none() && effective_limit.is_some();
    let mut tool_result_ref_auto_paged = false;
    if auto_paged {
        while is_over_text_budget(&rendered) && ranged.line_count > 1 {
            let next_limit = ((ranged.line_count / 2) as f64).floor().max(1.0);
            ranged = read_in_range(
                &resolved.absolute,
                offset,
                Some(next_limit),
                context_stat.size,
            )?;
            rendered =
                render_readable_range(&ranged.content, ranged.start_line, ranged.total_lines);
        }
    }
    let managed_ref = is_managed_tool_result_ref(&resolved.relative);
    if !auto_paged && managed_ref {
        while is_over_text_budget(&rendered) && ranged.line_count > 1 {
            let next_limit = ((ranged.line_count / 2) as f64).floor().max(1.0);
            ranged = read_in_range(
                &resolved.absolute,
                offset,
                Some(next_limit),
                context_stat.size,
            )?;
            rendered =
                render_readable_range(&ranged.content, ranged.start_line, ranged.total_lines);
            tool_result_ref_auto_paged = true;
        }
    }
    if is_over_text_budget(&rendered) && input_limit.is_none() {
        auto_paged = true;
        rendered = render_oversized_line_preview(&rendered, &resolved.relative, ranged.start_line)?;
    }
    if is_over_text_budget(&rendered) && tool_result_ref_auto_paged {
        rendered = render_oversized_line_preview(&rendered, &resolved.relative, ranged.start_line)?;
    }
    ensure_token_budget(&rendered, &resolved.relative, Some(ranged.start_line))?;
    if auto_paged && ranged.truncated {
        rendered.push_str(&render_read_more_notice(
            &resolved.relative,
            ranged.end_line + 1,
            ranged.line_count,
        ));
    }
    if tool_result_ref_auto_paged && ranged.truncated {
        rendered.push_str(&render_tool_result_ref_read_more_notice(
            &resolved.relative,
            ranged.end_line + 1,
            ranged.line_count,
        ));
    }
    read_state_set(
        context,
        read_state_key.clone(),
        read_state_value(ranged.mtime_ms, ReadKind::Text, input),
    );
    let snapshot_limit = input_limit.or(if ranged.full_content.is_none() {
        Some(ranged.line_count as f64)
    } else {
        None
    });
    record_write_snapshot(
        context,
        &resolved.absolute,
        ranged.full_content.as_deref().unwrap_or(&ranged.content),
        ranged.mtime_ms,
        number(field(input, "offset")),
        snapshot_limit,
    );
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(rendered)),
        ]))],
        data: Some(JsValue::Object(object([
            ("filePath", string(resolved.relative.clone())),
            ("kind", string("text")),
            ("startLine", JsValue::Number(ranged.start_line as f64)),
            ("endLine", JsValue::Number(ranged.end_line as f64)),
            ("totalLines", JsValue::Number(ranged.total_lines as f64)),
            ("truncated", JsValue::Bool(ranged.truncated)),
            (
                "autoPaged",
                JsValue::Bool(auto_paged || tool_result_ref_auto_paged),
            ),
            (
                "nextOffset",
                if ranged.truncated {
                    JsValue::Number((ranged.end_line + 1) as f64)
                } else {
                    JsValue::Undefined
                },
            ),
        ]))),
        metadata: Some(object([("truncated", JsValue::Bool(ranged.truncated))])),
        supplemental_messages: None,
    })
}

fn read_state_value(mtime_ms: f64, kind: ReadKind, input: &JsValue) -> JsValue {
    let mut state = object([
        ("mtimeMs", JsValue::Number(mtime_ms)),
        ("kind", string(kind.as_str())),
    ]);
    if let Some(offset) = number(field(input, "offset")) {
        state.insert("offset".into(), JsValue::Number(offset));
    }
    if let Some(limit) = number(field(input, "limit")) {
        state.insert("limit".into(), JsValue::Number(limit));
    }
    if let Some(pages) = field(input, "pages").as_string() {
        state.insert("pages".into(), JsValue::String(pages.clone()));
    }
    JsValue::Object(state)
}

fn record_write_snapshot(
    context: &ToolRunContext,
    absolute: &JsString,
    content: &str,
    mtime_ms: f64,
    offset: Option<f64>,
    limit: Option<f64>,
) {
    let hash = format!("{:x}", Sha256::digest(content.as_bytes()));
    let mut entry = object([
        ("absolutePath", string(absolute.clone())),
        ("mtimeMs", JsValue::Number(mtime_ms)),
        ("contentHash", string(hash)),
    ]);
    if let Some(offset) = offset {
        entry.insert("offset".into(), JsValue::Number(offset));
    }
    if let Some(limit) = limit {
        entry.insert("limit".into(), JsValue::Number(limit));
    }
    context
        .host
        .files
        .write_snapshots
        .lock()
        .unwrap()
        .insert(absolute.clone(), JsValue::Object(entry));
}

struct Ranged {
    content: String,
    full_content: Option<String>,
    line_count: usize,
    total_lines: usize,
    start_line: usize,
    end_line: usize,
    truncated: bool,
    mtime_ms: f64,
}

fn read_in_range(
    path: &JsString,
    start_line: f64,
    limit: Option<f64>,
    _total_bytes: u64,
) -> Result<Ranged, ToolError> {
    let file_stat = stat(path)?;
    if !matches!(file_stat.kind, StatKind::File) {
        return Err(tool_error(
            "file_conflict",
            format!("{} is not a regular file.", text(path)),
        ));
    }
    let bytes = std::fs::read(os_path(path)).map_err(|error| failed(error, "open", path))?;
    if limit.is_none() {
        if bytes.contains(&0) {
            return Err(tool_error(
                "invalid_tool_input",
                format!("{} appears to be a binary file.", text(path)),
            ));
        }
        let decoded = String::from_utf8_lossy(&bytes).into_owned();
        let decoded = strip_bom(&decoded);
        let lines = split_lf(decoded);
        let normalized_start = start_line.max(1.0) as usize;
        let start_index = normalized_start - 1;
        let selected: Vec<&str> = if start_index >= lines.len() {
            Vec::new()
        } else {
            lines[start_index..].to_vec()
        };
        let content = selected.join("\n");
        let (actual_start, actual_end) =
            actual_range(normalized_start, selected.len(), lines.len());
        return Ok(Ranged {
            content,
            full_content: Some(decoded.to_owned()),
            line_count: selected.len(),
            total_lines: lines.len(),
            start_line: actual_start,
            end_line: actual_end,
            truncated: start_index > 0,
            mtime_ms: file_stat.mtime_ms,
        });
    }
    if bytes.contains(&0) {
        return Err(tool_error(
            "invalid_tool_input",
            format!("{} appears to be a binary file.", text(path)),
        ));
    }
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    let lines = readline_lines(&decoded);
    let normalized_start = start_line.max(1.0) as usize;
    let normalized_limit = limit.unwrap_or(0.0).max(0.0) as usize;
    let start_index = normalized_start - 1;
    let end_index_exclusive = start_index.saturating_add(normalized_limit);
    let selected: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(index, _)| *index >= start_index && *index < end_index_exclusive)
        .map(|(_, line)| line.as_str())
        .collect();
    let content = selected.join("\n");
    let (actual_start, actual_end) = actual_range(normalized_start, selected.len(), lines.len());
    Ok(Ranged {
        content,
        full_content: None,
        line_count: selected.len(),
        total_lines: lines.len(),
        start_line: actual_start,
        end_line: actual_end,
        truncated: start_index > 0 || end_index_exclusive < lines.len(),
        mtime_ms: file_stat.mtime_ms,
    })
}

fn actual_range(normalized_start: usize, selected: usize, total: usize) -> (usize, usize) {
    if selected > 0 {
        (normalized_start, normalized_start + selected - 1)
    } else {
        let start = normalized_start.min(total + 1);
        (start, start.saturating_sub(1))
    }
}

fn strip_bom(value: &str) -> &str {
    value.strip_prefix('\u{FEFF}').unwrap_or(value)
}

fn split_lf(value: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = value.split('\n').collect();
    for line in &mut lines {
        if let Some(stripped) = line.strip_suffix('\r') {
            *line = stripped;
        }
    }
    lines
}

/// Node readline with `crlfDelay: Infinity`: `\r\n`, `\r` and `\n` each end a
/// line, and a final terminator does not produce a trailing empty line.
fn readline_lines(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push(std::mem::take(&mut current));
            }
            '\n' => lines.push(std::mem::take(&mut current)),
            character => current.push(character),
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if let Some(first) = lines.first_mut()
        && let Some(stripped) = first.strip_prefix('\u{FEFF}')
    {
        *first = stripped.to_owned();
    }
    lines
}

fn render_readable_range(content: &str, start_line: usize, total_lines: usize) -> String {
    if !content.is_empty() {
        render_numbered_lines(&content.split('\n').collect::<Vec<_>>(), start_line)
    } else if total_lines == 0 {
        "<system-reminder>Warning: the file exists but the contents are empty.</system-reminder>"
            .into()
    } else {
        format!(
            "<system-reminder>Warning: the file exists but is shorter than the provided offset ({start_line}). The file has {total_lines} lines.</system-reminder>"
        )
    }
}

fn render_numbered_lines(lines: &[impl AsRef<str>], start_line: usize) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{}|{}", start_line + index, line.as_ref()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_read_more_notice(file_path: &JsString, next_offset: usize, limit: usize) -> String {
    format!(
        "\n<system-reminder>The file is too large to read in one response, so read_file returned the first {limit} lines. Continue with read_file({{ file_path: \"{}\", offset: {next_offset}, limit: {limit} }}) if you need more.</system-reminder>",
        text(file_path)
    )
}

fn render_tool_result_ref_read_more_notice(
    file_path: &JsString,
    next_offset: usize,
    limit: usize,
) -> String {
    format!(
        "\n<system-reminder>The persisted tool result was too large for the requested range, so read_file returned {limit} lines. Continue with read_file({{ file_path: \"{}\", offset: {next_offset}, limit: {limit} }}) if you need more.</system-reminder>",
        text(file_path)
    )
}

fn is_managed_tool_result_ref(path: &JsString) -> bool {
    let value = text(path);
    let Some(rest) = value
        .strip_prefix(".pilotdeck/")
        .or_else(|| value.strip_prefix(".pilotdeck\\"))
    else {
        return false;
    };
    let Some(rest) = rest
        .strip_prefix("tool-results/")
        .or_else(|| rest.strip_prefix("tool-results\\"))
    else {
        return false;
    };
    let Some(rest) = rest
        .strip_prefix("refs/")
        .or_else(|| rest.strip_prefix("refs\\"))
    else {
        return false;
    };
    let Some(number) = rest.strip_prefix("result-") else {
        return false;
    };
    let Some((digits, extension)) = number.rsplit_once('.') else {
        return false;
    };
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(extension, "txt" | "json")
}

fn render_oversized_line_preview(
    rendered: &str,
    file_path: &JsString,
    offset: usize,
) -> Result<String, ToolError> {
    let limited = apply_result_size_limit(rendered, OVERSIZED_LINE_PREVIEW_BYTES);
    Ok(format!(
        "{limited}\n<system-reminder>This line range is still too large for the model context, so read_file returned a head/tail preview. Use a smaller line range, for example read_file({{ file_path: \"{}\", offset: {offset}, limit: 1 }}), or use grep to find the relevant section.</system-reminder>",
        text_of(file_path)
    ))
}

fn text_of(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

/// JS `String(value)` for notebook renderings; an over-deep conversion keeps
/// the successful outer tool result and prints the RangeError message.
fn js_text(value: &JsValue) -> Result<String, ToolError> {
    js_value_string(value).map(|value| text_of(&value))
}

fn is_over_text_budget(value: &str) -> bool {
    let bytes = value.len();
    if bytes > LARGE_TEXT_AUTO_PAGE_BYTES {
        return true;
    }
    if bytes <= SAFE_TEXT_BUDGET_BYTES {
        return false;
    }
    pilotdeck_tokenizer::count_tokens(&JsString::from(value))
        .map(|tokens| tokens > MAX_TEXT_TOKENS)
        .unwrap_or(true)
}

fn ensure_token_budget(
    value: &str,
    file_path: &JsString,
    suggested_offset: Option<usize>,
) -> Result<(), ToolError> {
    if !is_over_text_budget(value) {
        return Ok(());
    }
    let action = match suggested_offset {
        None => "Use offset and limit to read a smaller portion.".to_owned(),
        Some(offset) => format!(
            "Use a smaller limit, for example read_file({{ file_path: \"{}\", offset: {offset}, limit: 500 }}).",
            text(file_path)
        ),
    };
    Err(tool_error(
        "result_too_large",
        format!(
            "File content from {} exceeds the text token budget. {action}",
            text(file_path)
        ),
    ))
}

fn apply_result_size_limit(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let suffix = "\n[Tool output truncated: head and tail shown.]";
    let budget = max_bytes.saturating_sub(suffix.len());
    format!("{}{suffix}", head_tail_truncate_utf8(value, budget))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn truncate_utf8_from_end(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_owned()
}

fn head_tail_truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    if max_bytes <= 64 {
        return truncate_utf8(value, max_bytes);
    }
    let separator = "\n... [middle omitted] ...\n";
    let content_budget = max_bytes.saturating_sub(separator.len());
    let head_budget = content_budget.div_ceil(2);
    let tail_budget = content_budget / 2;
    format!(
        "{}{separator}{}",
        truncate_utf8(value, head_budget),
        truncate_utf8_from_end(value, tail_budget)
    )
}

struct RenderedText {
    lines: Vec<String>,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    truncated: bool,
}

fn slice_rendered_text(value: &str, start_line: f64, limit: Option<f64>) -> RenderedText {
    let lines: Vec<String> = split_lf(value).into_iter().map(str::to_owned).collect();
    let start = start_line.max(1.0) as usize;
    let start_index = start.saturating_sub(1);
    let selected: Vec<String> = match limit {
        None => lines.iter().skip(start_index).cloned().collect(),
        Some(limit) => lines
            .iter()
            .skip(start_index)
            .take(limit.max(0.0) as usize)
            .cloned()
            .collect(),
    };
    let (actual_start, actual_end) = actual_range(start, selected.len(), lines.len());
    let truncated = start_index > 0
        || limit
            .is_some_and(|limit| start_index.saturating_add(limit.max(0.0) as usize) < lines.len());
    RenderedText {
        lines: selected,
        start_line: actual_start,
        end_line: actual_end,
        total_lines: lines.len(),
        truncated,
    }
}

fn render_notebook(cells: &[JsValue]) -> Result<String, ToolError> {
    let mut lines: Vec<String> = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        let cell = cell.as_object();
        let cell_type = cell
            .and_then(|cell| cell.get("cell_type"))
            .and_then(JsValue::as_string)
            .map(text_of)
            .unwrap_or_else(|| "unknown".into());
        lines.push(format!("# Cell {index} ({cell_type})"));
        if let Some(execution_count) = cell.and_then(|cell| cell.get("execution_count"))
            && !matches!(execution_count, JsValue::Undefined | JsValue::Null)
        {
            lines.push(format!("execution_count: {}", js_text(execution_count)?));
        }
        let source = normalize_multiline(cell.and_then(|cell| cell.get("source")))?;
        if !source.is_empty() {
            lines.push(source);
        }
        let outputs = format_outputs(cell.and_then(|cell| cell.get("outputs")))?;
        if !outputs.is_empty() {
            lines.push("## Outputs".into());
            lines.push(outputs);
        }
        if index < cells.len() - 1 {
            lines.push(String::new());
        }
    }
    Ok(lines.join("\n"))
}

fn format_outputs(outputs: Option<&JsValue>) -> Result<String, ToolError> {
    let mut chunks = Vec::new();
    let outputs: &[JsValue] = match outputs {
        Some(JsValue::Array(outputs)) => outputs,
        _ => &[],
    };
    for output in outputs {
        let Some(output) = output.as_object() else {
            return match output {
                JsValue::Null => Err(tool_error(
                    "tool_execution_failed",
                    "Cannot read properties of null (reading 'text')",
                )),
                JsValue::Undefined => Err(tool_error(
                    "tool_execution_failed",
                    "Cannot read properties of undefined (reading 'text')",
                )),
                _ => continue,
            };
        };
        let text_value = normalize_multiline(output.get("text"))?;
        if !text_value.is_empty() {
            chunks.push(text_value);
            continue;
        }
        if let Some(JsValue::Array(traceback)) = output.get("traceback")
            && !traceback.is_empty()
        {
            chunks.push(
                traceback
                    .iter()
                    .map(|value| match value {
                        JsValue::Null | JsValue::Undefined => Ok(String::new()),
                        _ => js_text(value),
                    })
                    .collect::<Result<Vec<_>, ToolError>>()?
                    .join("\n"),
            );
            continue;
        }
        let ename = output
            .get("ename")
            .filter(|value| truthy(value))
            .map(js_text)
            .transpose()?;
        let evalue = output
            .get("evalue")
            .filter(|value| truthy(value))
            .map(js_text)
            .transpose()?;
        if ename.is_some() || evalue.is_some() {
            let ename = ename.unwrap_or_else(|| "Error".into());
            let evalue = evalue.unwrap_or_default();
            chunks.push(format!("{ename}: {evalue}").trim().to_owned());
            continue;
        }
        if let Some(plain) = output.get("data").and_then(|data| data.get("text/plain")) {
            let plain = normalize_multiline(Some(plain))?;
            if !plain.is_empty() {
                chunks.push(plain);
            }
        }
    }
    Ok(chunks.join("\n"))
}

fn normalize_multiline(value: Option<&JsValue>) -> Result<String, ToolError> {
    match value {
        Some(JsValue::Array(values)) => values
            .iter()
            .map(|value| match value {
                JsValue::Undefined | JsValue::Null => Ok(String::new()),
                _ => js_text(value),
            })
            .collect(),
        Some(JsValue::String(value)) => Ok(text_of(value)),
        _ => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_state_key_preserves_original_utf16_path_identity() {
        let path = JsString::from_utf16(vec![
            b'/' as u16,
            b't' as u16,
            0xd800,
            b'/' as u16,
            b'a' as u16,
        ]);
        let input = JsValue::Object(object([("offset", JsValue::Number(2.0))]));
        let key = read_state_key(&path, ReadKind::Text, &input);
        assert_eq!(&key.units()[..path.units().len()], path.units());
        assert!(key.units().contains(&0xd800), "lone surrogate survives");
    }
}
