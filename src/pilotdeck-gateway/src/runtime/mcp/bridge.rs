//! `PluginToToolBridge`, ported from the fixed TypeScript baseline's
//! `src/mcp/runtime/PluginToToolBridge.ts`: every tool an [`McpRuntimePort`]
//! advertises becomes an [`ExecutableTool`] (M10 wire name, M11 truncated
//! description, M12 annotation flags) whose `execute` calls the runtime and
//! projects the result (M14):
//!
//! - `text` and `image` blocks map to PilotDeck `text`/`image` content;
//!   every other block is collected into one trailing `json` block;
//! - with a stdio `cwd` and no inline image, Markdown links to local
//!   `./….png|jpg|jpeg|gif|webp` files are read and inlined as base64;
//!   local reads are limited to regular files within the real cwd or explicit
//!   image roots, up to 5 MiB (A6.5-MCP-IMAGE-READ-SANDBOX-001, human-approved);
//! - `isError: true` fails with the joined text blocks; client errors become
//!   `tool_execution_failed` with `details.errorCode`.
//!
//! `isDestructive` / `isOpenWorld` have no slot in the native tool trait yet;
//! they are exposed as inherent methods for the permission layer.

use super::client::{McpAbort, McpAbortSignal, McpCallOptions, McpErrorCode};
use super::protocol::McpToolSpec;
use super::runtime::McpRuntimePort;
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use base64::Engine;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fs::{File, Metadata},
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

/// `maxResultBytes` of every MCP tool.
pub const MCP_TOOL_MAX_RESULT_BYTES: f64 = 200_000.0;

/// Match AttachmentResolver.ts's DEFAULT_MAX_IMAGE_BYTES (5 MiB), also for
/// files which grow after stat. Inline MCP image blocks are outside this policy.
pub const MCP_FILE_IMAGE_MAX_BYTES: u64 = 5_242_880;

/// `CreateToolDefinitionsOptions`.
#[derive(Clone, Debug, Default)]
pub struct CreateToolDefinitionsOptions {
    /// Per-call timeout override (default falls through to the client).
    pub call_timeout_ms: Option<f64>,
    /// Trusted host configuration, never taken from MCP result text. Empty by
    /// default. Absolute directories only; each is realpath'd on every call.
    /// Applies only to automatic local images from stdio servers with a cwd.
    pub image_read_allowed_roots: Vec<PathBuf>,
}

/// `createMcpToolDefinitionsFromRuntime`.
pub async fn create_mcp_tool_definitions_from_runtime(
    runtime: Arc<dyn McpRuntimePort>,
    options: CreateToolDefinitionsOptions,
) -> Result<Vec<Arc<McpBridgeTool>>, crate::runtime::project::RuntimeError> {
    Ok(runtime
        .list_all_tools()
        .await?
        .into_iter()
        .map(|spec| Arc::new(McpBridgeTool::new(spec, runtime.clone(), options.clone())))
        .collect())
}

/// One MCP tool as a native tool definition.
pub struct McpBridgeTool {
    fields: JsObject,
    spec: McpToolSpec,
    runtime: Arc<dyn McpRuntimePort>,
    options: CreateToolDefinitionsOptions,
    read_only: bool,
    destructive: bool,
    open_world: bool,
}

fn annotation(spec: &McpToolSpec, key: &str) -> Option<bool> {
    spec.annotations.as_ref()?.get(key)?.as_bool()
}

/// `normalizeSchema`: an object schema with `type: "object"` is kept as is.
fn normalize_schema(raw: &JsValue) -> JsValue {
    if let JsValue::Object(object) = raw
        && object
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|t| t.equals_str("object"))
    {
        return raw.clone();
    }
    let mut fallback = JsObject::new();
    fallback.insert("type".into(), "object".into());
    fallback.insert("additionalProperties".into(), JsValue::Bool(true));
    fallback.insert("properties".into(), JsValue::Object(JsObject::new()));
    JsValue::Object(fallback)
}

impl McpBridgeTool {
    pub fn new(
        spec: McpToolSpec,
        runtime: Arc<dyn McpRuntimePort>,
        options: CreateToolDefinitionsOptions,
    ) -> Self {
        let mut fields = JsObject::new();
        fields.insert("name".into(), JsValue::String(spec.wire_name.clone()));
        fields.insert(
            "description".into(),
            JsValue::String(spec.description.clone()),
        );
        fields.insert("kind".into(), "mcp".into());
        fields.insert("inputSchema".into(), normalize_schema(&spec.input_schema));
        fields.insert(
            "maxResultBytes".into(),
            JsValue::Number(MCP_TOOL_MAX_RESULT_BYTES),
        );
        Self {
            read_only: annotation(&spec, "readOnlyHint") == Some(true),
            destructive: annotation(&spec, "destructiveHint") == Some(true),
            open_world: annotation(&spec, "openWorldHint") != Some(false),
            fields,
            spec,
            runtime,
            options,
        }
    }

    pub fn spec(&self) -> &McpToolSpec {
        &self.spec
    }

    /// `isDestructive()`: `annotations.destructiveHint === true`.
    pub fn is_destructive(&self) -> bool {
        self.destructive
    }

    /// `isOpenWorld()`: `annotations.openWorldHint !== false`.
    pub fn is_open_world(&self) -> bool {
        self.open_world
    }

    #[allow(clippy::result_large_err)] // the ExecutableTool error type
    async fn run(&self, input: JsValue, context: ToolRunContext) -> Result<ToolOutput, ToolError> {
        let spec = &self.spec;
        let server_id = String::from_utf16_lossy(spec.server_id.units());
        let Some(server) = self.runtime.server_info(&server_id) else {
            return Err(ToolError::new(
                "unsupported_tool",
                format!("MCP server {server_id} is not registered"),
            ));
        };
        let options = McpCallOptions {
            timeout_ms: self.options.call_timeout_ms,
            signal: Some(McpAbortSignal::new(Arc::new(ContextSignal(context)))),
        };
        let result = self
            .runtime
            .call_tool(&server_id, &spec.tool_name, &input, options)
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let code = match error.code() {
                    Some(McpErrorCode::Client(code)) => JsValue::String(code.as_str().into()),
                    Some(McpErrorCode::Text(code) | McpErrorCode::Errno(code)) => {
                        JsValue::String((*code).into())
                    }
                    Some(McpErrorCode::Number(code)) => JsValue::Number(*code),
                    None => JsValue::String("mcp_call_failed".into()),
                };
                let mut details = JsObject::new();
                details.insert("errorCode".into(), code);
                let mut failure = ToolError::failed(error.message());
                failure.details = Some(details);
                return Err(failure);
            }
        };
        if result.is_error == Some(true) {
            let tool_name = String::from_utf16_lossy(spec.tool_name.units());
            let mut details = JsObject::new();
            details.insert("content".into(), result.content.clone());
            let mut failure =
                ToolError::failed(error_text(&result.content, &server_id, &tool_name));
            failure.details = Some(details);
            return Err(failure);
        }
        let cwd = (server.transport == "stdio")
            .then_some(server.cwd)
            .flatten();
        let mut mcp = JsObject::new();
        mcp.insert("serverId".into(), JsValue::String(spec.server_id.clone()));
        mcp.insert("toolName".into(), JsValue::String(spec.tool_name.clone()));
        mcp.insert("wireName".into(), JsValue::String(spec.wire_name.clone()));
        let mut metadata = JsObject::new();
        metadata.insert("mcp".into(), JsValue::Object(mcp));
        let raw = result.content;
        let (content, raw) = if let Some(cwd) = cwd {
            // Filesystem resolution/reads stay off the async executor.
            let roots = self.options.image_read_allowed_roots.clone();
            tokio::task::spawn_blocking(move || {
                let content = marshal_mcp_content_with_roots(&raw, Some(&cwd), &roots);
                (content, raw)
            })
            .await
            .map_err(|error| ToolError::failed(format!("MCP content task failed: {error}")))?
        } else {
            (marshal_mcp_content(&raw, None), raw)
        };
        Ok(ToolOutput {
            content,
            supplemental_messages: None,
            data: Some(raw),
            metadata: Some(metadata),
        })
    }
}

impl ToolDefinition for McpBridgeTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(self.read_only))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(self.read_only))
    }
}

impl ExecutableTool for McpBridgeTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let tool = Self {
            fields: self.fields.clone(),
            spec: self.spec.clone(),
            runtime: self.runtime.clone(),
            options: self.options.clone(),
            read_only: self.read_only,
            destructive: self.destructive,
            open_world: self.open_world,
        };
        Box::pin(async move { tool.run(input, context).await })
    }
}

/// `context.abortSignal` as the client's abort signal.
struct ContextSignal(ToolRunContext);
impl McpAbort for ContextSignal {
    fn is_aborted(&self) -> bool {
        self.0.is_aborted()
    }
    fn aborted(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(self.0.cancelled())
    }
}

fn block_type(block: &JsValue) -> Option<&JsString> {
    match block {
        JsValue::Object(object) => object.get("type").and_then(JsValue::as_string),
        _ => None,
    }
}

fn content_block(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}

/// `marshalMcpContent(raw, cwd)`.
pub fn marshal_mcp_content(raw: &JsValue, cwd: Option<&str>) -> Vec<JsValue> {
    marshal_mcp_content_with_roots(raw, cwd, &[])
}

fn marshal_mcp_content_with_roots(
    raw: &JsValue,
    cwd: Option<&str>,
    allowed_roots: &[PathBuf],
) -> Vec<JsValue> {
    let JsValue::Array(blocks) = raw else {
        return vec![content_block(vec![
            ("type", "json".into()),
            ("value", raw.clone()),
        ])];
    };
    let mut result = Vec::new();
    let mut remainder = Vec::new();
    let mut has_image = false;
    for block in blocks {
        let Some(kind) = block_type(block) else {
            remainder.push(block.clone());
            continue;
        };
        let text = block.get("text").and_then(JsValue::as_string);
        let data = block.get("data").and_then(JsValue::as_string);
        let mime = block.get("mimeType").and_then(JsValue::as_string);
        if kind.equals_str("text")
            && let Some(text) = text
        {
            result.push(content_block(vec![
                ("type", "text".into()),
                ("text", JsValue::String(text.clone())),
            ]));
        } else if kind.equals_str("image")
            && let (Some(data), Some(mime)) = (data, mime)
        {
            result.push(content_block(vec![
                ("type", "image".into()),
                ("mimeType", JsValue::String(mime.clone())),
                ("data", JsValue::String(data.clone())),
            ]));
            has_image = true;
        } else {
            remainder.push(block.clone());
        }
    }
    if !has_image && let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
        let policy = ImageReadPolicy::new(cwd, allowed_roots);
        for block in blocks {
            if block_type(block).is_some_and(|k| k.equals_str("text"))
                && let Some(text) = block.get("text").and_then(JsValue::as_string)
            {
                result.extend(extract_file_images(text, cwd, &policy));
            }
        }
    }
    if !remainder.is_empty() {
        result.push(content_block(vec![
            ("type", "json".into()),
            ("value", JsValue::Array(remainder)),
        ]));
    }
    if result.is_empty() {
        result.push(content_block(vec![
            ("type", "json".into()),
            ("value", raw.clone()),
        ]));
    }
    result
}

/// `/\[.*?\]\((\.[^)]*\.(?:png|jpe?g|gif|webp))\)/gi` (JS `.` stops at line
/// terminators).
static IMAGE_LINK: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\[[^\n\r\u{2028}\u{2029}]*?\]\((\.[^)]*\.(?:png|jpe?g|gif|webp))\)")
        .expect("image link pattern")
});

/// `path.resolve(cwd, rel)` (POSIX, lexical).
fn resolve_path(cwd: &str, relative: &str) -> PathBuf {
    crate::metadata::resolve_process_path(Path::new(cwd), Path::new(relative))
}

fn extract_file_images(text: &JsString, cwd: &str, policy: &ImageReadPolicy) -> Vec<JsValue> {
    let text = String::from_utf16_lossy(text.units());
    let mut images = Vec::new();
    for capture in IMAGE_LINK.captures_iter(&text) {
        let relative = &capture[1];
        let Some(bytes) = policy.read(&resolve_path(cwd, relative)) else {
            continue;
        };
        let ext = relative.rsplit('.').next().unwrap_or("png").to_lowercase();
        let mime = match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => "image/png",
        };
        images.push(content_block(vec![
            ("type", "image".into()),
            ("mimeType", mime.into()),
            (
                "data",
                JsValue::String(
                    base64::engine::general_purpose::STANDARD
                        .encode(bytes)
                        .into(),
                ),
            ),
        ]));
    }
    images
}

/// Canonical roots are directories; failed/relative whitelist entries grant
/// nothing. Path::starts_with compares components, never string prefixes.
struct ImageReadPolicy {
    roots: Vec<PathBuf>,
}

impl ImageReadPolicy {
    fn new(cwd: &str, allowed_roots: &[PathBuf]) -> Self {
        let roots = std::iter::once(Path::new(cwd))
            .chain(
                allowed_roots
                    .iter()
                    .filter(|p| p.is_absolute())
                    .map(PathBuf::as_path),
            )
            .filter_map(|p| p.canonicalize().ok())
            .filter(|p| p.is_dir())
            .collect();
        Self { roots }
    }

    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        self.read_with(path, crate::project_files::open_beneath, read_limited_image)
    }

    // The injected boundaries let tests prove rejected paths are neither opened
    // nor read, and exercise replacements/growth at the exact race boundaries.
    fn read_with(
        &self,
        path: &Path,
        open: impl FnOnce(&Path, &Path) -> io::Result<File>,
        read: impl FnOnce(File) -> io::Result<Vec<u8>>,
    ) -> Option<Vec<u8>> {
        let canonical = path.canonicalize().ok()?;
        let root = self.roots.iter().find(|root| canonical.starts_with(root))?;
        let before = std::fs::symlink_metadata(&canonical).ok()?;
        if !safe_image_metadata(&before) {
            return None;
        }
        // open_beneath walks ALL canonical components with NOFOLLOW and pins
        // directories by fd. NONBLOCK also prevents a raced-in FIFO blocking.
        let file = open(root, &canonical).ok()?;
        let after = file.metadata().ok()?;
        if !safe_image_metadata(&after)
            || before.dev() != after.dev()
            || before.ino() != after.ino()
        {
            return None;
        }
        read(file).ok()
    }
}

fn safe_image_metadata(metadata: &Metadata) -> bool {
    metadata.is_file() && metadata.len() <= MCP_FILE_IMAGE_MAX_BYTES
}

fn read_limited_image(file: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MCP_FILE_IMAGE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MCP_FILE_IMAGE_MAX_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

/// `extractMcpErrorText`.
fn error_text(content: &JsValue, server_id: &str, tool_name: &str) -> JsString {
    let fallback = || {
        JsString::from(format!(
            "MCP server {server_id}/{tool_name} returned isError"
        ))
    };
    let JsValue::Array(blocks) = content else {
        return fallback();
    };
    let texts: Vec<&JsString> = blocks
        .iter()
        .filter(|block| block_type(block).is_some_and(|k| k.equals_str("text")))
        .filter_map(|block| block.get("text").and_then(JsValue::as_string))
        .collect();
    if texts.is_empty() {
        return fallback();
    }
    let mut joined = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        if index > 0 {
            joined.push(u16::from(b'\n'));
        }
        joined.extend_from_slice(text.units());
    }
    JsString::from_utf16(joined)
}

#[cfg(test)]
#[path = "bridge/tests.rs"]
mod tests;
