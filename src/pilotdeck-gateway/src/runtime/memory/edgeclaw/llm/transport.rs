//! EdgeClaw structured HTTP calls (cc7c4e6 llm-extraction.ts:2261–2486).
//! D-2: a request's deadline also bounds its body. Successful body/JSON reads
//! stay outside retry/fallback, as in TS. Dropping a call drops all its work.
use super::{
    config::{ModelSelection, resolve_api_key, resolve_selection},
    envelope::truncate,
};
use crate::{
    fetch_redirect,
    runtime::{
        memory::js_trim_units,
        model::{
            http::{ModelBody, ModelHttpTransport},
            providers::google::normalize_model_id,
        },
        network::{Exchange, exchange_with_prearmed_parent, node_timer_duration},
    },
};
use bytes::Bytes;
use hyper::{
    HeaderMap, Method, Response,
    header::{HeaderName, HeaderValue},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, json_syntax, parse_json, stringify_json};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::time::Instant;

type FutureResult<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type Deadline = Option<(Instant, f64)>;
type RequestHeaders = HeaderMap<Vec<u8>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmError {
    pub name: &'static str,
    pub message: JsString,
    pub status: Option<u16>,
}
impl LlmError {
    pub fn new(message: impl Into<JsString>) -> Self {
        Self {
            name: "Error",
            message: message.into(),
            status: None,
        }
    }
    fn transient(&self) -> bool {
        if let Some(status) = self.status {
            return retryable_status(status);
        }
        static NETWORK: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(
            "(?i)(fetch failed|network|econnreset|econnrefused|etimedout|socket hang up|temporar|rate limit|too many requests)"
        ).expect("constant regex")
        });
        self.timeout() || NETWORK.is_match(&text(&self.message))
    }
    fn timeout(&self) -> bool {
        self.name == "AbortError" || text(&self.message).to_lowercase().contains("timeout")
    }
}
impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&text(&self.message))
    }
}
impl std::error::Error for LlmError {}
fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504)
}

/// Optional runtime seams. Sleep is awaited inline; no background worker or
/// detached auth callback is created. Telemetry is a separate follow-up package.
pub trait LlmRuntime: Send + Sync {
    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
    fn resolve_auth<'a>(
        &'a self,
        _provider: &'a JsString,
        _config: &'a JsValue,
    ) -> FutureResult<'a, Result<Option<JsValue>, LlmError>> {
        Box::pin(async { Ok(None) })
    }
    fn sleep(&self, ms: u64) -> FutureResult<'_, ()> {
        Box::pin(tokio::time::sleep(std::time::Duration::from_millis(ms)))
    }
}
impl LlmRuntime for () {}

#[derive(Clone, Debug)]
pub struct StructuredInput {
    pub system_prompt: JsString,
    pub user_prompt: JsString,
    pub agent_id: Option<JsString>,
    pub request_label: JsString,
    pub timeout_ms: Option<f64>,
}

pub struct LlmMemoryExtractor {
    config: JsValue,
    transport: Arc<dyn ModelHttpTransport>,
    runtime: Arc<dyn LlmRuntime>,
    transport_timeout: Duration,
}
impl LlmMemoryExtractor {
    pub fn new(
        config: JsValue,
        transport: Arc<dyn ModelHttpTransport>,
        runtime: Arc<dyn LlmRuntime>,
    ) -> Self {
        Self {
            config,
            transport,
            runtime,
            transport_timeout: Duration::from_secs(300),
        }
    }
    /// Match the host dispatcher: Node defaults to 300 s; the CLI installs 600 s.
    /// This bounds headers and each body idle gap even when timeout_ms <= 0.
    pub fn with_transport_timeout(mut self, timeout: Duration) -> Self {
        self.transport_timeout = timeout;
        self
    }
    pub async fn call_structured_json(
        &self,
        input: &StructuredInput,
    ) -> Result<JsString, LlmError> {
        let selection = resolve_selection(&self.config, input.agent_id.as_ref())
            .map_err(|e| LlmError::new(e.message))?;
        if selection.base_url.as_ref().is_none_or(JsString::is_empty) {
            return Err(LlmError::new(join(&[
                &input.request_label,
                &" provider \"".into(),
                &selection.provider,
                &"\" does not have a baseUrl".into(),
            ])));
        }
        let env = |name: &str| self.runtime.env(name);
        let api_key = match resolve_api_key(&self.config, &selection.provider, &env, None) {
            Ok(key) => key,
            Err(_) => {
                let auth = self
                    .runtime
                    .resolve_auth(&selection.provider, &self.config)
                    .await?;
                resolve_api_key(&self.config, &selection.provider, &env, auth.as_ref())
                    .map_err(|e| LlmError::new(e.message))?
            }
        };
        let (url, headers, body, api) = request(&selection, &api_key, input)?;
        let response = match self.retry(&url, &headers, &body, input).await {
            Ok(response) => Ok(response),
            Err(_) if body.get("response_format").is_some() => {
                let mut fallback = JsObject::new();
                for (key, value) in body.as_object().expect("request object").entries() {
                    if !key.equals_str("response_format") {
                        fallback.insert(key.clone(), value.clone());
                    }
                }
                let fallback = JsValue::Object(fallback);
                self.retry(&url, &headers, &fallback, input).await
            }
            Err(error) => Err(error),
        }?;
        let bytes = read_body(
            response.0.into_body(),
            response.1,
            input,
            self.transport_timeout,
        )
        .await?;
        // undici's Response.json() and TextDecoder each remove one BOM.
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
        let decoded = String::from_utf8_lossy(bytes);
        let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(&decoded);
        let payload = parse_json(decoded).map_err(|error| LlmError {
            name: "SyntaxError",
            status: None,
            message: json_syntax::parse_error_units(&decoded.encode_utf16().collect::<Vec<_>>())
                .unwrap_or_else(|| error.to_string().into()),
        })?;
        extract_text(&api, &payload)
    }
    pub async fn call_structured_json_with_debug(
        &self,
        input: &StructuredInput,
        parse: impl FnOnce(&JsString) -> Result<JsValue, LlmError>,
        mut debug: impl FnMut(JsValue),
    ) -> Result<JsValue, LlmError> {
        let mut raw = JsString::default();
        let result = match self.call_structured_json(input).await {
            Ok(value) => {
                raw = value;
                parse(&raw)
            }
            Err(error) => Err(error),
        };
        let mut record = fields([
            ("requestLabel", string(&input.request_label)),
            ("systemPrompt", string(&input.system_prompt)),
            ("userPrompt", string(&input.user_prompt)),
            ("rawResponse", string(&raw)),
        ]);
        match &result {
            Ok(value) => {
                record.insert("parsedResult".into(), value.clone());
            }
            Err(error) => {
                record.insert("errored".into(), JsValue::Bool(true));
                record.insert(
                    "timedOut".into(),
                    JsValue::Bool(
                        error.timeout()
                            || text(&error.message).to_lowercase().contains("timed out"),
                    ),
                );
                record.insert("errorMessage".into(), string(&error.message));
            }
        }
        debug(JsValue::Object(record));
        result
    }
    async fn retry(
        &self,
        url: &str,
        headers: &RequestHeaders,
        body: &JsValue,
        input: &StructuredInput,
    ) -> Result<(Response<ModelBody>, Deadline), LlmError> {
        let body = Bytes::from(
            stringify_json(body)
                .expect("serializable request")
                .expect("request object"),
        );
        for attempt in 0..3 {
            let timeout = input
                .timeout_ms
                .filter(|v| v.is_finite())
                .unwrap_or(30_000.0);
            let deadline =
                (timeout > 0.0).then(|| (Instant::now() + node_timer_duration(timeout), timeout));
            let exchange = async {
                let parsed = url::Url::parse(url)
                    .map_err(|_| type_error(format!("Failed to parse URL from {url}")))?;
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err(type_error(format!(
                        "Request cannot be constructed from a URL that includes credentials: {url}"
                    )));
                }
                // Node Headers permits some bytes (e.g. 0x01 / DEL) that
                // undici only rejects at dispatch, inside the retry loop.
                let mut wire_headers = HeaderMap::new();
                for (name, value) in headers {
                    wire_headers.insert(
                        name,
                        HeaderValue::from_bytes(value).map_err(|_| type_error("fetch failed"))?,
                    );
                }
                fetch_redirect::follow(
                    parsed,
                    Method::POST,
                    wire_headers,
                    body.clone(),
                    || type_error("fetch failed"),
                    |mut request| async {
                        // Undici derives these for every hop from the original
                        // headers, so redirects cannot retain the previous scheme
                        // default or append identity repeatedly.
                        let secure = request.uri().scheme_str() == Some("https");
                        let headers = request.headers_mut();
                        if headers.contains_key("range") {
                            let value = if let Some(value) = headers.get("accept-encoding") {
                                let mut bytes = value.as_bytes().to_vec();
                                bytes.extend(b", identity");
                                HeaderValue::from_bytes(&bytes).expect("validated encoding")
                            } else {
                                HeaderValue::from_static("identity")
                            };
                            headers.insert("accept-encoding", value);
                        }
                        if !headers.contains_key("accept-encoding") {
                            headers.insert(
                                "accept-encoding",
                                HeaderValue::from_static(if secure {
                                    "br, gzip, deflate"
                                } else {
                                    "gzip, deflate"
                                }),
                            );
                        }
                        match exchange_with_prearmed_parent(
                            self.transport.send(request),
                            wait_deadline(deadline),
                            || deadline.map(|(at, _)| at),
                            Some(Instant::now() + self.transport_timeout),
                        )
                        .await
                        {
                            Exchange::Response(result, _) => {
                                result.map_err(|e| type_error(e.message))
                            }
                            Exchange::Timeout { .. } => Err(type_error("fetch failed")),
                            Exchange::Aborted => {
                                Err(timeout_error(input, deadline.expect("request deadline").1))
                            }
                        }
                    },
                )
                .await
            };
            let result = async {
                let response = exchange.await?;
                if response.status().is_success() {
                    return Ok((response, deadline));
                }
                let status = response.status().as_u16();
                let bytes = read_body(
                    response.into_body(),
                    deadline,
                    input,
                    self.transport_timeout,
                )
                .await?;
                let decoded = String::from_utf8_lossy(&bytes);
                let message = truncate(
                    &decoded.strip_prefix('\u{feff}').unwrap_or(&decoded).into(),
                    300.0,
                );
                Err(LlmError {
                    status: Some(status),
                    ..LlmError::new(join(&[
                        &input.request_label,
                        &format!(" request failed ({status}): ").into(),
                        &message,
                    ]))
                })
            }
            .await;
            match result {
                Ok(response) => return Ok(response),
                Err(error) if attempt == 2 || !error.transient() => return Err(error),
                Err(_) => self.runtime.sleep(1000 * (1 << attempt)).await,
            }
        }
        unreachable!("third attempt always returns")
    }
}
async fn wait_deadline(deadline: Deadline) {
    match deadline {
        Some((at, _)) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
fn timeout_error(input: &StructuredInput, ms: f64) -> LlmError {
    let ms = stringify_json(&JsValue::Number(ms)).unwrap().unwrap();
    LlmError::new(join(&[
        &input.request_label,
        &format!(" request timed out after {ms}ms").into(),
    ]))
}
fn type_error(message: impl Into<JsString>) -> LlmError {
    LlmError {
        name: "TypeError",
        ..LlmError::new(message)
    }
}
async fn read_body(
    body: ModelBody,
    deadline: Deadline,
    input: &StructuredInput,
    idle: Duration,
) -> Result<Vec<u8>, LlmError> {
    fetch_redirect::read_text_body_with_deadline(
        body,
        fetch_redirect::MAX_STRING_LENGTH,
        Some(idle),
        deadline.map(|(at, _)| at),
    )
    .await
    .map_err(|error| match error {
        fetch_redirect::BodyError::Aborted => {
            timeout_error(input, deadline.expect("body deadline").1)
        }
        fetch_redirect::BodyError::TooLong => type_error(fetch_redirect::STRING_TOO_LONG),
        fetch_redirect::BodyError::Terminated => type_error("terminated"),
    })
}
fn fields<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    entries
        .into_iter()
        .fold(JsObject::new(), |mut out, (key, value)| {
            out.insert(key.into(), value);
            out
        })
}
fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    JsValue::Object(fields(entries))
}
fn string(value: &JsString) -> JsValue {
    JsValue::String(value.clone())
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
fn join(parts: &[&JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|v| v.units().iter().copied())
            .collect(),
    )
}
fn trimmed(value: &JsString) -> JsString {
    JsString::from_utf16(js_trim_units(value.units()).to_vec())
}
fn array(value: Option<&JsValue>) -> &[JsValue] {
    match value {
        Some(JsValue::Array(v)) => v,
        _ => &[],
    }
}
fn field_text(value: &JsValue, key: &str) -> Option<JsString> {
    value.as_object()?.get(key)?.as_string().cloned()
}
fn texts(parts: &[JsValue], filter_empty: bool) -> JsString {
    let chunks: Vec<_> = parts
        .iter()
        .filter_map(|p| field_text(p, "text"))
        .filter(|s| !filter_empty || !s.is_empty())
        .collect();
    JsString::from_utf16(
        chunks
            .iter()
            .map(|s| s.units())
            .collect::<Vec<_>>()
            .join(&10),
    )
}
fn is_record(value: &JsValue) -> bool {
    matches!(value, JsValue::Object(_) | JsValue::Array(_))
}
/// Extractors keep UTF-16 strings intact, including content-array empty lines.
pub fn extract_text(api: &str, payload: &JsValue) -> Result<JsString, LlmError> {
    let (key, invalid, empty) = match api {
        "responses" | "openai-responses" => {
            if !is_record(payload) {
                return Err(LlmError::new("Invalid responses payload"));
            }
            if let Some(value) = field_text(payload, "output_text")
                && !trimmed(&value).is_empty()
            {
                return Ok(value);
            }
            (
                "output",
                "Responses payload missing output",
                "Responses payload did not contain text",
            )
        }
        "anthropic" => (
            "content",
            "Invalid Anthropic messages payload",
            "Anthropic messages payload did not contain text",
        ),
        "google" => (
            "candidates",
            "Invalid Google generateContent payload",
            "Google generateContent payload did not contain text",
        ),
        _ => ("choices", "Invalid chat completions payload", ""),
    };
    let Some(JsValue::Array(items)) = payload.get(key) else {
        return Err(LlmError::new(invalid));
    };
    if key == "choices" {
        let message = items
            .first()
            .and_then(|v| v.get("message"))
            .filter(|v| is_record(v))
            .ok_or_else(|| LlmError::new("Missing chat completion message"))?;
        return match message.get("content") {
            Some(JsValue::String(s)) => Ok(s.clone()),
            Some(JsValue::Array(parts)) => Ok(texts(parts, true)),
            _ => Err(LlmError::new("Unsupported chat completion content shape")),
        };
    }
    let parts: Vec<_> = items
        .iter()
        .flat_map(|item| match key {
            "output" => array(item.get("content")),
            "candidates" => array(item.get("content").and_then(|v| v.get("parts"))),
            _ => std::slice::from_ref(item),
        })
        .cloned()
        .collect();
    let value = trimmed(&texts(&parts, key == "content"));
    if value.is_empty() {
        Err(LlmError::new(empty))
    } else {
        Ok(value)
    }
}

fn byte_string(value: &JsString) -> Result<Vec<u8>, LlmError> {
    value.units().iter().enumerate().map(|(index,&unit)|u8::try_from(unit).map_err(|_|type_error(format!("Cannot convert argument to a ByteString because the character at index {index} has a value of {unit} which is greater than 255.")))).collect()
}
fn header_value(value: &JsString, method: &str) -> Result<Vec<u8>, LlmError> {
    header_bytes(&byte_string(value)?, method)
}
fn header_bytes(bytes: &[u8], method: &str) -> Result<Vec<u8>, LlmError> {
    let trim = |b: &u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n');
    let start = bytes.iter().position(|b| !trim(b)).unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !trim(b))
        .map_or(start, |i| i + 1);
    let bytes = &bytes[start..end];
    if bytes.iter().any(|b| matches!(b, 0 | b'\r' | b'\n')) {
        return Err(type_error(format!(
            "Headers.{method}: \"{}\" is an invalid header value.",
            bytes.iter().map(|b| char::from(*b)).collect::<String>()
        )));
    }
    Ok(bytes.to_vec())
}
fn header(map: &mut RequestHeaders, name: &'static str, value: JsString) -> Result<(), LlmError> {
    if !map.contains_key(name) {
        map.insert(name, header_value(&value, "set")?);
    }
    Ok(())
}

fn request(
    selection: &ModelSelection,
    api_key: &JsString,
    input: &StructuredInput,
) -> Result<(String, RequestHeaders, JsValue, String), LlmError> {
    let mut headers = RequestHeaders::default();
    if let Some(source) = &selection.headers {
        // WebIDL converts the whole record to ByteStrings before appending.
        let entries = source
            .own_entries()
            .into_iter()
            .map(|(name, value)| {
                Ok((
                    name,
                    byte_string(name)?,
                    byte_string(value.as_string().expect("sanitized header"))?,
                ))
            })
            .collect::<Result<Vec<_>, LlmError>>()?;
        for (name, key, bytes) in entries {
            let key = HeaderName::from_bytes(&key).map_err(|_| {
                type_error(format!(
                    "Headers.append: \"{}\" is an invalid header name.",
                    text(name)
                ))
            })?;
            let mut value = header_bytes(&bytes, "append")?;
            if let Some(old) = headers.get(&key) {
                let mut joined = old.clone();
                joined.extend(b", ");
                joined.extend(&value);
                value = joined;
            }
            headers.insert(key, value);
        }
    }
    header(&mut headers, "content-type", "application/json".into())?;
    let mut api = text(&trimmed(&selection.api)).to_lowercase();
    if api == "gemini" {
        api = "google".into();
    }
    let base = text(selection.base_url.as_ref().expect("base URL checked"));
    let mut body = fields([("model", string(&selection.model))]);
    let message =
        |role: &str, value: &JsString| object([("role", role.into()), ("content", string(value))]);
    let user = message("user", &input.user_prompt);
    let messages = JsValue::Array(vec![message("system", &input.system_prompt), user.clone()]);
    let url = match api.as_str() {
        "responses" | "openai-responses" => {
            header(
                &mut headers,
                "authorization",
                join(&[&"Bearer ".into(), api_key]),
            )?;
            body.insert("input".into(), messages);
            format!("{base}/responses")
        }
        "anthropic" => {
            header(&mut headers, "x-api-key", api_key.clone())?;
            header(&mut headers, "anthropic-version", "2023-06-01".into())?;
            body.insert("max_tokens".into(), JsValue::Number(32768.0));
            body.insert("system".into(), string(&input.system_prompt));
            body.insert("messages".into(), JsValue::Array(vec![user]));
            format!("{base}/v1/messages")
        }
        "google" => {
            header(&mut headers, "x-goog-api-key", api_key.clone())?;
            let parts = |value: &JsString| JsValue::Array(vec![object([("text", string(value))])]);
            let user = object([
                ("role", "user".into()),
                ("parts", parts(&input.user_prompt)),
            ]);
            body = fields([
                (
                    "systemInstruction",
                    object([("parts", parts(&input.system_prompt))]),
                ),
                ("contents", JsValue::Array(vec![user])),
                (
                    "generationConfig",
                    object([("responseMimeType", "application/json".into())]),
                ),
            ]);
            google_url(
                &base,
                &selection.model.to_utf8().map_err(|_| LlmError {
                    name: "URIError",
                    ..LlmError::new("URI malformed")
                })?,
            )?
        }
        _ => {
            header(
                &mut headers,
                "authorization",
                join(&[&"Bearer ".into(), api_key]),
            )?;
            body.insert("stream".into(), JsValue::Bool(false));
            body.insert(
                "response_format".into(),
                object([("type", "json_object".into())]),
            );
            body.insert("messages".into(), messages);
            format!("{base}/chat/completions")
        }
    };
    // Node 22 fetch's defaults are observable at the local HTTP server.
    for (name, value) in [
        ("accept", "*/*"),
        ("accept-language", "*"),
        ("sec-fetch-mode", "cors"),
        ("user-agent", "node"),
        ("connection", "keep-alive"),
    ] {
        header(&mut headers, name, value.into())?;
    }
    headers.insert("sec-fetch-mode", b"cors".to_vec());
    Ok((url, headers, JsValue::Object(body), api))
}
fn google_url(base: &str, model: &str) -> Result<String, LlmError> {
    let mut url = url::Url::parse(base).map_err(|_| type_error("Invalid URL"))?;
    let mut parts: Vec<_> = url
        .path()
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let version = if parts.last().is_some_and(|s| s == "v1" || s == "v1beta") {
        parts.pop().expect("last")
    } else {
        "v1beta".into()
    };
    parts.push(version);
    parts.push("models".into());
    let encoded = normalize_model_id(model)
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    parts.push(format!("{encoded}:generateContent"));
    url.set_path(&format!("/{}", parts.join("/")));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}
