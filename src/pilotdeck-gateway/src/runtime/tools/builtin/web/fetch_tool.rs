//! `web_fetch` (baseline `tool/builtin/webFetch.ts` + `web/secondaryPrompt.ts`).
//!
//! The secondary model is injected: [`WebFetchToolOptions::model`] first,
//! else a [`WebFetchModelPort`] stored in the tool context's `model` slot,
//! the counterpart of `options.model ?? context.model`. Without either,
//! `mode: "llm"` fails with `unsupported_tool` and the `mode: "raw"` hint.
use super::{
    super::{field, object, schema, string},
    ask_permission,
    fetcher::{
        FetchError, FetchResult, MAX_MARKDOWN_LENGTH, UrlFetcher, WebFetchHttpError,
        truncate_markdown,
    },
    js_length,
    preapproved_hosts::is_preapproved_url,
    url_validation::validate_url,
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    js_value,
    runtime::{
        permission::{PermissionContext, PermissionFuture, PermissionTool},
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin, sync::Arc};

pub const WEB_FETCH_DESCRIPTION: &str = "- Fetches content from a specified URL and can process it using a secondary AI model\n- Takes a URL and a prompt as input\n- Fetches the URL content, converts HTML to markdown\n- If a model client is available, applies the prompt to the fetched content using a secondary model call\n- Without a model client, returns the fetched markdown content directly\n- Use this tool when you need to retrieve and analyze web content\n\nUsage notes:\n  - IMPORTANT: If an MCP-provided web fetch tool is available, prefer using that tool instead of this one, as it may have fewer restrictions.\n  - The URL must be a fully-formed valid URL\n  - HTTP URLs will be automatically upgraded to HTTPS\n  - The prompt should describe what information you want to extract from the page when mode is \"llm\"\n  - Use mode \"raw\" for exact, full-text, verification, or copy-the-source tasks where the fetched markdown must not be summarized or rewritten\n  - Use the default mode \"llm\" for summary, question-answering, and analysis tasks over a page\n  - This tool is read-only and does not modify any files\n  - Fetched content may be truncated and model responses may be summarized if the page is very large\n  - Includes a self-cleaning 15-minute cache for faster responses when repeatedly accessing the same URL\n  - When a URL redirects to a different host, the tool will inform you and provide the redirect URL in a special format. You should then make a new web_fetch request with the redirect URL to fetch the content.\n  - For GitHub URLs, prefer using the gh CLI via Bash instead (e.g., gh pr view, gh issue view, gh api).";

const INPUT_SCHEMA: &str = r#"{"type":"object","required":["url"],"additionalProperties":false,"properties":{"url":{"type":"string","description":"Fully-formed URL to fetch. HTTP URLs will be upgraded to HTTPS before the request is issued."},"prompt":{"type":"string","description":"Question or extraction directive to apply to the fetched markdown. When no model client is available, retry with mode \"raw\" to fetch markdown without secondary model processing."},"mode":{"type":"string","enum":["llm","raw"],"description":"Fetch mode. \"llm\" (default) applies prompt with the secondary model. \"raw\" returns fetched markdown directly without any model processing."}}}"#;

const DEFAULT_PROVIDER: &str = "openrouter";
const DEFAULT_MODEL_ID: &str = "moonshotai/kimi-k2.6";
const DEFAULT_MAX_OUTPUT_TOKENS: f64 = 65_536.0;

/// One item of `model.stream(request, signal)`.
#[derive(Clone, Debug)]
pub enum SecondaryModelEvent {
    /// A yielded event (`text_delta`, `usage`, `error`, ...).
    Event(JsValue),
    /// The iteration threw. `is_error` is `value instanceof Error`; an
    /// Error's own properties (`message`, `code`, ...) are in `value`.
    Throw { is_error: bool, value: JsValue },
}

pub type SecondaryStream = Pin<Box<dyn Future<Output = Vec<SecondaryModelEvent>> + Send>>;

/// `PilotDeckToolModelClient.stream` for the secondary call.
pub trait SecondaryModel: Send + Sync {
    fn stream(&self, request: JsValue) -> SecondaryStream;
}

/// What a host stores in `ToolRuntimeContext::model` for `web_fetch`.
pub struct WebFetchModelPort(pub Arc<dyn SecondaryModel>);

/// `CreateWebFetchToolOptions`.
#[derive(Clone)]
pub struct WebFetchToolOptions {
    pub model: Option<Arc<dyn SecondaryModel>>,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub max_output_tokens: Option<f64>,
    pub fetcher: UrlFetcher,
}

pub struct WebFetchTool {
    fields: JsObject,
    name: JsString,
    kind: JsString,
    options: WebFetchToolOptions,
}

impl WebFetchTool {
    pub fn new(options: WebFetchToolOptions) -> Self {
        Self {
            fields: object([
                ("name", string("web_fetch")),
                ("aliases", JsValue::Array(vec![string("WebFetch")])),
                ("description", string(WEB_FETCH_DESCRIPTION)),
                ("kind", string("network")),
                ("inputSchema", JsValue::Object(schema(INPUT_SCHEMA))),
                ("maxResultBytes", JsValue::Number(200_000.0)),
            ]),
            name: JsString::from("web_fetch"),
            kind: JsString::from("network"),
            options,
        }
    }
}

impl ToolDefinition for WebFetchTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}

/// `checkPermissions()`.
pub fn web_fetch_permission() -> JsValue {
    ask_permission(
        "web_fetch",
        "Network fetch requires permission.",
        "web fetch",
        "Allow fetch",
    )
}

impl PermissionTool for WebFetchTool {
    fn name(&self) -> &JsString {
        &self.name
    }
    fn kind(&self) -> Option<&JsString> {
        Some(&self.kind)
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn check_permissions<'a>(
        &'a self,
        _input: &'a JsValue,
        _context: &'a PermissionContext,
    ) -> PermissionFuture<'a, Option<JsValue>> {
        Box::pin(async { Ok(Some(web_fetch_permission())) })
    }
}

fn issue(path: &str, code: &str, message: &str) -> JsObject {
    object([
        ("path", string(path)),
        ("code", string(code)),
        ("message", string(message)),
    ])
}

fn non_empty_string(value: &JsValue) -> bool {
    value.as_string().is_some_and(|text| !text.is_empty())
}

/// `validateInput(input)`; `None` is `{ ok: true }`.
pub fn validate_web_fetch_input(input: &JsValue) -> Option<Vec<JsObject>> {
    if !matches!(input, JsValue::Object(_) | JsValue::Array(_)) {
        return Some(vec![issue("", "invalid_type", "input must be an object")]);
    }
    let url = field(input, "url");
    if !non_empty_string(url) {
        return Some(vec![issue("url", "required", "url is required")]);
    }
    if !validate_url(&String::from_utf16_lossy(url.as_string().unwrap().units())) {
        return Some(vec![issue(
            "url",
            "invalid_type",
            "url failed validation (length > 2000, malformed, embedded credentials, or non-public hostname).",
        )]);
    }
    let mode = field(input, "mode");
    let is_mode = |name: &str| mode.as_string().is_some_and(|m| m.equals_str(name));
    if !mode.is_undefined() && !is_mode("llm") && !is_mode("raw") {
        return Some(vec![issue(
            "mode",
            "invalid_enum",
            r#"mode must be "llm" or "raw""#,
        )]);
    }
    let llm = matches!(mode, JsValue::Undefined | JsValue::Null) || is_mode("llm");
    if llm && !non_empty_string(field(input, "prompt")) {
        return Some(vec![issue(
            "prompt",
            "required",
            r#"prompt is required when mode is "llm""#,
        )]);
    }
    None
}

impl ExecutableTool for WebFetchTool {
    fn validate_input(
        &self,
        input: JsValue,
        _context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move { Ok(validate_web_fetch_input(&input)) })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let options = self.options.clone();
        Box::pin(async move { execute(&input, &context, &options).await })
    }
}

fn tool_error(code: &str, message: impl Into<JsString>, details: Option<JsObject>) -> ToolError {
    let mut error = ToolError::new(code, message);
    error.details = details;
    error
}

/// `definedDetails(...)`: drop `undefined` members, keep `null`.
fn defined(entries: Vec<(&str, JsValue)>) -> JsObject {
    let mut out = JsObject::new();
    for (key, value) in entries {
        if !value.is_undefined() {
            out.insert(key.into(), value);
        }
    }
    out
}

fn is_transient(status: u16) -> bool {
    status == 408 || status == 429 || status >= 500
}

/// `buildHttpFetchErrorMessage(error)`.
fn http_error_message(error: &WebFetchHttpError) -> String {
    let label = if error.status_text.is_empty() {
        error.status.to_string()
    } else {
        format!("{} {}", error.status, error.status_text)
    };
    let mut lines = vec![format!(
        "web_fetch HTTP fetch failed with status {label} for {}.",
        error.url
    )];
    lines.push(if is_transient(error.status) {
        "The target website is temporarily unavailable or rate limited; do not treat the error page as page content. Retry later or use another source.".into()
    } else {
        "The target URL did not return a usable page; do not treat the error page as page content.".into()
    });
    if let Some(ms) = error.retry_after_ms {
        lines.push(format!("Retry-After: {}ms.", js_value::number_string(ms)));
    }
    lines.join(" ")
}

/// `makeSecondaryModelPrompt(markdown, prompt, isPreapproved)`.
pub fn make_secondary_prompt(
    markdown: &JsString,
    prompt: &JsString,
    preapproved: bool,
) -> JsString {
    let guidelines = if preapproved {
        "Provide a concise response based on the content above. Include relevant details, code examples, and documentation excerpts as needed."
    } else {
        "Provide a concise response based only on the content above. In your response:\n - Enforce a strict 125-character maximum for quotes from any source document. Open Source Software is ok as long as we respect the license.\n - Use quotation marks for exact language from articles; any language outside of the quotation should never be word-for-word the same.\n - You are not a lawyer and never comment on the legality of your own prompts and responses.\n - Never produce or reproduce exact song lyrics."
    };
    let mut units: Vec<u16> = "\nWeb page content:\n---\n".encode_utf16().collect();
    units.extend_from_slice(markdown.units());
    units.extend("\n---\n\n".encode_utf16());
    units.extend_from_slice(prompt.units());
    units.extend(format!("\n\n{guidelines}\n").encode_utf16());
    JsString::from_utf16(units)
}

fn is_record(value: &JsValue) -> bool {
    matches!(value, JsValue::Object(_) | JsValue::Array(_))
}

/// `isCanonicalModelError(value)`.
fn is_canonical_model_error(value: &JsValue) -> bool {
    let is_string = |key: &str| matches!(field(value, key), JsValue::String(_));
    is_record(value)
        && is_string("provider")
        && field(value, "protocol")
            .as_string()
            .is_some_and(|p| p.equals_str("anthropic") || p.equals_str("openai"))
        && is_string("code")
        && is_string("message")
        && matches!(field(value, "retryable"), JsValue::Bool(_))
}

fn secondary_error_details(error: &JsValue) -> JsObject {
    defined(vec![
        ("stage", string("secondary_model")),
        ("provider", field(error, "provider").clone()),
        ("protocol", field(error, "protocol").clone()),
        ("errorCode", field(error, "code").clone()),
        ("status", field(error, "status").clone()),
        ("retryable", field(error, "retryable").clone()),
        ("retryAfterMs", field(error, "retryAfterMs").clone()),
        ("userHint", field(error, "userHint").clone()),
    ])
}

fn typed(value: &JsValue, key: &str, check: fn(&JsValue) -> bool) -> JsValue {
    let property = if is_record(value) {
        field(value, key)
    } else {
        &JsValue::Undefined
    };
    if check(property) {
        property.clone()
    } else {
        JsValue::Undefined
    }
}

fn display(value: &JsValue) -> Result<JsString, ToolError> {
    js_value::to_string_units(value)
        .map(JsString::from_utf16)
        .map_err(|_| ToolError::failed(js_value::StackOverflow::MESSAGE))
}

fn concat(prefix: &str, message: &JsString) -> JsString {
    let mut units: Vec<u16> = prefix.encode_utf16().collect();
    units.extend_from_slice(message.units());
    JsString::from_utf16(units)
}

fn resolve_model(
    options: &WebFetchToolOptions,
    context: &ToolRunContext,
) -> Option<Arc<dyn SecondaryModel>> {
    options.model.clone().or_else(|| {
        context
            .host
            .model
            .as_ref()?
            .downcast_ref::<WebFetchModelPort>()
            .map(|port| Arc::clone(&port.0))
    })
}

async fn execute(
    input: &JsValue,
    context: &ToolRunContext,
    options: &WebFetchToolOptions,
) -> Result<ToolOutput, ToolError> {
    let url_value = field(input, "url").clone();
    let url = match &url_value {
        JsValue::String(url) => String::from_utf16_lossy(url.units()),
        other => String::from_utf16_lossy(display(other)?.units()),
    };
    let mode_value = field(input, "mode");
    let mode = match mode_value {
        JsValue::Undefined | JsValue::Null => JsValue::from("llm"),
        other => other.clone(),
    };
    let is_llm = mode.as_string().is_some_and(|m| m.equals_str("llm"));
    let is_raw = mode.as_string().is_some_and(|m| m.equals_str("raw"));
    let prompt = match field(input, "prompt") {
        JsValue::Undefined | JsValue::Null => JsString::from(""),
        JsValue::String(prompt) => prompt.clone(),
        other => display(other)?,
    };
    if is_llm && prompt.is_empty() {
        return Err(tool_error(
            "invalid_tool_input",
            r#"web_fetch prompt is required when mode is "llm". Use mode "raw" to fetch markdown without a secondary model."#,
            None,
        ));
    }

    let fetched = match options
        .fetcher
        .get_url_markdown_content(&url, Some(context))
        .await
    {
        Ok(result) => result,
        Err(FetchError::Http(error)) => {
            return Err(tool_error(
                "tool_execution_failed",
                http_error_message(&error).as_str(),
                Some(defined(vec![
                    ("stage", string("http_fetch")),
                    ("status", JsValue::Number(f64::from(error.status))),
                    ("statusText", string(error.status_text.as_str())),
                    ("url", string(error.url.as_str())),
                    ("contentType", string(error.content_type.as_str())),
                    (
                        "retryAfterMs",
                        error
                            .retry_after_ms
                            .map_or(JsValue::Undefined, JsValue::Number),
                    ),
                    (
                        "bodyPreview",
                        error
                            .body_preview
                            .clone()
                            .map_or(JsValue::Undefined, JsValue::String),
                    ),
                ])),
            ));
        }
        Err(FetchError::Other(message)) => {
            return Err(tool_error(
                "tool_execution_failed",
                concat("web_fetch failed: ", &message),
                Some(object([("stage", string("http_fetch"))])),
            ));
        }
    };

    let (entry, from_cache) = match fetched {
        FetchResult::Redirect {
            original_url,
            redirect_url,
            status_code,
        } => {
            let text = format!(
                "Redirect detected (status {status_code}).\nOriginal URL: {original_url}\nRedirect target: {redirect_url}\nThe redirect target is on a different host and was not auto-followed. If you trust the destination, call web_fetch again with that URL."
            );
            return Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", string("text")),
                    ("text", string(text.as_str())),
                ]))],
                data: Some(JsValue::Object(object([
                    ("url", url_value),
                    ("fromCache", JsValue::Bool(false)),
                    ("mode", mode),
                    ("llmUsed", JsValue::Bool(false)),
                    (
                        "redirect",
                        JsValue::Object(object([
                            ("redirectUrl", string(redirect_url.as_str())),
                            ("statusCode", JsValue::Number(f64::from(status_code))),
                        ])),
                    ),
                ]))),
                metadata: None,
                supplemental_messages: None,
            });
        }
        FetchResult::Fetched { entry, from_cache } => (entry, from_cache),
    };

    let truncated = truncate_markdown(&entry.content);
    let raw_length = js_length(&entry.content);
    let source_truncated = raw_length > MAX_MARKDOWN_LENGTH;
    let preapproved = is_preapproved_url(&url);
    let base = |llm_used: bool| {
        vec![
            ("url", url_value.clone()),
            ("fromCache", JsValue::Bool(from_cache)),
            ("mode", mode.clone()),
            ("llmUsed", JsValue::Bool(llm_used)),
            ("contentType", string(entry.content_type.as_str())),
            ("bytes", JsValue::Number(entry.bytes as f64)),
            ("status", JsValue::Number(f64::from(entry.code))),
            ("truncated", JsValue::Bool(source_truncated)),
            ("rawLength", JsValue::Number(raw_length as f64)),
        ]
    };

    if is_raw {
        let mut data = defined(base(false));
        data.insert(
            "returnedLength".into(),
            JsValue::Number(truncated.units().len() as f64),
        );
        return Ok(ToolOutput {
            content: vec![JsValue::Object(object([
                ("type", string("text")),
                ("text", JsValue::String(truncated)),
            ]))],
            data: Some(JsValue::Object(data)),
            metadata: None,
            supplemental_messages: None,
        });
    }

    let Some(model) = resolve_model(options, context) else {
        return Err(tool_error(
            "unsupported_tool",
            r#"web_fetch secondary model is not available. Retry with mode "raw" to fetch markdown without LLM processing."#,
            Some(object([
                ("stage", string("secondary_model")),
                ("url", url_value.clone()),
                ("mode", mode.clone()),
            ])),
        ));
    };

    let request = JsValue::Object(object([
        (
            "provider",
            string(options.provider.as_deref().unwrap_or(DEFAULT_PROVIDER)),
        ),
        (
            "model",
            string(options.model_id.as_deref().unwrap_or(DEFAULT_MODEL_ID)),
        ),
        (
            "messages",
            JsValue::Array(vec![JsValue::Object(object([
                ("role", string("user")),
                (
                    "content",
                    JsValue::Array(vec![JsValue::Object(object([
                        ("type", string("text")),
                        (
                            "text",
                            JsValue::String(make_secondary_prompt(
                                &truncated,
                                &prompt,
                                preapproved,
                            )),
                        ),
                    ]))]),
                ),
            ]))]),
        ),
        (
            "maxOutputTokens",
            JsValue::Number(
                options
                    .max_output_tokens
                    .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
            ),
        ),
        ("stream", JsValue::Bool(true)),
        (
            "metadata",
            JsValue::Object(object([
                ("tool", string("web_fetch")),
                ("url", url_value.clone()),
            ])),
        ),
    ]));

    let mut model_text: Vec<u16> = Vec::new();
    for item in model.stream(request).await {
        match item {
            SecondaryModelEvent::Event(event) => {
                if context.is_aborted() {
                    return Err(tool_error(
                        "tool_aborted",
                        "web_fetch aborted before completion.",
                        None,
                    ));
                }
                let kind = field(&event, "type");
                if kind.as_string().is_some_and(|k| k.equals_str("text_delta")) {
                    model_text.extend_from_slice(display(field(&event, "text"))?.units());
                } else if kind.as_string().is_some_and(|k| k.equals_str("error")) {
                    let error = field(&event, "error");
                    let message = display(field(error, "message"))?;
                    return Err(tool_error(
                        "tool_execution_failed",
                        concat("web_fetch secondary model error: ", &message),
                        Some(secondary_error_details(error)),
                    ));
                }
            }
            SecondaryModelEvent::Throw { is_error, value } => {
                let message = if is_error {
                    display(field(&value, "message"))?
                } else {
                    display(&value)?
                };
                let canonical = if is_canonical_model_error(&value) {
                    Some(value.clone())
                } else if is_record(&value) && is_canonical_model_error(field(&value, "error")) {
                    Some(field(&value, "error").clone())
                } else {
                    None
                };
                let details = match canonical {
                    Some(error) => secondary_error_details(&error),
                    None => defined(vec![
                        ("stage", string("secondary_model")),
                        ("message", JsValue::String(message.clone())),
                        (
                            "errorCode",
                            typed(&value, "code", |v| matches!(v, JsValue::String(_))),
                        ),
                        (
                            "status",
                            typed(&value, "status", |v| matches!(v, JsValue::Number(_))),
                        ),
                        (
                            "retryable",
                            typed(&value, "retryable", |v| matches!(v, JsValue::Bool(_))),
                        ),
                        (
                            "retryAfterMs",
                            typed(&value, "retryAfterMs", |v| matches!(v, JsValue::Number(_))),
                        ),
                        (
                            "userHint",
                            typed(&value, "userHint", |v| matches!(v, JsValue::String(_))),
                        ),
                    ]),
                };
                return Err(tool_error(
                    "tool_execution_failed",
                    concat("web_fetch secondary model failed: ", &message),
                    Some(details),
                ));
            }
        }
    }

    let visible = model_text
        .iter()
        .any(|unit| !char::from_u32(u32::from(*unit)).is_some_and(crate::runtime::tools::js_space));
    if !visible {
        return Err(tool_error(
            "tool_execution_failed",
            r#"web_fetch secondary model returned no visible text. Retry with mode "raw" to inspect the fetched markdown, or try again later."#,
            Some(object([
                ("stage", string("secondary_model")),
                ("url", url_value),
                ("mode", mode),
            ])),
        ));
    }
    let final_text = JsString::from_utf16(model_text);
    let mut data = defined(base(true));
    data.insert(
        "returnedLength".into(),
        JsValue::Number(final_text.units().len() as f64),
    );
    data.insert("modelResponse".into(), JsValue::String(final_text.clone()));
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", JsValue::String(final_text)),
        ]))],
        data: Some(JsValue::Object(data)),
        metadata: None,
        supplemental_messages: None,
    })
}
