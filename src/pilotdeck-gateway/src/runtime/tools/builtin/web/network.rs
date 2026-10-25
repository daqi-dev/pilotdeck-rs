//! `network/fetch.ts` `networkFetch` for the web tools: one request over the
//! shared [`ModelHttpTransport`] (hyper + rustls), a per-attempt timeout, the
//! parent abort signal, and the retry loop (statuses 408/409/425/429/500/502/
//! 503/504 and retryable network errors; GET always, POST only with
//! `retryOnPost`) with `Retry-After` or jittered exponential backoff.
//!
//! Request construction follows undici's `fetch` as far as the web tools can
//! reach it: the fragment is dropped, URLs with credentials, non-HTTP(S)
//! schemes and Fetch's blocked ports fail before any exchange, and repeated
//! header names are joined with `, `. undici's implicit `user-agent: node`,
//! `accept-language` and `accept-encoding` headers are not sent (see the
//! fixture README).
use crate::runtime::{
    model::http::{ModelBody, ModelHttpTransport, NetworkFetchError, TransportFailure},
    tools::ToolRunContext,
};
use bytes::Bytes;
use http_body_util::Full;
use pilotdeck_protocol::{JsObject, JsValue};
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;

/// The global dispatcher's undici `bodyTimeout` (`cli/proxy.ts` installs
/// 600000 ms, `probes/body-abort.out`): a body read that sees no bytes for
/// this long fails like `TypeError: terminated`. It is an idle timer, not a
/// total deadline — undici resets it on every chunk, so a steadily dripping
/// body can outlive it (same probe).
const BODY_TIMEOUT: Duration = crate::runtime::network::UNDICI_BODY_TIMEOUT;

use crate::runtime::network::{Exchange, delay, exchange};
pub use crate::runtime::network::{node_timer_delay_ms, node_timer_duration};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    /// In `Headers` insertion order; names are case-insensitive.
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// A response whose body has not been read yet.
pub struct HttpResponse {
    pub status: u16,
    /// undici's `statusText`: the reason phrase as sent.
    pub status_text: String,
    /// `response.headers` as a lowercase-name map (`Headers` iteration joins
    /// repeated names with `, `).
    pub headers: Vec<(String, String)>,
    pub body: ModelBody,
    /// The selected response's microtask continuation precedes a delivered
    /// later cancellation. This privilege ends at the first pending body read.
    body_before_abort: bool,
}

impl HttpResponse {
    pub async fn read_body(
        &mut self,
        cap: usize,
        parent: Option<&ToolRunContext>,
    ) -> Result<Vec<u8>, BodyError> {
        let body_before_abort = std::mem::take(&mut self.body_before_abort);
        read_body_inner(&mut self.body, cap, parent, body_before_abort).await
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A body read failure, as the baseline sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyError {
    /// undici `TypeError: terminated`.
    Terminated,
    /// The body grew past the caller's cap (bytes read so far).
    TooLarge,
    /// The tool call's parent signal fired while the body was being read.
    /// The baseline detaches its abort listener once the headers arrive, so
    /// this is a scheduler-contract guarantee rather than a TS behaviour
    /// (`probes/body-abort.out`).
    Aborted,
}

/// `response.arrayBuffer()`, stopping once more than `cap` bytes arrived:
/// the baseline reads everything and then compares the length, which is the
/// same outcome without holding an unbounded body in memory.
///
/// The read races every frame against the parent signal. undici's own
/// `bodyTimeout` is an idle timer (`BODY_TIMEOUT`); there is no total
/// deadline because the baseline clears its 60 s timer when the headers
/// arrive (`probes/body-abort.out`, `probes/body-fetch-path.out`).
pub async fn read_body(
    body: &mut ModelBody,
    cap: usize,
    parent: Option<&ToolRunContext>,
) -> Result<Vec<u8>, BodyError> {
    read_body_inner(body, cap, parent, false).await
}

async fn read_body_inner(
    body: &mut ModelBody,
    cap: usize,
    parent: Option<&ToolRunContext>,
    mut body_before_abort: bool,
) -> Result<Vec<u8>, BodyError> {
    let mut out = Vec::new();
    loop {
        // Ledger: specs/gateway-parity/diffs/A6.4-NETWORK-TIMING-POST-HEADERS-PARENT-001.md
        match crate::runtime::network::body_frame_with_abort(
            body,
            Some(BODY_TIMEOUT),
            parent_aborted(parent),
            || parent.and_then(ToolRunContext::abort_event_at),
            &mut body_before_abort,
        )
        .await
        {
            Exchange::Aborted => return Err(BodyError::Aborted),
            Exchange::Timeout { .. } | Exchange::Response(Some(Err(_)), _) => {
                return Err(BodyError::Terminated);
            }
            Exchange::Response(None, _) => return Ok(out),
            Exchange::Response(Some(Ok(frame)), _) => {
                if let Ok(data) = frame.into_data() {
                    if out.len().saturating_add(data.len()) > cap {
                        return Err(BodyError::TooLarge);
                    }
                    out.extend_from_slice(&data);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RetryOptions {
    pub max_retries: u32,
    pub base_delay_ms: f64,
    pub max_delay_ms: f64,
    pub retry_on_post: bool,
}

/// The retry settings every web tool call site passes.
pub const WEB_RETRY: RetryOptions = RetryOptions {
    max_retries: 2,
    base_delay_ms: 500.0,
    max_delay_ms: 5_000.0,
    retry_on_post: false,
};

/// `Math.random()` for the backoff jitter.
pub type Random = Arc<dyn Fn() -> f64 + Send + Sync>;

pub fn default_random() -> Random {
    Arc::new(|| {
        let bytes = uuid::Uuid::new_v4().into_bytes();
        let mut word = [0u8; 8];
        word.copy_from_slice(&bytes[..8]);
        (u64::from_le_bytes(word) >> 11) as f64 / (1u64 << 53) as f64
    })
}

pub struct FetchOptions<'a> {
    pub timeout_ms: f64,
    /// The parent `AbortSignal` (the tool call's).
    pub parent: Option<&'a ToolRunContext>,
    pub retry: RetryOptions,
    pub random: Random,
}

const RETRY_STATUSES: [u16; 8] = [408, 409, 425, 429, 500, 502, 503, 504];

fn object(entries: &[(&str, JsValue)]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert((*key).into(), value.clone());
    }
    object
}

fn fetch_failed(message: &str) -> NetworkFetchError {
    NetworkFetchError::classify(message, None)
}

fn build(request: &HttpRequest) -> Result<hyper::Request<Full<Bytes>>, NetworkFetchError> {
    let parsed = url::Url::parse(&request.url)
        .map_err(|_| fetch_failed(&format!("Failed to parse URL from {}", request.url)))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(fetch_failed(&format!(
            "Request cannot be constructed from a URL that includes credentials: {}",
            request.url
        )));
    }
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed
            .port()
            .is_some_and(crate::fetch_redirect::blocked_port)
    {
        return Err(fetch_failed("fetch failed"));
    }
    let mut uri = parsed;
    uri.set_fragment(None);
    let mut built = hyper::Request::builder()
        .method(request.method)
        .uri(uri.as_str())
        .body(Full::new(Bytes::from(
            request.body.clone().unwrap_or_default(),
        )))
        .map_err(|_| fetch_failed("fetch failed"))?;
    let map = built.headers_mut();
    for (name, value) in &request.headers {
        let name = hyper::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| fetch_failed("Invalid header name"))?;
        let value = value.trim_matches([' ', '\t', '\r', '\n']);
        let joined = match map.get(&name) {
            Some(existing) => format!("{}, {value}", existing.to_str().unwrap_or_default()),
            None => value.to_string(),
        };
        let value = hyper::header::HeaderValue::from_str(&joined)
            .map_err(|_| fetch_failed("Invalid header value"))?;
        map.insert(name, value);
    }
    Ok(built)
}

fn response_headers(map: &hyper::HeaderMap) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, value) in map {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        match out.iter_mut().find(|(key, _)| key == name.as_str()) {
            Some((_, existing)) if name != hyper::header::SET_COOKIE => {
                existing.push_str(", ");
                existing.push_str(&value);
            }
            Some((_, existing)) => *existing = value,
            None => out.push((name.as_str().to_string(), value)),
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn reason(response: &hyper::Response<ModelBody>) -> String {
    match response.extensions().get::<hyper::ext::ReasonPhrase>() {
        Some(phrase) => String::from_utf8_lossy(phrase.as_bytes()).into_owned(),
        None => response
            .status()
            .canonical_reason()
            .unwrap_or("")
            .to_string(),
    }
}

/// `resolveRetryDelay(attempt, retry, retryAfterHeader)`.
fn retry_delay(
    attempt: u32,
    retry: &RetryOptions,
    retry_after: Option<&str>,
    random: &Random,
) -> f64 {
    let cap = retry.max_delay_ms;
    if let Some(ms) = crate::runtime::model::http::retry_after_header_ms(retry_after) {
        return cap.min(ms);
    }
    let exponential = cap.min(retry.base_delay_ms * 2f64.powi(attempt as i32));
    let jitter = (random() * (exponential * 0.25).floor().max(1.0)).floor();
    cap.min(exponential + jitter)
}

async fn parent_aborted(parent: Option<&ToolRunContext>) {
    match parent {
        Some(context) => context.cancelled().await,
        None => std::future::pending().await,
    }
}

fn abort_error(message: &str) -> NetworkFetchError {
    NetworkFetchError::new(
        "network_abort",
        message,
        JsValue::Object(object(&[
            ("name", JsValue::from("AbortError")),
            ("message", JsValue::from("This operation was aborted")),
        ])),
    )
}

fn timeout_error(timeout_ms: f64) -> NetworkFetchError {
    NetworkFetchError::new(
        "network_timeout",
        format!(
            "Network request timed out after {}ms.",
            crate::js_value::number_string(timeout_ms)
        ),
        JsValue::Undefined,
    )
}

// Ledger: specs/gateway-parity/diffs/A6.4-NETWORK-TIMING-RETRY-TIE-001.md
/// Map the shared retry wait's event ordering to the web/provider errors.
async fn retry_wait(
    ms: f64,
    options: &FetchOptions<'_>,
    outer_deadline: Option<OuterDeadline>,
    continuation_before_abort: bool,
) -> Result<bool, NetworkFetchError> {
    match delay(
        node_timer_duration(ms),
        parent_aborted(options.parent),
        || options.parent.and_then(ToolRunContext::abort_event_at),
        outer_deadline.map(|outer| outer.at),
        continuation_before_abort,
    )
    .await
    {
        Exchange::Response((), before_abort) => Ok(before_abort),
        Exchange::Aborted => Err(abort_error("Network retry aborted.")),
        Exchange::Timeout { .. } => {
            let outer =
                outer_deadline.expect("only a surrounding deadline can time out a retry wait");
            Err(if outer.parent {
                abort_error("Network retry aborted.")
            } else {
                outer.error(options.timeout_ms)
            })
        }
    }
}

#[derive(Clone, Copy)]
struct OuterDeadline {
    at: Instant,
    /// urlFetcher's hop timer aborts networkFetch's parent controller.
    parent: bool,
}

impl OuterDeadline {
    fn error(self, timeout_ms: f64) -> NetworkFetchError {
        if self.parent {
            abort_error("Network request aborted by parent signal.")
        } else {
            timeout_error(timeout_ms)
        }
    }
}

/// `networkFetch(url, init, { timeoutMs, signal, retry })`.
pub async fn network_fetch(
    transport: &dyn ModelHttpTransport,
    request: &HttpRequest,
    options: &FetchOptions<'_>,
) -> Result<HttpResponse, NetworkFetchError> {
    network_fetch_inner(transport, request, options, None).await
}

/// The provider's timer surrounds all attempts and retry delays. Keeping it
/// beside the transport's completion source avoids an outer `select!` throwing
/// away the ordering established by the inner exchange.
pub(super) async fn network_fetch_until(
    transport: &dyn ModelHttpTransport,
    request: &HttpRequest,
    options: &FetchOptions<'_>,
    deadline: Instant,
) -> Result<HttpResponse, NetworkFetchError> {
    network_fetch_inner(
        transport,
        request,
        options,
        Some(OuterDeadline {
            at: deadline,
            parent: false,
        }),
    )
    .await
}

/// urlFetcher's surrounding hop timer is a parent abort, not a local timeout.
pub(super) async fn network_fetch_with_parent_deadline(
    transport: &dyn ModelHttpTransport,
    request: &HttpRequest,
    options: &FetchOptions<'_>,
    deadline: Instant,
) -> Result<HttpResponse, NetworkFetchError> {
    network_fetch_inner(
        transport,
        request,
        options,
        Some(OuterDeadline {
            at: deadline,
            parent: true,
        }),
    )
    .await
}

async fn network_fetch_inner(
    transport: &dyn ModelHttpTransport,
    request: &HttpRequest,
    options: &FetchOptions<'_>,
    outer_deadline: Option<OuterDeadline>,
) -> Result<HttpResponse, NetworkFetchError> {
    let retry = options.retry;
    let method = request.method.to_ascii_uppercase();
    let can_retry_method =
        matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS") || retry.retry_on_post;
    let mut attempt = 0;
    let mut continuation_before_abort = false;
    loop {
        let outcome = if !continuation_before_abort
            && options.parent.is_some_and(ToolRunContext::is_aborted)
        {
            Err(abort_error("Network request aborted by parent signal."))
        } else {
            let built = build(request);
            match built {
                Err(error) => Err(error),
                Ok(built) => {
                    // Only the per-attempt timer is gated on `timeoutMs > 0`.
                    let attempt_deadline = (options.timeout_ms > 0.0)
                        .then(|| Instant::now() + node_timer_duration(options.timeout_ms));
                    let deadline = match (outer_deadline.map(|outer| outer.at), attempt_deadline) {
                        (Some(outer), Some(attempt)) => Some(outer.min(attempt)),
                        (outer, attempt) => outer.or(attempt),
                    };
                    match exchange(
                        transport.send(built),
                        parent_aborted(options.parent),
                        || options.parent.and_then(ToolRunContext::abort_event_at),
                        deadline,
                    )
                    .await
                    {
                        Exchange::Response(Ok(response), body_before_abort) => {
                            continuation_before_abort = body_before_abort;
                            Ok((response, body_before_abort))
                        }
                        Exchange::Response(
                            Err(TransportFailure {
                                cause_code,
                                message,
                            }),
                            before_abort,
                        ) => {
                            continuation_before_abort = before_abort;
                            // TS normalizes raw fetch rejection before testing retry
                            // eligibility. Preserve an earlier error timer's continuation.
                            Err(
                                if !before_abort
                                    && options.parent.is_some_and(ToolRunContext::is_aborted)
                                {
                                    abort_error("Network request aborted by parent signal.")
                                } else {
                                    NetworkFetchError::classify(&message, cause_code)
                                },
                            )
                        }
                        Exchange::Aborted => {
                            Err(abort_error("Network request aborted by parent signal."))
                        }
                        Exchange::Timeout { before_abort } => {
                            continuation_before_abort = before_abort;
                            if let Some(outer) = outer_deadline
                                && deadline == Some(outer.at)
                            {
                                return Err(outer.error(options.timeout_ms));
                            }
                            Err(timeout_error(options.timeout_ms))
                        }
                    }
                }
            }
        };
        match outcome {
            Ok((response, body_before_abort)) => {
                let status = response.status().as_u16();
                if can_retry_method
                    && attempt < retry.max_retries
                    && RETRY_STATUSES.contains(&status)
                {
                    // `Headers.get()` joins repeated names with `, ` (the
                    // same read as `response_headers`).
                    let retry_after = {
                        let values: Vec<&str> = response
                            .headers()
                            .get_all(hyper::header::RETRY_AFTER)
                            .iter()
                            .filter_map(|value| value.to_str().ok())
                            .collect();
                        (!values.is_empty()).then(|| values.join(", "))
                    };
                    drop(response);
                    continuation_before_abort = retry_wait(
                        retry_delay(attempt, &retry, retry_after.as_deref(), &options.random),
                        options,
                        outer_deadline,
                        continuation_before_abort,
                    )
                    .await?;
                    attempt += 1;
                    continue;
                }
                let status_text = reason(&response);
                let headers = response_headers(response.headers());
                return Ok(HttpResponse {
                    status,
                    status_text,
                    headers,
                    body: response.into_body(),
                    body_before_abort,
                });
            }
            Err(error) => {
                let retryable = error.code != "network_abort" && error.code != "network_tls_error";
                if !can_retry_method || attempt >= retry.max_retries || !retryable {
                    return Err(error);
                }
                continuation_before_abort = retry_wait(
                    retry_delay(attempt, &retry, None, &options.random),
                    options,
                    outer_deadline,
                    continuation_before_abort,
                )
                .await?;
                attempt += 1;
            }
        }
    }
}
