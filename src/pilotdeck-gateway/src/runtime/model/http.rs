//! Provider streaming HTTP layer. Ported from PilotDeck `cc7c4e65`
//! `src/model/streaming/streamModel.ts` (HTTP and Google SDK paths),
//! `StreamingCheckpoint.ts`, `continuationRequest.ts`, `requestFingerprint.ts`,
//! `policy/RetryPolicy.ts` and the provider-facing half of `network/fetch.ts`.
//!
//! The request is the canonical request JSON (e.g. the output of
//! [`RouterRuntime::materialize_request`](super::router::RouterRuntime::materialize_request)),
//! the config is a `ModelConfig` JSON (`{ providers: { id: ProviderConfig } }`).
//! Events are delivered to a sink in order; an `Err` is what TS throws.
//!
//! HTTP redirects follow undici through `crate::fetch_redirect::follow`
//! (#360-1), including the Google SDK transport. Not ported here: invocation
//! audit staging (`options.invocation`, A6.4-pr10), `PILOTDECK_DUMP_REQUEST` and the
//! proxy/global dispatcher.
#![allow(clippy::result_large_err)]

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{Request, Response};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use sha2::{Digest, Sha256};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use super::count_body::{field, is, put, text as header_text};
use super::providers::{self, StreamState};
use super::router::AbortFlag;
use crate::host::ModuleError;

mod errors;
mod sse;
pub use errors::NetworkFetchError;
pub(crate) use errors::retry_after_from_message;

/// `parseRetryAfterHeader(value)` against the system clock (`Date.now()`).
pub(crate) fn retry_after_header_ms(value: Option<&str>) -> Option<f64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_millis() as f64);
    errors::retry_after_header(value, now)
}
use errors::{ErrorInput, is_premature_stream_close, normalize_model_error};

/// `Math.round`.
pub(crate) fn js_round(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let floor = value.floor();
    if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

/// An `Error` instance as `normalizeModelError` sees it: own `message`, an
/// optional own string `code`, and its `JSON.stringify` projection as `raw`.
#[derive(Clone, Debug)]
pub(crate) struct JsError {
    pub message: JsString,
    pub code: Option<JsString>,
    pub network_code: Option<&'static str>,
    pub raw: JsValue,
}

/// The `streamModel.ts` error classes that assign `this.name` and are
/// always retryable stream errors.
fn is_stream_guard_error(name: &JsString) -> bool {
    [
        "StreamIdleTimeoutError",
        "IncompleteStreamError",
        "MaxStreamingDurationError",
        "RepeatedStreamingChunkError",
    ]
    .iter()
    .any(|n| name.equals_str(n))
}

/// What `streamModel` throws.
#[derive(Clone, Debug)]
pub enum ModelStreamError {
    /// `ModelRequestError(code, message)`: local validation, never retried.
    Request { code: JsString, message: JsString },
    /// `ModelProviderError(canonical)`.
    Provider(JsObject),
    /// Any other `Error` (`TypeError: terminated`, `SyntaxError`, `AbortError`
    /// and the stream guard errors).
    Thrown { name: JsString, message: JsString },
}
impl ModelStreamError {
    fn thrown(name: &str, message: impl Into<JsString>) -> Self {
        Self::Thrown {
            name: name.into(),
            message: message.into(),
        }
    }
    fn abort() -> Self {
        // `new AbortController().abort()` reason, returned by createAbortError.
        Self::thrown("AbortError", "This operation was aborted")
    }
    pub fn name(&self) -> JsString {
        match self {
            Self::Request { .. } => "ModelRequestError".into(),
            Self::Provider(_) => "ModelProviderError".into(),
            Self::Thrown { name, .. } => name.clone(),
        }
    }
    pub fn message(&self) -> JsString {
        match self {
            Self::Request { message, .. } | Self::Thrown { message, .. } => message.clone(),
            Self::Provider(error) => error
                .get("message")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default(),
        }
    }
    fn message_text(&self) -> String {
        String::from_utf16_lossy(self.message().units())
    }
    /// `isAbortError`: every variant is an `Error`, so the message test also
    /// applies to provider/request errors (node probe: a ModelProviderError
    /// whose message contains "aborted" is an abort error).
    fn is_abort(&self) -> bool {
        self.name().equals_str("AbortError")
            || self.message_text().to_lowercase().contains("aborted")
    }
    fn as_js_error(&self) -> JsError {
        let name = self.name();
        // Custom stream errors assign `this.name`; built-ins serialize as `{}`.
        let mut raw = JsObject::new();
        if is_stream_guard_error(&name) {
            raw.insert("name".into(), JsValue::String(name.clone()));
        }
        JsError {
            message: self.message(),
            code: None,
            network_code: None,
            raw: JsValue::Object(raw),
        }
    }
}
impl From<ModelStreamError> for ModuleError {
    /// Host projection: canonical provider errors keep code, retryability and
    /// retry delay; request errors are never retryable.
    fn from(error: ModelStreamError) -> Self {
        match error {
            ModelStreamError::Request { code, message } => Self {
                message,
                code: Some(code),
                canonical: None,
                retryable: Some(false),
                retry_after_ms: None,
            },
            ModelStreamError::Provider(canonical) => Self {
                message: canonical
                    .get("message")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default(),
                code: canonical.get("code").and_then(JsValue::as_string).cloned(),
                retryable: canonical.get("retryable").and_then(JsValue::as_bool),
                retry_after_ms: canonical.get("retryAfterMs").and_then(JsValue::as_number),
                canonical: Some(canonical),
            },
            ModelStreamError::Thrown { message, .. } => Self::new(message),
        }
    }
}

// ---------------------------------------------------------------------------
// Transport

/// A failed exchange before response headers (undici `TypeError: fetch failed`).
#[derive(Clone, Debug)]
pub struct TransportFailure {
    /// `error.cause.code` as undici reports it (e.g. `ECONNREFUSED`).
    pub cause_code: Option<&'static str>,
    pub message: String,
}
impl TransportFailure {
    pub fn fetch_failed(cause_code: Option<&'static str>) -> Self {
        Self {
            cause_code,
            message: "fetch failed".into(),
        }
    }
}
/// A response body read failure (undici `TypeError: terminated`).
#[derive(Debug)]
pub struct BodyFailure;
impl std::fmt::Display for BodyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("terminated")
    }
}
impl std::error::Error for BodyFailure {}

pub type ModelBody = BoxBody<Bytes, BodyFailure>;
type TransportResult = Result<Response<ModelBody>, TransportFailure>;

/// An exchange with an observable completion source. A timer-backed transport
/// must use `after`, rather than hide a sleep in an opaque future: consumers
/// racing an abort timer need both deadlines when the executor resumes late.
/// Ordinary I/O is dispatched when polled; an already fulfilled exchange is a
/// microtask (`ready`). Dropping any variant also drops its body/connection.
pub struct TransportFuture {
    future: Pin<Box<dyn Future<Output = TransportResult> + Send>>,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
    microtask: bool,
}

impl TransportFuture {
    /// Socket I/O, without an application-level delayed completion.
    pub fn io(future: impl Future<Output = TransportResult> + Send + 'static) -> Self {
        Self {
            future: Box::pin(future),
            timer: None,
            microtask: false,
        }
    }

    pub fn ready(result: TransportResult) -> Self {
        Self::microtask(std::future::ready(result))
    }

    /// A promise continuation dispatched before the next timer callback.
    /// This must not hide a timer or socket wait; those have separate event
    /// sources. It may yield while its queued continuation is being delivered.
    pub fn microtask(future: impl Future<Output = TransportResult> + Send + 'static) -> Self {
        Self {
            future: Box::pin(future),
            timer: None,
            microtask: true,
        }
    }

    /// A timer callback that settles the exchange. Keep the timer here so
    /// callers can order it against their own timers, including overdue ones.
    pub fn after(delay: Duration, result: TransportResult) -> Self {
        Self {
            future: Box::pin(std::future::ready(result)),
            timer: Some(Box::pin(tokio::time::sleep(delay))),
            microtask: false,
        }
    }

    pub fn scheduled_at(&self) -> Option<tokio::time::Instant> {
        self.timer.as_ref().map(|timer| timer.deadline())
    }

    pub fn is_microtask(&self) -> bool {
        self.microtask
    }
}

impl Future for TransportFuture {
    type Output = TransportResult;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Some(timer) = &mut self.timer {
            std::task::ready!(timer.as_mut().poll(cx));
        }
        self.future.as_mut().poll(cx)
    }
}

/// One HTTP exchange. Dropping the future or the response body closes the
/// connection; nothing is detached.
pub trait ModelHttpTransport: Send + Sync {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture;
}

/// hyper + rustls (ring, TLS 1.2/1.3), HTTP/1.1, `http` and `https`.
#[derive(Clone)]
pub struct HyperModelTransport {
    client: Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>,
}
impl HyperModelTransport {
    /// Mozilla roots (`webpki-roots`), like Node's bundled CA store; the OS
    /// trust store and `NODE_EXTRA_CA_CERTS` are not consulted.
    pub fn new() -> Self {
        Self::from_config(webpki_client_config())
    }
    /// Trust exactly `roots` (tests use a throwaway CA).
    pub fn with_roots(roots: rustls::RootCertStore) -> Self {
        Self::from_config(rooted_client_config(roots))
    }
    fn builder() -> rustls::ConfigBuilder<rustls::ClientConfig, rustls::WantsVerifier> {
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
    }
    fn from_config(config: rustls::ClientConfig) -> Self {
        Self {
            client: Client::builder(TokioExecutor::new()).build(https_connector(config)),
        }
    }
}

/// Shared by the stream and count transports: rustls (ring) for `https`,
/// plain TCP for `http`, HTTP/1.1. The connect timeout is undici's default
/// `connectTimeout` (10 s); PilotDeck's global dispatcher does not override it.
pub(crate) fn https_connector(
    config: rustls::ClientConfig,
) -> hyper_rustls::HttpsConnector<HttpConnector> {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_connect_timeout(Some(Duration::from_secs(10)));
    hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_or_http()
        .enable_http1()
        .wrap_connector(http)
}

/// rustls client config trusting the Mozilla roots (`webpki-roots`).
pub(crate) fn webpki_client_config() -> rustls::ClientConfig {
    use hyper_rustls::ConfigBuilderExt;
    HyperModelTransport::builder()
        .with_webpki_roots()
        .with_no_client_auth()
}
/// rustls client config trusting exactly `roots`.
pub(crate) fn rooted_client_config(roots: rustls::RootCertStore) -> rustls::ClientConfig {
    HyperModelTransport::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}
impl Default for HyperModelTransport {
    fn default() -> Self {
        Self::new()
    }
}
impl ModelHttpTransport for HyperModelTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let client = self.client.clone();
        TransportFuture::io(async move {
            let response = client
                .request(request)
                .await
                .map_err(|error| TransportFailure::fetch_failed(cause_code(&error)))?;
            Ok(response.map(|body| body.map_err(|_| BodyFailure).boxed()))
        })
    }
}

/// undici's `cause.code` for a hyper/rustls/io failure chain.
fn cause_code(error: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = current {
        if let Some(tls) = error.downcast_ref::<rustls::Error>() {
            return Some(match tls {
                rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName)
                | rustls::Error::InvalidCertificate(
                    rustls::CertificateError::NotValidForNameContext { .. },
                ) => "ERR_TLS_CERT_ALTNAME_INVALID",
                rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer) => {
                    "UNABLE_TO_GET_ISSUER_CERT_LOCALLY"
                }
                rustls::Error::InvalidCertificate(
                    rustls::CertificateError::Expired
                    | rustls::CertificateError::ExpiredContext { .. },
                ) => "CERT_HAS_EXPIRED",
                rustls::Error::InvalidCertificate(_) => "UNABLE_TO_VERIFY_LEAF_SIGNATURE",
                _ => "ERR_SSL_PROTOCOL_ERROR",
            });
        }
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            if let Some(code) = io.get_ref().and_then(|inner| {
                let inner: &(dyn std::error::Error + 'static) = inner;
                cause_code(inner)
            }) {
                return Some(code);
            }
            return match io.kind() {
                std::io::ErrorKind::ConnectionRefused => Some("ECONNREFUSED"),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted => {
                    Some("ECONNRESET")
                }
                std::io::ErrorKind::TimedOut => Some("ETIMEDOUT"),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::BrokenPipe => {
                    Some("UND_ERR_SOCKET")
                }
                _ if io.to_string().contains("failed to lookup address") => Some("ENOTFOUND"),
                _ => None,
            };
        }
        if let Some(hyper) = error.downcast_ref::<hyper::Error>()
            && (hyper.is_incomplete_message() || hyper.is_closed())
        {
            return Some("UND_ERR_SOCKET");
        }
        current = error.source();
    }
    None
}

// ---------------------------------------------------------------------------
// Options and policy

pub enum StreamOutput {
    Event(JsValue),
    /// `onRetryProgress` payload.
    RetryProgress(JsValue),
}

#[derive(Clone)]
pub struct StreamOptions {
    pub abort: Option<AbortFlag>,
    /// `options.streamTimeoutMs`.
    pub stream_timeout_ms: Option<f64>,
    /// `Math.random` for retry jitter.
    pub random: Arc<dyn Fn() -> f64 + Send + Sync>,
    /// `Date.now` for Retry-After dates and the max-duration guard.
    pub clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    /// undici `bodyTimeout` of the global dispatcher (PilotDeck installs
    /// 600000 ms in `cli/proxy.ts`): a body read that sees no bytes for this
    /// long fails with `TypeError: terminated`.
    pub body_timeout_ms: f64,
}
impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            abort: None,
            stream_timeout_ms: None,
            random: Arc::new(|| {
                (uuid::Uuid::new_v4().as_u128() >> 75) as f64 / (1u64 << 53) as f64
            }),
            clock: Arc::new(super::router::wall_clock),
            body_timeout_ms: UNDICI_BODY_TIMEOUT_MS,
        }
    }
}

const DEFAULT_STREAM_IDLE_TIMEOUT_MS: f64 = 600_000.0;
/// `UNDICI_TRANSPORT_TIMEOUT_MS` (`cli/proxy.ts`, = LITELLM_COMPLETION_HTTP_FALLBACK_MS).
pub const UNDICI_BODY_TIMEOUT_MS: f64 = 600_000.0;
const REPEATED_CHUNK_LIMIT: f64 = 100.0;

fn retry_number(provider: &JsValue, key: &str) -> Option<f64> {
    match field(field(provider, "retry"), key) {
        JsValue::Undefined | JsValue::Null => None,
        value => Some(crate::js_value::to_number(value).unwrap_or(f64::NAN)),
    }
}

/// `createNativeRetryPolicy({ defaultMaxRetries: 2 }).decide({ kind: "stream" })`.
fn retry_decide(
    provider: &JsValue,
    attempt: f64,
    retry_after: Option<f64>,
    random: f64,
) -> (f64, f64) {
    let max_retries = retry_number(provider, "streamMaxRetries").unwrap_or(2.0);
    let max_delay = retry_number(provider, "maxDelayMs").unwrap_or(8_000.0);
    if let Some(after) = retry_after {
        return (max_retries, after.min(max_delay));
    }
    let base = retry_number(provider, "baseDelayMs").unwrap_or(500.0);
    let jitter = retry_number(provider, "jitter").unwrap_or(0.75);
    let deterministic = base * (attempt + 1.0);
    (
        max_retries,
        (deterministic + deterministic * jitter * random).min(max_delay),
    )
}

/// `resolveStreamIdleTimeout(provider, options)`.
fn idle_timeout_ms(provider: &JsValue, options: &StreamOptions) -> f64 {
    if let Some(ms) = options.stream_timeout_ms.filter(|ms| *ms > 0.0) {
        return ms;
    }
    match field(field(provider, "retry"), "streamIdleTimeoutMs") {
        JsValue::Number(ms) if *ms > 0.0 => *ms,
        _ => DEFAULT_STREAM_IDLE_TIMEOUT_MS,
    }
}

fn duration(ms: f64) -> Duration {
    Duration::from_secs_f64((ms.max(0.0) / 1000.0).min(1e9))
}

async fn wait_abort(abort: &Option<AbortFlag>) {
    match abort {
        Some(flag) => flag.cancelled().await,
        None => std::future::pending().await,
    }
}
fn aborted(options: &StreamOptions) -> bool {
    options.abort.as_ref().is_some_and(AbortFlag::is_aborted)
}
fn throw_if_aborted(options: &StreamOptions) -> Result<(), ModelStreamError> {
    if aborted(options) {
        Err(ModelStreamError::abort())
    } else {
        Ok(())
    }
}
/// `delay(ms, signal)`.
async fn delay(ms: f64, options: &StreamOptions) -> Result<(), ModelStreamError> {
    throw_if_aborted(options)?;
    tokio::select! {
        _ = tokio::time::sleep(duration(ms)) => Ok(()),
        _ = wait_abort(&options.abort) => Err(ModelStreamError::abort()),
    }
}

// ---------------------------------------------------------------------------
// Streaming checkpoint

#[derive(Default)]
struct Checkpoint {
    partial_text: Vec<u16>,
    has_tool_calls: bool,
    has_reasoning: bool,
    active: Vec<(JsString, JsValue, f64)>,
}
impl Checkpoint {
    /// `StreamingCheckpointManager.onEvent`; `partialText += text` throws
    /// past V8's string limit.
    fn on_event(&mut self, event: &JsValue) -> Result<(), ModelStreamError> {
        let tag = field(event, "type");
        let string = |key: &str| {
            JsString::from_utf16(
                crate::js_value::to_string_units(field(event, key)).unwrap_or_default(),
            )
        };
        if is(tag, "text_delta") {
            append_js_string(&mut self.partial_text, string("text").units())?;
        } else if is(tag, "thinking_delta") {
            self.has_reasoning = true;
        } else if is(tag, "tool_call_start") {
            self.has_tool_calls = true;
            self.set_active(string("id"), field(event, "name").clone(), 0.0);
        } else if is(tag, "tool_call_delta") {
            self.has_tool_calls = true;
            let id = string("id");
            let added = string("delta").len_utf16() as f64;
            match self.active.iter_mut().find(|(k, _, _)| *k == id) {
                Some(entry) => entry.2 += added,
                None => self.active.push((id, "".into(), added)),
            }
        } else if is(tag, "tool_call_end") {
            self.has_tool_calls = true;
            let id = JsString::from_utf16(
                crate::js_value::to_string_units(field(field(event, "toolCall"), "id"))
                    .unwrap_or_default(),
            );
            self.active.retain(|(k, _, _)| *k != id);
        }
        Ok(())
    }
    fn set_active(&mut self, id: JsString, name: JsValue, chars: f64) {
        match self.active.iter_mut().find(|(k, _, _)| *k == id) {
            Some(entry) => {
                entry.1 = name;
                entry.2 = chars;
            }
            None => self.active.push((id, name, chars)),
        }
    }
    fn has_text(&self) -> bool {
        let text = String::from_utf16_lossy(&self.partial_text);
        !crate::metadata::js_trim(&text).is_empty()
    }
    fn can_continue_text(&self) -> bool {
        self.has_text() && !self.has_reasoning && !self.has_tool_calls
    }
    fn is_empty(&self) -> bool {
        !self.has_tool_calls && !self.has_text() && !self.has_reasoning
    }
    fn interruption(&self) -> JsValue {
        let mut out = JsObject::new();
        if self.has_tool_calls {
            out.insert("phase".into(), "tool_call".into());
            let calls = self
                .active
                .iter()
                .map(|(id, name, chars)| {
                    let mut call = JsObject::new();
                    call.insert("id".into(), JsValue::String(id.clone()));
                    call.insert("name".into(), name.clone());
                    call.insert("argumentChars".into(), JsValue::Number(*chars));
                    JsValue::Object(call)
                })
                .collect();
            out.insert("activeToolCalls".into(), JsValue::Array(calls));
        } else if self.has_text() {
            out.insert("phase".into(), "text".into());
        } else if self.has_reasoning {
            out.insert("phase".into(), "reasoning".into());
        } else {
            out.insert("phase".into(), "empty".into());
        }
        JsValue::Object(out)
    }
}

const CONTINUATION_INSTRUCTION: &str = "You are a helpful assistant. You are given a message and you need to respond to it. You are also given a generated content. You need to respond to the message in continuation of the generated content. Do not repeat the same content. Your response should be in continuation of this text:";

fn text_message(role: &str, text: JsValue) -> JsValue {
    let mut block = JsObject::new();
    block.insert("type".into(), "text".into());
    block.insert("text".into(), text);
    let mut message = JsObject::new();
    message.insert("role".into(), role.into());
    message.insert(
        "content".into(),
        JsValue::Array(vec![JsValue::Object(block)]),
    );
    JsValue::Object(message)
}

/// `buildLiteLLMContinuationRequest(original, partialText)`.
fn continuation_request(original: &JsValue, partial: &[u16]) -> JsValue {
    let mut messages = match field(original, "messages") {
        JsValue::Array(messages) => messages.clone(),
        _ => Vec::new(),
    };
    // stripLiteLLMContinuationMessages
    if messages.len() >= 2 {
        let last = &messages[messages.len() - 1];
        let second = &messages[messages.len() - 2];
        let content = match field(last, "content") {
            JsValue::Array(content) => content.as_slice(),
            _ => &[],
        };
        if is(field(last, "role"), "user")
            && is(field(second, "role"), "assistant")
            && content.len() == 1
            && is(field(&content[0], "type"), "text")
            && is(field(&content[0], "text"), CONTINUATION_INSTRUCTION)
        {
            messages.truncate(messages.len() - 2);
        }
    }
    messages.push(text_message(
        "assistant",
        JsValue::String(JsString::from_utf16(partial.to_vec())),
    ));
    messages.push(text_message("user", CONTINUATION_INSTRUCTION.into()));
    let mut out = original.clone();
    put(&mut out, "messages", JsValue::Array(messages));
    put(&mut out, "cacheBreakpoints", JsValue::Undefined);
    put(&mut out, "cachePlan", JsValue::Undefined);
    out
}

// ---------------------------------------------------------------------------
// Errors as streamModel classifies them

fn provider_id(provider: &JsValue) -> JsString {
    field(provider, "id")
        .as_string()
        .cloned()
        .unwrap_or_default()
}
fn protocol(provider: &JsValue) -> JsString {
    field(provider, "protocol")
        .as_string()
        .cloned()
        .unwrap_or_default()
}

/// `isRetryableStreamError`.
fn is_retryable_stream_error(error: &ModelStreamError) -> bool {
    if error.is_abort() {
        return false;
    }
    match error {
        ModelStreamError::Provider(canonical) => {
            canonical.get("retryable") == Some(&JsValue::Bool(true))
                || is_premature_stream_close(&error.message_text())
        }
        ModelStreamError::Request { .. } => {
            let message = error.message_text().to_lowercase();
            retryable_message(&message) || is_premature_stream_close(&error.message_text())
        }
        ModelStreamError::Thrown { name, .. } => {
            if is_stream_guard_error(name) {
                return true;
            }
            let message = error.message_text().to_lowercase();
            retryable_message(&message) || is_premature_stream_close(&error.message_text())
        }
    }
}
fn retryable_message(message: &str) -> bool {
    [
        "network",
        "econnreset",
        "socket hang up",
        "fetch failed",
        "aborted",
        "timeout",
        "epipe",
        "econnrefused",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

/// `toProviderError(provider, error).error`.
fn provider_error(provider: &JsValue, error: &ModelStreamError) -> JsObject {
    match error {
        ModelStreamError::Provider(canonical) => canonical.clone(),
        other => {
            let js = other.as_js_error();
            normalize_model_error(
                &provider_id(provider),
                &protocol(provider),
                ErrorInput::Error(&js),
                None,
            )
        }
    }
}
/// `streamInterruptionError`.
fn interruption_error(
    provider: &JsValue,
    error: &ModelStreamError,
    checkpoint: &Checkpoint,
) -> JsValue {
    let mut canonical = provider_error(provider, error);
    canonical.insert("streamInterruption".into(), checkpoint.interruption());
    JsValue::Object(canonical)
}
fn error_event(error: JsValue) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type".into(), "error".into());
    event.insert("error".into(), error);
    JsValue::Object(event)
}
fn retry_reason(error: &ModelStreamError) -> &'static str {
    match error {
        ModelStreamError::Provider(canonical)
            if canonical
                .get("code")
                .and_then(JsValue::as_string)
                .is_some_and(|c| c.equals_str("server_error")) =>
        {
            "server_error"
        }
        _ => "network_error",
    }
}
fn retry_after(error: &ModelStreamError) -> Option<f64> {
    match error {
        ModelStreamError::Provider(canonical) => {
            canonical.get("retryAfterMs").and_then(JsValue::as_number)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Request sending

/// `new Request(url, init)` validation plus `new Headers(record)`: the
/// initial URL and header map that `fetch_redirect::follow` then sends,
/// re-checking scheme and bad ports on every hop.
fn build_http_request(
    url: &str,
    headers: &[(JsString, JsValue)],
) -> Result<(url::Url, hyper::HeaderMap), NetworkFetchError> {
    let failed = || NetworkFetchError::classify("fetch failed", None);
    let parsed = url::Url::parse(url).map_err(|_| {
        NetworkFetchError::classify(&format!("Failed to parse URL from {url}"), None)
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(NetworkFetchError::classify(
            &format!("Request cannot be constructed from a URL that includes credentials: {url}"),
            None,
        ));
    }
    // `new Headers(record)`: names are case-insensitive, later values append.
    let mut map = hyper::HeaderMap::new();
    for (name, value) in headers {
        let name = hyper::header::HeaderName::from_bytes(
            String::from_utf16_lossy(name.units()).as_bytes(),
        )
        .map_err(|_| NetworkFetchError::classify("Invalid header name", None))?;
        let text = header_text(value);
        let value =
            hyper::header::HeaderValue::from_str(text.trim_matches([' ', '\t', '\r', '\n']))
                .map_err(|_| NetworkFetchError::classify("Invalid header value", None))?;
        if let Some(existing) = map.get(&name) {
            let joined = format!(
                "{}, {}",
                existing.to_str().unwrap_or_default(),
                value.to_str().unwrap_or_default()
            );
            map.insert(
                name,
                hyper::header::HeaderValue::from_str(&joined).map_err(|_| failed())?,
            );
        } else {
            map.insert(name, value);
        }
    }
    Ok((parsed, map))
}

/// `sendProviderRequest` + `sendWithEndpointFallback` + `networkFetch`.
async fn send_provider_request(
    transport: &dyn ModelHttpTransport,
    provider: &JsValue,
    body: &JsValue,
    options: &StreamOptions,
) -> Result<Response<ModelBody>, ModelStreamError> {
    let timeout_ms = idle_timeout_ms(provider, options);
    let final_body = providers::final_body(body, provider);
    let headers = providers::provider_headers(provider, &final_body);
    let encoded = crate::js_value::json_stringify(&final_body)
        .map_err(|_| {
            ModelStreamError::thrown("RangeError", crate::js_value::StackOverflow::MESSAGE)
        })?
        .unwrap_or_default();
    let encoded = Bytes::from(encoded);
    let endpoints = providers::chat_endpoint_candidates(provider);
    let count = endpoints.len();
    let to_provider = |error: NetworkFetchError| {
        ModelStreamError::Provider(normalize_model_error(
            &provider_id(provider),
            &protocol(provider),
            ErrorInput::Error(&error.as_js_error()),
            None,
        ))
    };
    let exchange = async {
        let mut last = None;
        for endpoint in &endpoints {
            let (url, header_map) = build_http_request(endpoint, &headers)?;
            // undici `redirect: "follow"` (shared with hooks and count).
            let response = crate::fetch_redirect::follow(
                url,
                hyper::Method::POST,
                header_map,
                encoded.clone(),
                || NetworkFetchError::classify("fetch failed", None),
                |request| async move {
                    transport.send(request).await.map_err(|failure| {
                        NetworkFetchError::classify(&failure.message, failure.cause_code)
                    })
                },
            )
            .await?;
            let status = response.status().as_u16();
            let ok = (200..300).contains(&status);
            if !ok && count != 1 && matches!(status, 400 | 404 | 405) {
                last = Some(response);
                continue;
            }
            return Ok(response);
        }
        last.ok_or_else(|| NetworkFetchError::classify("fetch failed", None))
    };
    tokio::select! {
        result = tokio::time::timeout(duration(timeout_ms), exchange) => match result {
            Ok(result) => result.map_err(to_provider),
            Err(_) => Err(to_provider(NetworkFetchError::new(
                "network_timeout",
                format!("Stream idle timeout: no data received for {}ms", crate::js_value::number_string(timeout_ms)),
                JsValue::Undefined,
            ))),
        },
        _ = wait_abort(&options.abort) => Err(ModelStreamError::abort()),
    }
}

/// `await response.text()`: UTF-8 with BOM stripped.
async fn response_text(
    body: &mut ModelBody,
    options: &StreamOptions,
) -> Result<String, ModelStreamError> {
    // TS reads this body after the request timer was cleared and the abort
    // listener detached; only undici's `bodyTimeout` bounds a stalled body
    // (node probe: `TypeError: terminated`, cause UND_ERR_BODY_TIMEOUT). The
    // caller's abort is still honoured here (safer than TS, which hangs on).
    let mut decoder = sse::Utf8Decoder::default();
    let mut text = String::new();
    let mut units = 0usize;
    loop {
        let frame = tokio::select! {
            frame = tokio::time::timeout(duration(options.body_timeout_ms), body.frame()) => frame,
            _ = wait_abort(&options.abort) => return Err(ModelStreamError::abort()),
        };
        match frame {
            Err(_) | Ok(Some(Err(_))) => {
                return Err(ModelStreamError::thrown("TypeError", "terminated"));
            }
            Ok(None) => break,
            Ok(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let start = text.len();
                    decoder.decode(data, &mut text);
                    // V8's cap counts UTF-16 code units of the decoded text.
                    units += text[start..].encode_utf16().count();
                    if units > super::count_http::MAX_STRING_LENGTH {
                        return Err(ModelStreamError::thrown(
                            "RangeError",
                            "Cannot create a string longer than 0x1fffffe8 characters",
                        ));
                    }
                }
            }
        }
    }
    decoder.finish(&mut text);
    Ok(text)
}

struct StreamGuard {
    started: f64,
    max_duration: Option<f64>,
    limit: f64,
    last: Option<JsString>,
    repeated: f64,
}
impl StreamGuard {
    fn new(provider: &JsValue, now: f64) -> Self {
        let env = std::env::var("LITELLM_MAX_STREAMING_DURATION_SECONDS")
            .ok()
            .filter(|raw| !crate::metadata::js_trim(raw).is_empty())
            .map(|raw| crate::js_value::string_to_number(&raw))
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(|v| v * 1000.0);
        Self {
            started: now,
            max_duration: retry_number(provider, "maxStreamingDurationMs").or(env),
            limit: retry_number(provider, "repeatedChunkLimit").unwrap_or(REPEATED_CHUNK_LIMIT),
            last: None,
            repeated: 1.0,
        }
    }
    fn check_duration(&self, now: f64) -> Result<(), ModelStreamError> {
        match self.max_duration {
            Some(max) if now - self.started > max => Err(ModelStreamError::thrown(
                "MaxStreamingDurationError",
                format!(
                    "Stream exceeded max streaming duration of {}ms",
                    crate::js_value::number_string(max)
                ),
            )),
            _ => Ok(()),
        }
    }
    fn observe(&mut self, event: &JsValue, now: f64) -> Result<(), ModelStreamError> {
        self.check_duration(now)?;
        let text = match (field(event, "type"), field(event, "text")) {
            (JsValue::String(t), JsValue::String(text))
                if t.equals_str("text_delta") && text.len_utf16() > 2 =>
            {
                text
            }
            _ => {
                self.repeated = 1.0;
                self.last = None;
                return Ok(());
            }
        };
        if self.last.as_ref() == Some(text) {
            self.repeated += 1.0;
        } else {
            self.last = Some(text.clone());
            self.repeated = 1.0;
        }
        if self.repeated >= self.limit {
            let mut message: Vec<u16> = "The model is repeating the same chunk = "
                .encode_utf16()
                .collect();
            message.extend(text.units());
            message.push(u16::from(b'.'));
            return Err(ModelStreamError::thrown(
                "RepeatedStreamingChunkError",
                JsString::from_utf16(message),
            ));
        }
        Ok(())
    }
}

fn object_entries(value: &JsValue) -> JsObject {
    value.as_object().cloned().unwrap_or_default()
}

/// Read one body frame under the idle timeout and the abort signal.
async fn read_frame(
    body: &mut ModelBody,
    idle_ms: f64,
    options: &StreamOptions,
) -> Result<Option<Bytes>, ModelStreamError> {
    // undici's bodyTimeout races the idle watchdog; the shorter one wins
    // (equal: the watchdog, armed per read like TS's `withIdleTimeout`).
    let body_first = options.body_timeout_ms < idle_ms;
    let wait = if body_first {
        options.body_timeout_ms
    } else {
        idle_ms
    };
    loop {
        let frame = tokio::select! {
            frame = tokio::time::timeout(duration(wait), body.frame()) => frame,
            _ = wait_abort(&options.abort) => return Err(ModelStreamError::abort()),
        };
        match frame {
            Err(_) if body_first => {
                return Err(ModelStreamError::thrown("TypeError", "terminated"));
            }
            Err(_) => {
                return Err(ModelStreamError::thrown(
                    "StreamIdleTimeoutError",
                    format!(
                        "Stream idle timeout: no data received for {}ms",
                        crate::js_value::number_string(idle_ms)
                    ),
                ));
            }
            Ok(None) => return Ok(None),
            Ok(Some(Err(_))) => return Err(ModelStreamError::thrown("TypeError", "terminated")),
            Ok(Some(Ok(frame))) => {
                if let Ok(data) = frame.into_data() {
                    return Ok(Some(data));
                }
            }
        }
    }
}

/// V8's string length limit (`MAX_STRING_LENGTH` UTF-16 units), shared by
/// every cross-event string accumulator of the stream path.
#[cfg(not(test))]
pub(crate) fn string_limit() -> usize {
    super::count_http::MAX_STRING_LENGTH
}
#[cfg(test)]
thread_local! {
    /// Tests shrink V8's string limit to exercise the cap without 512 MiB.
    pub(crate) static STRING_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(super::count_http::MAX_STRING_LENGTH) };
}
/// Restores a test's thread-local limit even if its assertion panics.
#[cfg(test)]
pub(crate) struct LimitGuard(usize);
#[cfg(test)]
impl LimitGuard {
    pub(crate) fn set(limit: usize) -> Self {
        Self(STRING_LIMIT.with(|cell| cell.replace(limit)))
    }
}
#[cfg(test)]
impl Drop for LimitGuard {
    fn drop(&mut self) {
        STRING_LIMIT.with(|cell| cell.set(self.0));
    }
}
#[cfg(test)]
pub(crate) fn string_limit() -> usize {
    STRING_LIMIT.with(std::cell::Cell::get)
}
#[cfg(not(test))]
fn sse_limit() -> usize {
    super::count_http::MAX_STRING_LENGTH
}
#[cfg(test)]
thread_local! {
    /// Separate knob so accumulator tests can keep the SSE buffer unbounded.
    pub(crate) static SSE_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(super::count_http::MAX_STRING_LENGTH) };
}
#[cfg(test)]
fn sse_limit() -> usize {
    SSE_LIMIT.with(std::cell::Cell::get)
}

/// `buffer += more` on a JS string: past V8's limit it throws
/// `RangeError: Invalid string length` (node probe r2), leaving `buffer`
/// unchanged. Exactly the limit is still allowed.
pub(crate) fn append_js_string(
    buffer: &mut Vec<u16>,
    more: &[u16],
) -> Result<(), ModelStreamError> {
    if buffer.len().saturating_add(more.len()) > string_limit() {
        return Err(invalid_string_length());
    }
    buffer.extend_from_slice(more);
    Ok(())
}
pub(crate) fn invalid_string_length() -> ModelStreamError {
    ModelStreamError::thrown("RangeError", "Invalid string length")
}

fn sse_error(error: sse::SseError) -> ModelStreamError {
    match error {
        sse::SseError::Syntax(message) => ModelStreamError::thrown("SyntaxError", message),
        // V8 `RangeError: Invalid string length` (node probe), not retryable.
        sse::SseError::InvalidStringLength => invalid_string_length(),
    }
}

// ---------------------------------------------------------------------------
// Google protocol (`streamGoogleProviderRequest` in streamModel.ts)

/// `sendGoogleCompleteRequest`/`generateContentStream` transport for the SDK:
/// one URL, SDK headers, SDK `extraBody` deep merge. Transport failures are
/// plain `TypeError: fetch failed` (the SDK uses bare fetch, not networkFetch).
async fn send_google_request(
    transport: &dyn ModelHttpTransport,
    provider: &JsValue,
    model: &JsValue,
    body: &JsValue,
    options: &StreamOptions,
) -> Result<Response<ModelBody>, ModelStreamError> {
    let timeout_ms = idle_timeout_ms(provider, options);
    let final_body = providers::google::final_body(body, provider);
    let headers = providers::google::headers(provider)?;
    let encoded = crate::js_value::json_stringify(&final_body)
        .map_err(|_| {
            ModelStreamError::thrown("RangeError", crate::js_value::StackOverflow::MESSAGE)
        })?
        .unwrap_or_default();
    let endpoint = providers::google::endpoint(provider, model)?;
    let (url, header_map) = build_http_request(&endpoint, &headers)
        .map_err(|error| ModelStreamError::thrown("TypeError", error.message))?;
    let exchange = crate::fetch_redirect::follow(
        url,
        hyper::Method::POST,
        header_map,
        Bytes::from(encoded),
        || ModelStreamError::thrown("TypeError", "fetch failed"),
        |request| async move {
            transport
                .send(request)
                .await
                .map_err(|failure| ModelStreamError::thrown("TypeError", failure.message))
        },
    );
    tokio::select! {
        result = tokio::time::timeout(duration(timeout_ms), exchange) => match result {
            Ok(result) => result,
            Err(_) => Err(ModelStreamError::thrown(
                "StreamIdleTimeoutError",
                format!(
                    "Stream idle timeout: no data received for {}ms",
                    crate::js_value::number_string(timeout_ms)
                ),
            )),
        },
        _ = wait_abort(&options.abort) => Err(ModelStreamError::abort()),
    }
}

/// `throwErrorIfNotOK` + `toProviderError` for the SDK. A JSON content type is
/// parsed like `Response.json()`; any other content type is synthesized into
/// `{error:{message,code,status}}` before `JSON.stringify`.
async fn google_response_error(
    response: Response<ModelBody>,
    options: &StreamOptions,
    provider: &JsValue,
) -> Result<ModelStreamError, ModelStreamError> {
    let status = response.status().as_u16();
    // Hyper retains non-canonical and explicitly empty statusText in this
    // extension. An absent extension means the canonical reason phrase.
    let status_text = response
        .extensions()
        .get::<hyper::ext::ReasonPhrase>()
        .map(|reason| String::from_utf8_lossy(reason.as_bytes()).into_owned())
        .unwrap_or_else(|| {
            response
                .status()
                .canonical_reason()
                .unwrap_or_default()
                .to_string()
        });
    // The SDK uses case-sensitive `includes("application/json")` on the value.
    let content_type = response
        .headers()
        .get("content-type")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .unwrap_or_default();
    let mut body = response.into_body();
    let text = response_text(&mut body, options).await?;
    let error_body = if content_type.contains("application/json") {
        providers::google::parse_error_body(&text)?
    } else {
        providers::google::synthesized_error_body(&text, status, &status_text)
    };
    let message = providers::google::api_error_message(&error_body)?;
    let js_error = JsError {
        message,
        code: None,
        network_code: None,
        raw: JsValue::Object(providers::google::api_error_raw(status)),
    };
    let canonical = normalize_model_error(
        &provider_id(provider),
        &"google".into(),
        ErrorInput::Error(&js_error),
        Some(f64::from(status)),
    );
    Ok(ModelStreamError::Provider(canonical))
}

/// One `streamGoogleProviderRequest` attempt: send, then consume the SDK SSE
/// stream. Returns as soon as a `message_end` event is emitted, like the TS
/// loop's `stream.return(...)`.
async fn google_attempt(
    transport: &dyn ModelHttpTransport,
    provider: &JsValue,
    model: &JsValue,
    body: &JsValue,
    options: &StreamOptions,
    sink: &mut (dyn FnMut(StreamOutput) + Send),
    checkpoint: &mut Checkpoint,
) -> Result<(), ModelStreamError> {
    let response = send_google_request(transport, provider, model, body, options).await?;
    let status = response.status().as_u16();
    if matches!(status, 204 | 205) {
        // fetch exposes a null body for null-body statuses; the SDK's stream
        // reader throws before yielding any chunk.
        return Err(ModelStreamError::thrown("Error", "Response body is empty"));
    }
    if !(200..300).contains(&status) {
        return Err(google_response_error(response, options, provider).await?);
    }
    let mut body = response.into_body();
    let mut state = providers::google::StreamState::default();
    let idle_ms = idle_timeout_ms(provider, options);
    let mut guard = StreamGuard::new(provider, (options.clock)());
    let mut buffer = sse::SseBuffer::with_limit(sse_limit());
    loop {
        throw_if_aborted(options)?;
        let chunk = read_frame(&mut body, idle_ms, options).await?;
        throw_if_aborted(options)?;
        let parsed = match chunk {
            Some(ref bytes) => buffer.push(bytes),
            None => buffer.finish(),
        };
        for item in parsed {
            let item = item.map_err(sse_error)?;
            guard.check_duration((options.clock)())?;
            let raw = match item {
                sse::SseEvent::Done => continue,
                sse::SseEvent::Data(raw) => raw,
            };
            for event in state.normalize(&raw)? {
                let tag = field(&event, "type");
                if tag.as_string().is_some_and(|tag| tag.equals_str("error")) {
                    return Err(ModelStreamError::Provider(object_entries(field(
                        &event, "error",
                    ))));
                }
                let message_end = tag
                    .as_string()
                    .is_some_and(|tag| tag.equals_str("message_end"));
                guard.observe(&event, (options.clock)())?;
                checkpoint.on_event(&event)?;
                sink(StreamOutput::Event(event));
                if message_end {
                    return Ok(());
                }
            }
        }
        if chunk.is_none() {
            break;
        }
    }
    guard.check_duration((options.clock)())?;
    if !state.ended() {
        return Err(ModelStreamError::thrown(
            "IncompleteStreamError",
            "Network stream ended before provider completion sentinel.",
        ));
    }
    Ok(())
}

fn request_started_event(
    provider: &JsValue,
    current: &JsValue,
) -> Result<JsValue, ModelStreamError> {
    let fingerprint = crate::js_value::json_stringify(current)
        .map_err(|_| {
            ModelStreamError::thrown("RangeError", crate::js_value::StackOverflow::MESSAGE)
        })?
        .unwrap_or_default();
    let mut started = JsObject::new();
    started.insert("type".into(), "request_started".into());
    started.insert("provider".into(), field(provider, "id").clone());
    started.insert("model".into(), field(current, "model").clone());
    started.insert(
        "providerBaseUrl".into(),
        providers::normalize_provider_base_url(field(provider, "url")),
    );
    started.insert(
        "requestFingerprint".into(),
        format!("{:x}", Sha256::digest(fingerprint.as_bytes()))
            .as_str()
            .into(),
    );
    started.insert("metadata".into(), field(current, "metadata").clone());
    Ok(JsValue::Object(started))
}

/// `streamGoogleProviderRequest`: request_started per attempt, SDK error
/// canonicalization, continuation/network retries and error events.
async fn stream_google_model(
    transport: &dyn ModelHttpTransport,
    request: &JsValue,
    config: &JsValue,
    options: &StreamOptions,
    sink: &mut (dyn FnMut(StreamOutput) + Send),
    provider: &JsValue,
) -> Result<(), ModelStreamError> {
    let mut current = request.clone();
    let (max_retries, _) = retry_decide(provider, 0.0, None, 0.0);
    let mut checkpoint = Checkpoint::default();
    let mut attempt = 0.0;
    while attempt <= max_retries {
        throw_if_aborted(options)?;
        sink(StreamOutput::Event(request_started_event(
            provider, &current,
        )?));

        let (_, model) = providers::validate_model_request(&current, config)?;
        let model_id = field(&current, "model").clone();
        let outcome = match providers::google::build_request(&current, &model) {
            Ok(body) => {
                google_attempt(
                    transport,
                    provider,
                    &model_id,
                    &body,
                    options,
                    sink,
                    &mut checkpoint,
                )
                .await
            }
            Err(error) => Err(error),
        };
        let Err(error) = outcome else {
            return Ok(());
        };
        if aborted(options) {
            return Err(ModelStreamError::abort());
        }
        let canonical = match &error {
            ModelStreamError::Provider(canonical) => canonical.clone(),
            other => normalize_model_error(
                &provider_id(provider),
                &"google".into(),
                ErrorInput::Error(&other.as_js_error()),
                None,
            ),
        };
        let canonical_message = canonical
            .get("message")
            .and_then(JsValue::as_string)
            .map(|message| String::from_utf16_lossy(message.units()).to_lowercase())
            .unwrap_or_default();
        let retryable = canonical.get("retryable") == Some(&JsValue::Bool(true))
            || is_retryable_stream_error(&error)
            || retryable_message(&canonical_message);
        let current_model = field(&current, "model").clone();
        let progress =
            |sink: &mut (dyn FnMut(StreamOutput) + Send), reason: &str, delay_ms: f64| {
                let mut value = JsObject::new();
                value.insert("reason".into(), reason.into());
                value.insert("attempt".into(), JsValue::Number(attempt + 1.0));
                value.insert("maxAttempts".into(), JsValue::Number(max_retries));
                value.insert("delayMs".into(), JsValue::Number(js_round(delay_ms)));
                value.insert("provider".into(), field(provider, "id").clone());
                value.insert("model".into(), current_model.clone());
                sink(StreamOutput::RetryProgress(JsValue::Object(value)));
            };
        if attempt < max_retries && retryable && checkpoint.can_continue_text() {
            current = continuation_request(&current, &checkpoint.partial_text);
            let (_, delay_ms) = retry_decide(provider, attempt, None, (options.random)());
            progress(sink, "continuation", delay_ms);
            delay(delay_ms, options).await?;
            attempt += 1.0;
            continue;
        }
        let empty = checkpoint
            .interruption()
            .get("phase")
            .is_some_and(|phase| phase.as_string().is_some_and(|p| p.equals_str("empty")));
        if attempt < max_retries && retryable && empty {
            let (_, delay_ms) = retry_decide(provider, attempt, None, (options.random)());
            progress(sink, "network_error", delay_ms);
            delay(delay_ms, options).await?;
            attempt += 1.0;
            continue;
        }
        let mut canonical = canonical;
        if retryable {
            canonical.insert("streamInterruption".into(), checkpoint.interruption());
        }
        sink(StreamOutput::Event(error_event(JsValue::Object(canonical))));
        return Ok(());
    }
    Ok(())
}

/// `streamModel(request, config, options)` for the HTTP protocols.
pub async fn stream_model(
    transport: &dyn ModelHttpTransport,
    request: &JsValue,
    config: &JsValue,
    options: &StreamOptions,
    sink: &mut (dyn FnMut(StreamOutput) + Send),
) -> Result<(), ModelStreamError> {
    let mut current = request.clone();
    put(&mut current, "stream", JsValue::Bool(true));
    let (provider, _) = providers::validate_model_request(&current, config)?;
    if is(field(&provider, "protocol"), "google") {
        return stream_google_model(transport, &current, config, options, sink, &provider).await;
    }
    let (max_retries, _) = retry_decide(&provider, 0.0, None, 0.0);
    let mut checkpoint = Checkpoint::default();
    let mut attempt = 0.0;
    while attempt <= max_retries {
        throw_if_aborted(options)?;
        sink(StreamOutput::Event(request_started_event(
            &provider, &current,
        )?));
        let (provider_now, model_now) = providers::validate_model_request(&current, config)?;
        let body = providers::build_model_request(&current, &provider_now, &model_now)?;

        let progress = |sink: &mut (dyn FnMut(StreamOutput) + Send),
                        reason: &str,
                        delay_ms: f64,
                        model: &JsValue| {
            let mut value = JsObject::new();
            value.insert("reason".into(), reason.into());
            value.insert("attempt".into(), JsValue::Number(attempt + 1.0));
            value.insert("maxAttempts".into(), JsValue::Number(max_retries));
            value.insert("delayMs".into(), JsValue::Number(js_round(delay_ms)));
            value.insert("provider".into(), field(&provider, "id").clone());
            value.insert("model".into(), model.clone());
            sink(StreamOutput::RetryProgress(JsValue::Object(value)));
        };

        let response = match send_provider_request(transport, &provider, &body, options).await {
            Ok(response) => response,
            Err(error) => {
                if attempt < max_retries && is_retryable_stream_error(&error) {
                    let (_, delay_ms) = retry_decide(&provider, attempt, None, (options.random)());
                    progress(sink, "network_error", delay_ms, field(&current, "model"));
                    delay(delay_ms, options).await?;
                    attempt += 1.0;
                    continue;
                }
                if is_retryable_stream_error(&error) && !checkpoint.is_empty() {
                    sink(StreamOutput::Event(error_event(interruption_error(
                        &provider,
                        &error,
                        &checkpoint,
                    ))));
                    return Ok(());
                }
                return Err(error);
            }
        };
        let status = response.status().as_u16();
        let retry_after_header = {
            let values: Vec<String> = response
                .headers()
                .get_all("retry-after")
                .iter()
                .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
                .collect();
            (!values.is_empty()).then(|| values.join(", "))
        };
        let mut body = response.into_body();
        if matches!(status, 204 | 205) {
            // Null-body statuses: `response.body` is null in fetch.
            let missing = JsError {
                message: "Missing response body.".into(),
                code: None,
                network_code: None,
                raw: JsValue::Object(JsObject::new()),
            };
            sink(StreamOutput::Event(error_event(JsValue::Object(
                normalize_model_error(
                    &provider_id(&provider),
                    &protocol(&provider),
                    ErrorInput::Error(&missing),
                    None,
                ),
            ))));
            return Ok(());
        }
        if !(200..300).contains(&status) {
            let text = response_text(&mut body, options).await?;
            let raw = parse_json(&text).unwrap_or_else(|_| JsValue::String(text.as_str().into()));
            let mut error = normalize_model_error(
                &provider_id(&provider),
                &protocol(&provider),
                ErrorInput::Value(&raw),
                Some(f64::from(status)),
            );
            if error.get("retryAfterMs").is_none_or(JsValue::is_undefined)
                && let Some(ms) =
                    errors::retry_after_header(retry_after_header.as_deref(), (options.clock)())
            {
                errors::set_retry_after(&mut error, ms);
            }
            let retryable = error.get("retryable") == Some(&JsValue::Bool(true));
            if retryable && attempt < max_retries {
                let after = error.get("retryAfterMs").and_then(JsValue::as_number);
                let (_, delay_ms) = retry_decide(&provider, attempt, after, (options.random)());
                let reason = retry_reason(&ModelStreamError::Provider(error.clone()));
                progress(sink, reason, delay_ms, field(&current, "model"));
                drop(body);
                delay(delay_ms, options).await?;
                attempt += 1.0;
                continue;
            }
            if retryable && !checkpoint.is_empty() {
                let error = ModelStreamError::Provider(error);
                sink(StreamOutput::Event(error_event(interruption_error(
                    &provider,
                    &error,
                    &checkpoint,
                ))));
                return Ok(());
            }
            sink(StreamOutput::Event(error_event(JsValue::Object(error))));
            return Ok(());
        }

        let mut state = StreamState::new(&provider);
        let mut sentinel = false;
        let idle_ms = idle_timeout_ms(&provider, options);
        let mut guard = StreamGuard::new(&provider, (options.clock)());
        let mut buffer = sse::SseBuffer::with_limit(sse_limit());
        let outcome: Result<(), ModelStreamError> = async {
            throw_if_aborted(options)?;
            loop {
                throw_if_aborted(options)?;
                let chunk = read_frame(&mut body, idle_ms, options).await?;
                throw_if_aborted(options)?;
                let parsed = match chunk {
                    Some(ref bytes) => buffer.push(bytes),
                    None => buffer.finish(),
                };
                for item in parsed {
                    let item = item.map_err(sse_error)?;
                    guard.check_duration((options.clock)())?;
                    let raw = match item {
                        sse::SseEvent::Done => {
                            sentinel = true;
                            continue;
                        }
                        sse::SseEvent::Data(raw) => raw,
                    };
                    for event in state.normalize(&raw)? {
                        let tag = field(&event, "type");
                        if tag.as_string().is_some_and(|t| t.equals_str("message_end")) {
                            sentinel = true;
                        }
                        if tag.as_string().is_some_and(|t| t.equals_str("error")) {
                            return Err(ModelStreamError::Provider(object_entries(field(
                                &event, "error",
                            ))));
                        }
                        guard.observe(&event, (options.clock)())?;
                        checkpoint.on_event(&event)?;
                        sink(StreamOutput::Event(event));
                    }
                }
                if chunk.is_none() {
                    break;
                }
            }
            guard.check_duration((options.clock)())?;
            if !sentinel {
                return Err(ModelStreamError::thrown(
                    "IncompleteStreamError",
                    "Network stream ended before provider completion sentinel.",
                ));
            }
            Ok(())
        }
        .await;
        // Closing the body (here or on any return) cancels the connection.
        drop(body);
        let Err(error) = outcome else {
            return Ok(());
        };
        let retryable = is_retryable_stream_error(&error);
        if attempt < max_retries && retryable && checkpoint.can_continue_text() {
            current = continuation_request(&current, &checkpoint.partial_text);
            let (_, delay_ms) =
                retry_decide(&provider, attempt, retry_after(&error), (options.random)());
            progress(sink, "continuation", delay_ms, field(&current, "model"));
            delay(delay_ms, options).await?;
            attempt += 1.0;
            continue;
        }
        if retryable && attempt < max_retries && checkpoint.is_empty() {
            let (_, delay_ms) =
                retry_decide(&provider, attempt, retry_after(&error), (options.random)());
            progress(
                sink,
                retry_reason(&error),
                delay_ms,
                field(&current, "model"),
            );
            delay(delay_ms, options).await?;
            attempt += 1.0;
            continue;
        }
        if retryable {
            sink(StreamOutput::Event(error_event(interruption_error(
                &provider,
                &error,
                &checkpoint,
            ))));
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
