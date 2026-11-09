//! `web_search` (baseline `tool/builtin/webSearch.ts` +
//! `additionalWebSearch.ts` + `pilot/config/webSearchProviders.ts`): one
//! configured provider (glm, tavily, serper, brave, baidu, bocha, exa,
//! serpapi or a custom endpoint), its request shape, the result projection
//! onto `{ title, link, snippet, source, publishedAt }`, and the baseline's
//! error texts.
//!
//! Property reads on the parsed response follow JS: reading a member of
//! `null` throws `Cannot read properties of null (reading 'x')`, a JSON
//! syntax error throws V8's `SyntaxError` message (`json_syntax`), and the
//! nested result search (`extractResultItems`) is iterative with the shared
//! V8 depth bound.
use super::{
    super::{field, object, schema, string},
    ask_permission,
    network::{
        BodyError, FetchOptions, HttpRequest, HttpResponse, Random, RetryOptions,
        node_timer_duration,
    },
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    js_value,
    runtime::{
        model::http::{ModelHttpTransport, NetworkFetchError},
        permission::{PermissionContext, PermissionFuture, PermissionTool},
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

const DESCRIPTION: &str = "- Searches the web for current information using the configured search provider\n- Takes a search query and optional country code (`gl`) as input\n- Returns structured search data including organic results and, when available, answer box content\n- Use this tool for current events, recent documentation, and information beyond the model's knowledge cutoff\n- Use this tool when API/SDK/framework usage is unknown, version-sensitive, or likely changed since training. Search with package/service name, version, framework, and the specific method/option/error.\n\nUsage notes:\n  - Configure `tools.webSearch.provider` as `glm`, `tavily`, `serper`, `brave`, `baidu`, `bocha`, `exa`, `serpapi`, or `custom` in `pilotdeck.yaml`\n  - Requires `tools.webSearch.apiKey`, the provider API key environment variable, or `CUSTOM_WEB_SEARCH_API_KEY` unless custom auth is `none`\n  - The optional `gl` parameter is forwarded only by providers that support localization\n  - This tool is read-only and does not modify files";

const INPUT_SCHEMA: &str = r#"{"type":"object","required":["query"],"additionalProperties":false,"properties":{"query":{"type":"string","description":"Search query string. Be specific, and include versions or the current year when looking for recent documentation, releases, or current events."},"gl":{"type":"string","description":"Optional country code for localized results. Defaults to \"us\"; use \"cn\" for China-localized results."}}}"#;

pub const SERPAPI_ENGINES: [&str; 6] = ["google", "bing", "baidu", "duckduckgo", "yahoo", "yandex"];

/// `WEB_SEARCH_ENDPOINTS`.
pub fn default_endpoint(provider: &str) -> Option<&'static str> {
    Some(match provider {
        "glm" => "https://api.z.ai/api/paas/v4/web_search",
        "tavily" => "https://api.tavily.com/search",
        "serper" => "https://google.serper.dev/search",
        "brave" => "https://api.search.brave.com/res/v1/web/search",
        "baidu" => "https://qianfan.baidubce.com/v2/ai_search/web_search",
        "bocha" => "https://api.bocha.cn/v1/web-search",
        "exa" => "https://api.exa.ai/search",
        "serpapi" => "https://serpapi.com/search.json",
        _ => return None,
    })
}

/// `WebSearchCustomProviderConfig` (all members optional).
#[derive(Clone, Debug, Default)]
pub struct CustomProviderConfig {
    pub name: Option<String>,
    pub auth: Option<String>,
    pub method: Option<String>,
    pub query_param: Option<String>,
    pub api_key_param: Option<String>,
    pub results_path: Option<String>,
    pub title_field: Option<String>,
    pub url_field: Option<String>,
    pub snippet_field: Option<String>,
    pub source_field: Option<String>,
    pub published_at_field: Option<String>,
}

/// `normalizeCustomProviderConfig(config)`.
#[derive(Clone, Debug)]
struct Custom {
    auth: String,
    name: String,
    method: String,
    query_param: String,
    api_key_param: String,
    results_path: String,
    title_field: String,
    url_field: String,
    snippet_field: String,
    source_field: String,
    published_at_field: String,
}

fn js_trim(value: &str) -> &str {
    crate::metadata::js_trim(value)
}

fn normalize_custom(config: &Option<CustomProviderConfig>) -> Custom {
    let empty = CustomProviderConfig::default();
    let config = config.as_ref().unwrap_or(&empty);
    let or = |value: &Option<String>, fallback: &str| {
        value
            .as_deref()
            .map(js_trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback)
            .to_string()
    };
    Custom {
        auth: config.auth.clone().unwrap_or_else(|| "bearer".into()),
        name: or(&config.name, "custom"),
        method: config.method.clone().unwrap_or_else(|| "POST".into()),
        query_param: or(&config.query_param, "query"),
        api_key_param: or(&config.api_key_param, "api_key"),
        results_path: or(&config.results_path, ""),
        title_field: or(&config.title_field, "title"),
        url_field: or(&config.url_field, "url"),
        snippet_field: or(&config.snippet_field, "snippet"),
        source_field: or(&config.source_field, "source"),
        published_at_field: or(&config.published_at_field, "publishedAt"),
    }
}

/// `CreateWebSearchToolOptions` (the transport replaces `fetchImpl`).
#[derive(Clone)]
pub struct WebSearchOptions {
    pub provider: Option<String>,
    pub search_engine: Option<String>,
    pub api_key: Option<String>,
    pub endpoint: Option<String>,
    pub custom_provider: Option<CustomProviderConfig>,
    pub transport: Arc<dyn ModelHttpTransport>,
    pub timeout_ms: Option<f64>,
    pub organic_limit: Option<f64>,
    pub random: Random,
}

impl WebSearchOptions {
    pub fn new(transport: Arc<dyn ModelHttpTransport>) -> Self {
        Self {
            provider: None,
            search_engine: None,
            api_key: None,
            endpoint: None,
            custom_provider: None,
            transport,
            timeout_ms: None,
            organic_limit: None,
            random: super::network::default_random(),
        }
    }
}

pub struct WebSearchTool {
    fields: JsObject,
    name: JsString,
    kind: JsString,
    options: WebSearchOptions,
}

impl WebSearchTool {
    pub fn new(options: WebSearchOptions) -> Self {
        Self {
            fields: object([
                ("name", string("web_search")),
                ("aliases", JsValue::Array(vec![string("WebSearch")])),
                ("description", string(DESCRIPTION)),
                ("kind", string("network")),
                ("inputSchema", JsValue::Object(schema(INPUT_SCHEMA))),
                ("maxResultBytes", JsValue::Number(200_000.0)),
            ]),
            name: JsString::from("web_search"),
            kind: JsString::from("network"),
            options,
        }
    }
    /// `checkAvailability(context)`: `None` is `{ ok: true }`, otherwise
    /// `(code, reason)`. `env` is the context env (`None` reads the process).
    pub fn check_availability(
        &self,
        env: Option<&JsValue>,
    ) -> Option<(&'static str, &'static str)> {
        let reader = EnvReader(env);
        let provider = resolve_provider(&self.options, &reader);
        let api_key = resolve_api_key(&self.options, &provider, &reader);
        let custom = normalize_custom(&self.options.custom_provider);
        if api_key.is_none() && !(provider == "custom" && custom.auth == "none") {
            return Some(("setup_required", "web_search requires an API key."));
        }
        if provider == "custom"
            && self
                .options
                .endpoint
                .as_deref()
                .map(js_trim)
                .is_none_or(str::is_empty)
        {
            return Some((
                "setup_required",
                "web_search custom provider requires an endpoint URL.",
            ));
        }
        None
    }
}

impl ToolDefinition for WebSearchTool {
    fn check_permissions<'a>(
        &'a self,
        input: &'a JsValue,
        context: &'a PermissionContext,
    ) -> PermissionFuture<'a, Option<JsValue>> {
        PermissionTool::check_permissions(self, input, context)
    }

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
pub fn web_search_permission() -> JsValue {
    ask_permission(
        "web_search",
        "Network search requires permission.",
        "web search",
        "Allow search",
    )
}

impl PermissionTool for WebSearchTool {
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
        Box::pin(async { Ok(Some(web_search_permission())) })
    }
}

impl ExecutableTool for WebSearchTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let options = self.options.clone();
        Box::pin(async move { execute(&input, &context, &options).await })
    }
}

// ---- env / provider resolution -------------------------------------------------

/// `(context.env ?? process.env)[name]?.trim()`, empty as `undefined`.
struct EnvReader<'a>(Option<&'a JsValue>);

impl EnvReader<'_> {
    fn read(&self, name: &str) -> Option<String> {
        let value = match self.0 {
            Some(env) if !matches!(env, JsValue::Undefined | JsValue::Null) => field(env, name)
                .as_string()
                .map(|value| String::from_utf16_lossy(value.units())),
            _ => std::env::var(name).ok(),
        }?;
        let trimmed = js_trim(&value).to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    }
}

fn resolve_provider(options: &WebSearchOptions, env: &EnvReader<'_>) -> String {
    if let Some(provider) = &options.provider {
        return provider.clone();
    }
    if options
        .api_key
        .as_deref()
        .map(js_trim)
        .is_some_and(|key| !key.is_empty())
    {
        return "glm".into();
    }
    for (name, provider) in [
        ("TAVILY_API_KEY", "tavily"),
        ("GLM_WEB_SEARCH_API_KEY", "glm"),
        ("ZAI_API_KEY", "glm"),
        ("SERPER_API_KEY", "serper"),
        ("BRAVE_API_KEY", "brave"),
        ("CUSTOM_WEB_SEARCH_API_KEY", "custom"),
        ("BAIDU_WEB_SEARCH_API_KEY", "baidu"),
        ("BOCHA_API_KEY", "bocha"),
        ("EXA_API_KEY", "exa"),
        ("SERPAPI_API_KEY", "serpapi"),
    ] {
        if env.read(name).is_some() {
            return provider.into();
        }
    }
    "glm".into()
}

fn resolve_api_key(
    options: &WebSearchOptions,
    provider: &str,
    env: &EnvReader<'_>,
) -> Option<String> {
    if let Some(key) = options
        .api_key
        .as_deref()
        .map(js_trim)
        .filter(|key| !key.is_empty())
    {
        return Some(key.to_string());
    }
    let name = match provider {
        "tavily" => "TAVILY_API_KEY",
        "serper" => "SERPER_API_KEY",
        "brave" => "BRAVE_API_KEY",
        "custom" => "CUSTOM_WEB_SEARCH_API_KEY",
        "baidu" => "BAIDU_WEB_SEARCH_API_KEY",
        "bocha" => "BOCHA_API_KEY",
        "exa" => "EXA_API_KEY",
        "serpapi" => "SERPAPI_API_KEY",
        _ => {
            return env
                .read("GLM_WEB_SEARCH_API_KEY")
                .or_else(|| env.read("ZAI_API_KEY"));
        }
    };
    env.read(name)
}

// ---- JS helpers ------------------------------------------------------------------

fn err(code: &str, message: impl Into<JsString>) -> ToolError {
    ToolError::new(code, message)
}

fn failed(message: impl Into<JsString>) -> ToolError {
    err("tool_execution_failed", message)
}

/// `isRecord` of webSearch.ts: a non-null, non-array object.
fn is_record(value: &JsValue) -> bool {
    matches!(value, JsValue::Object(_))
}

/// `value[key]` with JS's TypeError on `null` / `undefined`.
fn prop<'a>(value: &'a JsValue, key: &str) -> Result<&'a JsValue, ToolError> {
    match value {
        JsValue::Null => Err(failed(
            format!("Cannot read properties of null (reading '{key}')").as_str(),
        )),
        JsValue::Undefined => Err(failed(
            format!("Cannot read properties of undefined (reading '{key}')").as_str(),
        )),
        other => Ok(field(other, key)),
    }
}

/// `readString(value)`.
fn read_string(value: &JsValue) -> Option<JsString> {
    value.as_string().filter(|text| !text.is_empty()).cloned()
}

fn first(values: &[&JsValue]) -> JsValue {
    values
        .iter()
        .find_map(|value| read_string(value))
        .map_or(JsValue::Undefined, JsValue::String)
}

/// `readPath(value, path)`.
fn read_path(value: &JsValue, path: &str) -> JsValue {
    let trimmed = js_trim(path);
    if trimmed.is_empty() {
        return JsValue::Undefined;
    }
    let mut current = value.clone();
    for segment in trimmed.split('.') {
        if !is_record(&current) {
            return JsValue::Undefined;
        }
        current = field(&current, segment).clone();
    }
    current
}

/// `truncate(text, max)` on UTF-16 units, with `…`.
fn truncate(text: &str, max: usize) -> JsString {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= max {
        return JsString::from_utf16(units);
    }
    let mut out = units[..max].to_vec();
    out.push(0x2026);
    JsString::from_utf16(out)
}

fn concat(parts: &[&[u16]]) -> JsString {
    JsString::from_utf16(parts.concat())
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `extractResultItems(value)`, iterative: an array is returned as is; a
/// record is searched through `search_result`, `results`, `items`,
/// `webPages`, `data` in order, depth first.
fn extract_result_items(value: &JsValue) -> Result<Vec<JsValue>, ToolError> {
    const KEYS: [&str; 5] = ["search_result", "results", "items", "webPages", "data"];
    if let JsValue::Array(items) = value {
        return Ok(items.clone());
    }
    if !is_record(value) {
        return Ok(Vec::new());
    }
    let mut stack: Vec<(&JsValue, usize)> = vec![(value, 0)];
    while let Some((node, index)) = stack.pop() {
        if index >= KEYS.len() {
            continue;
        }
        stack.push((node, index + 1));
        let child = field(node, KEYS[index]);
        if let JsValue::Array(items) = child {
            return Ok(items.clone());
        }
        if is_record(child) {
            if stack.len() >= js_value::MAX_ARRAY_DEPTH {
                return Err(err(
                    "tool_execution_failed",
                    js_value::StackOverflow::MESSAGE,
                ));
            }
            stack.push((child, 0));
        }
    }
    Ok(Vec::new())
}

fn organic_entry(entries: [(&str, JsValue); 5]) -> JsValue {
    JsValue::Object(object(entries))
}

/// `parseGlmResults(value, limit)`.
fn parse_glm_results(value: &JsValue, limit: usize) -> Result<Vec<JsValue>, ToolError> {
    let JsValue::Array(items) = value else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .take(limit)
        .map(|entry| {
            let get = |key: &str| prop(entry, key);
            Ok(organic_entry([
                ("title", first(&[get("title")?, get("name")?])),
                ("link", first(&[get("url")?, get("link")?, get("href")?])),
                (
                    "snippet",
                    first(&[
                        get("snippet")?,
                        get("summary")?,
                        get("content")?,
                        get("description")?,
                        get("text")?,
                    ]),
                ),
                (
                    "source",
                    first(&[get("source")?, get("site")?, get("media")?]),
                ),
                (
                    "publishedAt",
                    first(&[
                        get("publishedAt")?,
                        get("published_at")?,
                        get("publish_date")?,
                        get("date")?,
                    ]),
                ),
            ]))
        })
        .collect()
}

fn parse_mapped_results(value: &JsValue, limit: usize, custom: &Custom) -> Vec<JsValue> {
    let JsValue::Array(items) = value else {
        return Vec::new();
    };
    items
        .iter()
        .take(limit)
        .map(|entry| {
            let get = |path: &str| first(&[&read_path(entry, path)]);
            organic_entry([
                ("title", get(&custom.title_field)),
                ("link", get(&custom.url_field)),
                ("snippet", get(&custom.snippet_field)),
                ("source", get(&custom.source_field)),
                ("publishedAt", get(&custom.published_at_field)),
            ])
        })
        .collect()
}

/// `formatTextSummary(output)`.
fn format_summary(
    query: &JsString,
    organic: &[JsValue],
    answer_box: Option<&JsValue>,
) -> Result<JsString, ToolError> {
    let mut out = concat(&[&units("Web search results for: "), query.units()])
        .units()
        .to_vec();
    let push_line = |out: &mut Vec<u16>, line: &[u16]| {
        out.push(u16::from(b'\n'));
        out.extend_from_slice(line);
    };
    if let Some(answer) = answer_box {
        push_line(&mut out, &[]);
        push_line(&mut out, &units("Answer box:"));
        let json = js_value::json_stringify(answer)
            .map_err(|_| failed(js_value::StackOverflow::MESSAGE))?
            .unwrap_or_default();
        push_line(&mut out, &units(&json));
    }
    if organic.is_empty() {
        push_line(&mut out, &[]);
        push_line(&mut out, &units("No organic results."));
    } else {
        push_line(&mut out, &[]);
        push_line(&mut out, &units("Organic results:"));
        for entry in organic {
            let title = field(entry, "title")
                .as_string()
                .cloned()
                .unwrap_or_else(|| JsString::from("(no title)"));
            let link = field(entry, "link")
                .as_string()
                .cloned()
                .unwrap_or_else(|| JsString::from(""));
            push_line(
                &mut out,
                concat(&[&units("- "), title.units(), &units(" — "), link.units()]).units(),
            );
            if let Some(snippet) = field(entry, "snippet")
                .as_string()
                .filter(|s| !s.is_empty())
            {
                push_line(&mut out, concat(&[&units("  "), snippet.units()]).units());
            }
        }
    }
    Ok(JsString::from_utf16(out))
}

fn output(
    query: &JsString,
    organic: Vec<JsValue>,
    answer_box: Option<JsValue>,
    metadata: JsObject,
) -> Result<ToolOutput, ToolError> {
    let text = format_summary(query, &organic, answer_box.as_ref())?;
    let mut data = object([
        ("query", JsValue::String(query.clone())),
        ("organic", JsValue::Array(organic)),
    ]);
    if let Some(answer) = answer_box {
        data.insert("answerBox".into(), answer);
    }
    let data = JsValue::Object(data);
    Ok(ToolOutput {
        content: vec![
            JsValue::Object(object([
                ("type", string("text")),
                ("text", JsValue::String(text)),
            ])),
            JsValue::Object(object([("type", string("json")), ("value", data.clone())])),
        ],
        data: Some(data),
        metadata: Some(metadata),
        supplemental_messages: None,
    })
}

/// `new URL(endpoint)` restricted to http(s).
fn http_url(endpoint: &str) -> Option<url::Url> {
    url::Url::parse(endpoint)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
}

fn assert_http_endpoint(endpoint: &str, provider: &str) -> Result<(), ToolError> {
    match http_url(endpoint) {
        Some(_) => Ok(()),
        None => Err(err(
            "setup_required",
            format!("web_search ({provider}) requires a valid HTTP(S) endpoint URL.").as_str(),
        )),
    }
}

/// `url.searchParams.set(name, value)`.
fn set_search_param(url: &mut url::Url, name: &str, value: &str) {
    let mut pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    match pairs.iter().position(|(key, _)| key == name) {
        Some(index) => {
            pairs[index].1 = value.to_string();
            let mut seen = false;
            pairs.retain(|(key, _)| {
                if key != name {
                    return true;
                }
                let keep = !seen;
                seen = true;
                keep
            });
        }
        None => pairs.push((name.to_string(), value.to_string())),
    }
    let serialized = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter())
        .finish();
    url.set_query((!serialized.is_empty()).then_some(serialized.as_str()));
}

fn query_of(input: &JsValue) -> Result<JsString, ToolError> {
    match field(input, "query") {
        JsValue::String(query) => Ok(JsString::from(js_trim(&String::from_utf16_lossy(
            query.units(),
        )))),
        JsValue::Undefined => Err(failed(
            "Cannot read properties of undefined (reading 'trim')",
        )),
        JsValue::Null => Err(failed("Cannot read properties of null (reading 'trim')")),
        _ => Err(failed("input.query.trim is not a function")),
    }
}

fn gl_of(input: &JsValue) -> Option<String> {
    field(input, "gl")
        .as_string()
        .map(|gl| js_trim(&String::from_utf16_lossy(gl.units())).to_string())
        .filter(|gl| !gl.is_empty())
}

fn empty_query() -> ToolError {
    err(
        "invalid_tool_input",
        "web_search requires a non-empty `query`.",
    )
}

fn json_text(value: &JsValue) -> String {
    js_value::json_stringify(value)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// `response.text()` (UTF-8 with the BOM stripped, as `TextDecoder`).
async fn response_text(
    response: &mut HttpResponse,
    context: &ToolRunContext,
) -> Result<String, BodyError> {
    let body = response
        .read_body(crate::runtime::model::MAX_STRING_LENGTH, Some(context))
        .await?;
    let text = String::from_utf8_lossy(&body).into_owned();
    Ok(text
        .strip_prefix('\u{FEFF}')
        .map(str::to_string)
        .unwrap_or(text))
}

/// `response.json()`: `Err` is the thrown message (`TypeError: terminated`
/// or V8's `SyntaxError`).
async fn response_json(
    response: &mut HttpResponse,
    context: &ToolRunContext,
) -> Result<JsValue, String> {
    let text = response_text(response, context)
        .await
        .map_err(|error| match error {
            // The parent signal fired mid-body; `send` reports the same
            // `network_abort` message before the headers.
            BodyError::Aborted => "Network request aborted by parent signal.".to_string(),
            BodyError::Terminated | BodyError::TooLarge => "terminated".to_string(),
        })?;
    if let Some(message) = pilotdeck_protocol::json_syntax::parse_error(&text) {
        return Err(message);
    }
    pilotdeck_protocol::parse_json(&text).map_err(|_| "Unexpected end of JSON input".to_string())
}

/// The per-provider `AbortController` with its `timeoutMs` timer around
/// `networkFetch`. `Err(None)` is a local timeout.
async fn send(
    options: &WebSearchOptions,
    context: &ToolRunContext,
    request: &HttpRequest,
    retry_on_post: bool,
) -> Result<HttpResponse, Option<NetworkFetchError>> {
    let timeout_ms = options.timeout_ms.unwrap_or(30_000.0);
    let fetch = FetchOptions {
        timeout_ms,
        parent: Some(context),
        retry: RetryOptions {
            max_retries: 2,
            base_delay_ms: 500.0,
            max_delay_ms: 5_000.0,
            retry_on_post,
        },
        random: Arc::clone(&options.random),
    };
    // `setTimeout(() => controller.abort(), timeoutMs)` in every provider is
    // unconditional: 0, a negative value and NaN still abort the call. Only
    // `networkFetch`'s own per-attempt timer is gated on `timeoutMs > 0`.
    // Node clamps the delay to [1, 2^31-1] ms (NaN, ±Infinity, values below
    // 1 and values above 2^31-1 become 1 ms), so an overflowing number never
    // reaches `Duration::from_secs_f64`, which panics.
    // The network layer dispatches this deadline together with the response,
    // parent signal, per-attempt timer and retry timers; no outer race may
    // override the event that it has already selected.
    let deadline = tokio::time::Instant::now() + node_timer_duration(timeout_ms);
    match super::network::network_fetch_until(options.transport.as_ref(), request, &fetch, deadline)
        .await
    {
        Ok(response) => Ok(response),
        // exchange already selected the event: a later parent cancellation
        // must not relabel an earlier local timeout.
        Err(error) if error.code == "network_timeout" => Err(None),
        Err(error) => Err(Some(error)),
    }
}

fn timed_out(label: &str, options: &WebSearchOptions) -> ToolError {
    err(
        "tool_timeout",
        format!(
            "{label} timed out after {}ms.",
            js_value::number_string(options.timeout_ms.unwrap_or(30_000.0))
        )
        .as_str(),
    )
}

fn organic_limit(options: &WebSearchOptions) -> f64 {
    options.organic_limit.unwrap_or(8.0)
}

/// `Array#slice(0, limit)` count for a non-negative limit.
fn take_count(limit: f64) -> usize {
    if limit.is_nan() || limit <= 0.0 {
        0
    } else {
        limit.trunc().min(usize::MAX as f64) as usize
    }
}

async fn execute(
    input: &JsValue,
    context: &ToolRunContext,
    options: &WebSearchOptions,
) -> Result<ToolOutput, ToolError> {
    let env_value = context.fields.get("env").cloned();
    let env = EnvReader(env_value.as_ref());
    let provider = resolve_provider(options, &env);
    let api_key = resolve_api_key(options, &provider, &env);
    let custom = normalize_custom(&options.custom_provider);
    if api_key.is_none() && !(provider == "custom" && custom.auth == "none") {
        return Err(ToolError::new(
            "setup_required",
            "web_search requires an API key. Please configure it in Settings → Search.",
        )
        .with_object_details(object([("tool", string("web_search"))])));
    }
    let api_key = api_key.unwrap_or_default();
    if provider == "custom" {
        let Some(endpoint) = options
            .endpoint
            .as_deref()
            .filter(|e| !js_trim(e).is_empty())
        else {
            return Err(ToolError::new(
                "setup_required",
                "web_search custom provider requires an endpoint URL. Please configure it in Settings → Search.",
            )
            .with_object_details(object([("tool", string("web_search"))])));
        };
        return custom_search(input, context, options, &api_key, endpoint, &custom).await;
    }
    if matches!(provider.as_str(), "baidu" | "bocha" | "exa" | "serpapi") {
        let endpoint = options
            .endpoint
            .clone()
            .unwrap_or_else(|| default_endpoint(&provider).unwrap().to_string());
        return additional_search(input, context, options, &api_key, &provider, &endpoint).await;
    }
    match provider.as_str() {
        "tavily" => {
            let endpoint = options
                .endpoint
                .clone()
                .unwrap_or_else(|| default_endpoint("tavily").unwrap().into());
            tavily_search(input, context, options, &api_key, &endpoint).await
        }
        "serper" | "brave" => {
            let endpoint = options
                .endpoint
                .clone()
                .unwrap_or_else(|| default_endpoint(&provider).unwrap().into());
            structured_search(input, context, options, &api_key, &provider, &endpoint).await
        }
        _ => {
            let endpoint = options
                .endpoint
                .clone()
                .or_else(|| env.read("GLM_WEB_SEARCH_ENDPOINT"))
                .unwrap_or_else(|| default_endpoint("glm").unwrap().into());
            glm_search(input, context, options, &api_key, &endpoint).await
        }
    }
}

trait WithObjectDetails {
    fn with_object_details(self, details: JsObject) -> Self;
}
impl WithObjectDetails for ToolError {
    fn with_object_details(mut self, details: JsObject) -> Self {
        self.details = Some(details);
        self
    }
}

fn post_json(url: String, headers: Vec<(&str, String)>, body: &JsValue) -> HttpRequest {
    HttpRequest {
        method: "POST",
        url,
        headers: headers
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        body: Some(json_text(body)),
    }
}

/// The text of a non-OK body, or `statusText` when it cannot be read.
async fn error_detail(response: &mut HttpResponse, context: &ToolRunContext) -> String {
    match response_text(response, context).await {
        Ok(text) => text,
        Err(_) => response.status_text.clone(),
    }
}

fn request_failed(label: &str, message: &JsString) -> ToolError {
    failed(concat(&[
        &units(&format!("{label} request failed: ")),
        message.units(),
    ]))
}

async fn structured_search(
    input: &JsValue,
    context: &ToolRunContext,
    options: &WebSearchOptions,
    api_key: &str,
    provider: &str,
    endpoint: &str,
) -> Result<ToolOutput, ToolError> {
    assert_http_endpoint(endpoint, provider)?;
    let query = query_of(input)?;
    if query.is_empty() {
        return Err(empty_query());
    }
    let limit = organic_limit(options);
    let mut url = http_url(endpoint).unwrap();
    let query_text = String::from_utf16_lossy(query.units());
    let mut headers = vec![("Accept".to_string(), "application/json".to_string())];
    let mut body = None;
    if provider == "serper" {
        headers.push(("X-API-KEY".into(), api_key.into()));
        headers.push(("Content-Type".into(), "application/json".into()));
        let mut payload = object([
            ("q", JsValue::String(query.clone())),
            ("num", JsValue::Number(limit)),
        ]);
        if let Some(gl) = gl_of(input) {
            payload.insert("gl".into(), JsValue::from(gl.as_str()));
        }
        body = Some(json_text(&JsValue::Object(payload)));
    } else {
        headers.push(("X-Subscription-Token".into(), api_key.into()));
        set_search_param(&mut url, "q", &query_text);
        set_search_param(&mut url, "count", &js_value::number_string(limit));
        if let Some(gl) = gl_of(input) {
            set_search_param(&mut url, "country", &gl);
        }
    }
    let request = HttpRequest {
        method: if provider == "serper" { "POST" } else { "GET" },
        url: url.to_string(),
        headers,
        body,
    };
    let label = format!("web_search ({provider})");
    let mut response = match send(options, context, &request, provider == "serper").await {
        Ok(response) => response,
        Err(None) => return Err(timed_out(&label, options)),
        Err(Some(error)) => return Err(request_failed(&label, &error.message)),
    };
    if !response.ok() {
        let detail = error_detail(&mut response, context).await;
        return Err(failed(concat(&[
            &units(&format!("{provider} API error ({}): ", response.status)),
            truncate(&detail, 500).units(),
        ])));
    }
    let raw = response_json(&mut response, context)
        .await
        .map_err(|message| failed(message.as_str()))?;
    let results = if provider == "brave" {
        read_path(&raw, "web.results")
    } else {
        prop(&raw, "organic")?.clone()
    };
    let organic = parse_glm_results(&results, take_count(limit))?;
    let count = organic.len();
    output(
        &query,
        organic,
        None,
        object([
            ("provider", string(provider)),
            ("endpoint", string(endpoint)),
            ("organicCount", JsValue::Number(count as f64)),
        ]),
    )
}

async fn tavily_search(
    input: &JsValue,
    context: &ToolRunContext,
    options: &WebSearchOptions,
    api_key: &str,
    endpoint: &str,
) -> Result<ToolOutput, ToolError> {
    assert_http_endpoint(endpoint, "tavily")?;
    let query = query_of(input)?;
    if query.is_empty() {
        return Err(empty_query());
    }
    let limit = organic_limit(options);
    let body = JsValue::Object(object([
        ("api_key", string(api_key)),
        ("query", JsValue::String(query.clone())),
        ("max_results", JsValue::Number(limit)),
        ("include_answer", JsValue::Bool(true)),
        ("search_depth", string("basic")),
    ]));
    let request = post_json(
        endpoint.to_string(),
        vec![
            ("Content-Type", "application/json".into()),
            ("Accept", "application/json".into()),
        ],
        &body,
    );
    let label = "web_search (tavily)";
    let mut response = match send(options, context, &request, true).await {
        Ok(response) => response,
        Err(None) => return Err(timed_out(label, options)),
        Err(Some(error)) => return Err(request_failed(label, &error.message)),
    };
    if !response.ok() {
        let detail = error_detail(&mut response, context).await;
        return Err(failed(concat(&[
            &units(&format!("Tavily API error ({}): ", response.status)),
            truncate(&detail, 500).units(),
        ])));
    }
    let raw = response_json(&mut response, context)
        .await
        .map_err(|message| failed(message.as_str()))?;
    let mut organic = Vec::new();
    if let JsValue::Array(results) = prop(&raw, "results")? {
        for result in results.iter().take(take_count(limit)) {
            let get = |key: &str| prop(result, key);
            let mut entry = JsObject::new();
            entry.insert("title".into(), first(&[get("title")?]));
            entry.insert("link".into(), first(&[get("url")?]));
            entry.insert("snippet".into(), first(&[get("content")?]));
            entry.insert("source".into(), first(&[get("url")?]));
            organic.push(JsValue::Object(entry));
        }
    }
    let answer = match prop(&raw, "answer")? {
        JsValue::String(answer) if !answer.is_empty() => Some(JsValue::Object(object([(
            "answer",
            JsValue::String(answer.clone()),
        )]))),
        _ => None,
    };
    let count = organic.len();
    output(
        &query,
        organic,
        answer,
        object([
            ("provider", string("tavily")),
            ("endpoint", string(endpoint)),
            ("engine", string("tavily")),
            ("organicCount", JsValue::Number(count as f64)),
        ]),
    )
}

async fn glm_search(
    input: &JsValue,
    context: &ToolRunContext,
    options: &WebSearchOptions,
    api_key: &str,
    endpoint: &str,
) -> Result<ToolOutput, ToolError> {
    assert_http_endpoint(endpoint, "glm")?;
    let query = query_of(input)?;
    if query.is_empty() {
        return Err(empty_query());
    }
    let limit = organic_limit(options);
    let body = JsValue::Object(object([
        ("search_engine", string("search-prime")),
        ("search_query", JsValue::String(query.clone())),
        // `Math.max(1, Math.min(limit, 50))`; `clamp` keeps NaN like JS.
        ("count", JsValue::Number(limit.clamp(1.0, 50.0))),
        ("search_recency_filter", string("noLimit")),
    ]));
    let request = post_json(
        endpoint.to_string(),
        vec![
            ("Authorization", format!("Bearer {api_key}")),
            ("Content-Type", "application/json".into()),
            ("Accept", "application/json".into()),
        ],
        &body,
    );
    let mut response = match send(options, context, &request, true).await {
        Ok(response) => response,
        Err(None) => return Err(timed_out("web_search", options)),
        Err(Some(error)) => return Err(request_failed("web_search (glm)", &error.message)),
    };
    if !response.ok() {
        let detail = error_detail(&mut response, context).await;
        return Err(failed(concat(&[
            &units(&format!("GLM web search error ({}): ", response.status)),
            truncate(&detail, 500).units(),
        ])));
    }
    let raw = response_json(&mut response, context)
        .await
        .map_err(|message| failed(message.as_str()))?;
    if let JsValue::String(error) = prop(&raw, "error")?
        && !error.is_empty()
    {
        return Err(failed(concat(&[
            &units("GLM web search error: "),
            error.units(),
        ])));
    }
    if let JsValue::Number(code) = prop(&raw, "code")?
        && *code != 0.0
    {
        let message = match prop(&raw, "msg")? {
            JsValue::String(message) => message.clone(),
            _ => JsString::from("search proxy error"),
        };
        return Err(failed(concat(&[
            &units(&format!(
                "GLM web search error code={}: ",
                js_value::number_string(*code)
            )),
            message.units(),
        ])));
    }
    let organic = parse_glm_results(
        &JsValue::Array(extract_result_items(&raw)?),
        take_count(limit),
    )?;
    let count = organic.len();
    output(
        &query,
        organic,
        None,
        object([
            ("provider", string("glm")),
            ("endpoint", string(endpoint)),
            ("organicCount", JsValue::Number(count as f64)),
        ]),
    )
}

async fn custom_search(
    input: &JsValue,
    context: &ToolRunContext,
    options: &WebSearchOptions,
    api_key: &str,
    endpoint: &str,
    custom: &Custom,
) -> Result<ToolOutput, ToolError> {
    let query = query_of(input)?;
    if query.is_empty() {
        return Err(empty_query());
    }
    let Some(mut url) = http_url(endpoint) else {
        return Err(err(
            "invalid_tool_input",
            format!("web_search custom provider endpoint is not a valid URL: {endpoint}").as_str(),
        ));
    };
    let query_text = String::from_utf16_lossy(query.units());
    let mut headers = vec![("Accept".to_string(), "application/json".to_string())];
    let mut body = JsObject::new();
    let get = custom.method == "GET";
    if get {
        set_search_param(&mut url, &custom.query_param, &query_text);
        if let Some(gl) = gl_of(input) {
            set_search_param(&mut url, "gl", &gl);
        }
    } else {
        headers.push(("Content-Type".into(), "application/json".into()));
        body.insert(
            custom.query_param.as_str().into(),
            JsValue::String(query.clone()),
        );
        if let Some(gl) = gl_of(input) {
            body.insert("gl".into(), JsValue::from(gl.as_str()));
        }
    }
    if !api_key.is_empty() {
        match custom.auth.as_str() {
            "bearer" => headers.push(("Authorization".into(), format!("Bearer {api_key}"))),
            "queryApiKey" => set_search_param(&mut url, &custom.api_key_param, api_key),
            "bodyApiKey" if get => set_search_param(&mut url, &custom.api_key_param, api_key),
            "bodyApiKey" => {
                body.insert(custom.api_key_param.as_str().into(), JsValue::from(api_key));
            }
            _ => {}
        }
    }
    let request = HttpRequest {
        method: if get { "GET" } else { "POST" },
        url: url.to_string(),
        headers,
        body: (!get).then(|| json_text(&JsValue::Object(body))),
    };
    let label = "web_search (custom)";
    let mut response = match send(options, context, &request, !get).await {
        Ok(response) => response,
        Err(None) => return Err(timed_out(label, options)),
        Err(Some(error)) => return Err(request_failed(label, &error.message)),
    };
    if !response.ok() {
        let detail = error_detail(&mut response, context).await;
        return Err(failed(concat(&[
            &units(&format!("Custom web search error ({}): ", response.status)),
            truncate(&detail, 500).units(),
        ])));
    }
    let raw = response_json(&mut response, context)
        .await
        .map_err(|message| failed(message.as_str()))?;
    if let JsValue::String(error) = prop(&raw, "error")?
        && !error.is_empty()
    {
        return Err(failed(concat(&[
            &units("Custom web search error: "),
            error.units(),
        ])));
    }
    if let JsValue::Number(code) = prop(&raw, "code")?
        && *code != 0.0
    {
        let message = match prop(&raw, "msg")? {
            JsValue::String(message) => message.clone(),
            _ => JsString::from("search provider error"),
        };
        return Err(failed(concat(&[
            &units(&format!(
                "Custom web search error code={}: ",
                js_value::number_string(*code)
            )),
            message.units(),
        ])));
    }
    let results = if custom.results_path.is_empty() {
        JsValue::Array(extract_result_items(&raw)?)
    } else {
        read_path(&raw, &custom.results_path)
    };
    let organic = parse_mapped_results(&results, take_count(organic_limit(options)), custom);
    let count = organic.len();
    output(
        &query,
        organic,
        None,
        object([
            ("provider", string("custom")),
            ("providerName", string(custom.name.as_str())),
            ("endpoint", string(endpoint)),
            ("organicCount", JsValue::Number(count as f64)),
        ]),
    )
}

// ---- additional providers (baidu, bocha, exa, serpapi) ---------------------------------

/// `text(value)` of additionalWebSearch.ts: a string with non-blank content.
fn text_value(value: &JsValue) -> Option<JsString> {
    value
        .as_string()
        .filter(|text| !js_trim(&String::from_utf16_lossy(text.units())).is_empty())
        .cloned()
}

fn first_text(values: &[&JsValue]) -> Option<JsString> {
    values.iter().find_map(|value| text_value(value))
}

/// `additionalSearchRequest(provider, options)`; `Err` is the thrown message.
pub fn additional_search_request(
    provider: &str,
    endpoint: &str,
    api_key: &str,
    query: &JsString,
    limit: f64,
    search_engine: Option<&str>,
    gl: Option<&str>,
) -> Result<HttpRequest, String> {
    let mut url = url::Url::parse(endpoint).map_err(|_| "Invalid URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Search endpoint must use HTTP(S).".into());
    }
    let mut headers = vec![("Accept".to_string(), "application/json".to_string())];
    let limit = limit.floor().clamp(1.0, 50.0);
    let query_text = String::from_utf16_lossy(query.units());
    if provider == "serpapi" {
        let engine = search_engine.unwrap_or("google");
        if !SERPAPI_ENGINES.contains(&engine) {
            return Err("Unsupported SerpAPI search engine.".into());
        }
        set_search_param(&mut url, "engine", engine);
        let key = match engine {
            "yahoo" => "p",
            "yandex" => "text",
            _ => "q",
        };
        set_search_param(&mut url, key, &query_text);
        set_search_param(&mut url, "api_key", api_key);
        if engine == "google"
            && let Some(gl) = gl.map(js_trim).filter(|gl| !gl.is_empty())
        {
            set_search_param(&mut url, "gl", gl);
        }
        return Ok(HttpRequest {
            method: "GET",
            url: url.to_string(),
            headers,
            body: None,
        });
    }
    headers.push(("Content-Type".into(), "application/json".into()));
    let body = match provider {
        "baidu" => {
            headers.push(("Authorization".into(), format!("Bearer {api_key}")));
            object([
                (
                    "messages",
                    JsValue::Array(vec![JsValue::Object(object([
                        ("role", string("user")),
                        ("content", JsValue::String(query.clone())),
                    ]))]),
                ),
                ("search_source", string("baidu_search_v2")),
                (
                    "resource_type_filter",
                    JsValue::Array(vec![JsValue::Object(object([
                        ("type", string("web")),
                        ("top_k", JsValue::Number(limit)),
                    ]))]),
                ),
            ])
        }
        "bocha" => {
            headers.push(("Authorization".into(), format!("Bearer {api_key}")));
            object([
                ("query", JsValue::String(query.clone())),
                ("count", JsValue::Number(limit)),
                ("freshness", string("noLimit")),
                ("summary", JsValue::Bool(true)),
            ])
        }
        _ => {
            headers.push(("x-api-key".into(), api_key.into()));
            object([
                ("query", JsValue::String(query.clone())),
                ("numResults", JsValue::Number(limit)),
                ("type", string("auto")),
                (
                    "contents",
                    JsValue::Object(object([
                        ("highlights", JsValue::Bool(true)),
                        ("text", JsValue::Bool(false)),
                    ])),
                ),
            ])
        }
    };
    Ok(HttpRequest {
        method: "POST",
        url: url.to_string(),
        headers,
        body: Some(json_text(&JsValue::Object(body))),
    })
}

/// `additionalSearchResults(provider, raw, limit)`; `Err` is the thrown message.
pub fn additional_search_results(
    provider: &str,
    raw: &JsValue,
    limit: f64,
) -> Result<Vec<JsValue>, JsString> {
    if !is_record(raw) {
        return Err(JsString::from(
            "Search API returned an invalid JSON response.",
        ));
    }
    let error = field(raw, "error");
    if crate::runtime::tools::builtin::truthy(error) {
        return Err(match error {
            JsValue::String(text) => text.clone(),
            other => JsString::from(json_text(other).as_str()),
        });
    }
    let code = field(raw, "code");
    let ok_code = match code {
        JsValue::Undefined => true,
        JsValue::Number(n) => *n == 0.0 || (provider == "bocha" && *n == 200.0),
        JsValue::String(s) => s.equals_str("0") || (provider == "bocha" && s.equals_str("200")),
        _ => false,
    };
    if !ok_code {
        let shown = js_value::to_string_units(code)
            .map_err(|_| JsString::from(js_value::StackOverflow::MESSAGE))?;
        let message = first_text(&[field(raw, "message"), field(raw, "msg")])
            .unwrap_or_else(|| JsString::from("Search provider error"));
        return Err(concat(&[
            &units("code="),
            &shown,
            &units(": "),
            message.units(),
        ]));
    }
    let metadata = field(raw, "search_metadata");
    let status_is = |value: &str| {
        is_record(metadata)
            && field(metadata, "status")
                .as_string()
                .is_some_and(|s| s.equals_str(value))
    };
    if provider == "serpapi" && status_is("Error") {
        return Err(JsString::from("SerpAPI search failed."));
    }
    let data = if provider == "bocha" && is_record(field(raw, "data")) {
        field(raw, "data")
    } else {
        raw
    };
    let pages = field(data, "webPages");
    let pages = if is_record(pages) {
        field(pages, "value")
    } else {
        &JsValue::Undefined
    };
    let items = match provider {
        "baidu" => field(raw, "references"),
        "bocha" => pages,
        "exa" => field(raw, "results"),
        _ => field(raw, "organic_results"),
    };
    let JsValue::Array(items) = items else {
        if provider == "serpapi" && status_is("Success") {
            return Ok(Vec::new());
        }
        return Err(JsString::from(
            "Search API response is missing its result list.",
        ));
    };
    let count = take_count(limit);
    Ok(items
        .iter()
        .filter(|item| is_record(item))
        .filter(|item| {
            provider != "baidu"
                || !crate::runtime::tools::builtin::truthy(field(item, "type"))
                || field(item, "type")
                    .as_string()
                    .is_some_and(|t| t.equals_str("web"))
        })
        .take(count)
        .map(|item| {
            let get = |key: &str| field(item, key);
            let highlights = match get("highlights") {
                JsValue::Array(values) => {
                    let joined: Vec<Vec<u16>> = values
                        .iter()
                        .filter_map(|value| value.as_string().map(|s| s.units().to_vec()))
                        .collect();
                    let joined = joined.join(&(u16::from(b'\n')));
                    (!joined.is_empty()).then(|| JsString::from_utf16(joined))
                }
                _ => None,
            };
            let snippet = first_text(&[get("summary"), get("snippet"), get("content")])
                .or(highlights)
                .or_else(|| text_value(get("text")));
            let value =
                |option: Option<JsString>| option.map_or(JsValue::Undefined, JsValue::String);
            organic_entry([
                ("title", value(first_text(&[get("title"), get("name")]))),
                ("link", value(first_text(&[get("url"), get("link")]))),
                ("snippet", value(snippet)),
                (
                    "source",
                    value(first_text(&[get("siteName"), get("source")])),
                ),
                (
                    "publishedAt",
                    value(first_text(&[
                        get("publishedDate"),
                        get("datePublished"),
                        get("date"),
                    ])),
                ),
            ])
        })
        .collect())
}

/// `encodeURIComponent(value)`.
fn encode_uri_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `redactSearchError(error, apiKey)`.
pub fn redact_search_error(message: &JsString, api_key: &str) -> JsString {
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("key", api_key)
        .finish();
    let form = form.get(4..).unwrap_or("").to_string();
    let mut units_vec = message.units().to_vec();
    for secret in [api_key.to_string(), encode_uri_component(api_key), form] {
        if secret.is_empty() {
            continue;
        }
        let needle: Vec<u16> = secret.encode_utf16().collect();
        let replacement = units("[redacted]");
        let mut out = Vec::with_capacity(units_vec.len());
        let mut index = 0;
        while index < units_vec.len() {
            if units_vec[index..].starts_with(&needle) {
                out.extend_from_slice(&replacement);
                index += needle.len();
            } else {
                out.push(units_vec[index]);
                index += 1;
            }
        }
        units_vec = out;
    }
    units_vec.truncate(500);
    JsString::from_utf16(units_vec)
}

async fn additional_search(
    input: &JsValue,
    context: &ToolRunContext,
    options: &WebSearchOptions,
    api_key: &str,
    provider: &str,
    endpoint: &str,
) -> Result<ToolOutput, ToolError> {
    let query = query_of(input)?;
    if query.is_empty() {
        return Err(empty_query());
    }
    assert_http_endpoint(endpoint, provider)?;
    let limit = organic_limit(options);
    let label = format!("web_search ({provider})");
    let fail = |message: JsString| request_failed(&label, &redact_search_error(&message, api_key));
    let gl = gl_of(input);
    let request = additional_search_request(
        provider,
        endpoint,
        api_key,
        &query,
        limit,
        options.search_engine.as_deref(),
        gl.as_deref(),
    )
    .map_err(|message| fail(JsString::from(message.as_str())))?;
    let mut response = match send(options, context, &request, request.method == "POST").await {
        Ok(response) => response,
        Err(None) => return Err(timed_out(&label, options)),
        Err(Some(error)) => return Err(fail(error.message)),
    };
    if !response.ok() {
        let detail = response_text(&mut response, context)
            .await
            .map_err(|error| {
                fail(JsString::from(match error {
                    BodyError::Aborted => "Network request aborted by parent signal.",
                    BodyError::Terminated | BodyError::TooLarge => "terminated",
                }))
            })?;
        return Err(fail(JsString::from(
            format!("API error ({}): {detail}", response.status).as_str(),
        )));
    }
    let raw = response_json(&mut response, context)
        .await
        .map_err(|message| fail(JsString::from(message.as_str())))?;
    let organic = additional_search_results(provider, &raw, limit).map_err(fail)?;
    let mut metadata = object([
        ("provider", string(provider)),
        ("endpoint", string(endpoint)),
    ]);
    if provider == "serpapi" {
        metadata.insert(
            "engine".into(),
            string(options.search_engine.as_deref().unwrap_or("google")),
        );
    }
    metadata.insert("organicCount".into(), JsValue::Number(organic.len() as f64));
    output(&query, organic, None, metadata)
}
