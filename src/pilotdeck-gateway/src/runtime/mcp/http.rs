//! HTTP and HTTPS MCP transports, ported from
//! `@modelcontextprotocol/sdk@1.29.0` as the fixed baseline drives them from
//! `McpClient.buildTransport()`:
//!
//! - `streamable_http` — `StreamableHTTPClientTransport` with the baseline's
//!   `networkFetch` (`src/network/fetch.ts`): POST timeout `callTimeoutMs ??
//!   DEFAULT_CALL_TIMEOUT_MS`, GET timeout `handshakeTimeoutMs ?? 10000`, one
//!   retry for GETs on 408/409/425/429/500/502/503/504 (TS `DEFAULT_RETRY_STATUSES`, network/fetch.ts:48) or retryable network errors;
//! - `sse` — the legacy `SSEClientTransport` over the `eventsource@3`
//!   `EventSource`, posting with the global `fetch` (no timeout or retry).
//!
//! Server-Sent Events are parsed as `eventsource-parser@3.0.8` does.
//! `http:` and `https:` URLs are served through the shared model transport's
//! rustls connector (`runtime/model/http.rs`), wrapped in undici's 10 s
//! `connectTimeout` so the deadline covers the TLS handshake as well as TCP;
//! redirects (including scheme changes) follow `crate::fetch_redirect` like
//! the baseline's `fetch`. Other schemes are refused up front with
//! `mcp_unsupported_transport` (the baseline would fail later in `fetch`).
//!
//! Deliberate lifecycle differences (unapproved; network side effects remain):
//! gateway accepted-diffs A6.5-MCP-HTTP-LIFETIME-001. A closed
//! transport also drops its open event streams (undici keeps a body open
//! after `networkFetch` detached the abort signal); a legacy `EventSource`
//! whose first connection fails stops instead of retrying every 3 s in the
//! background.
//!
//! Proxy: [`HttpLink::with_config`] builds the connector from
//! `ProxyConfig::from_settings`, the same `src/cli/proxy.ts` sources (see
//! `runtime/model/http.rs`). Like undici's `ProxyAgent.dispatch`
//! (`proxy-agent.js:241-243,323-341`), a request that routes through the proxy
//! is refused when it carries any `Proxy-Authorization` header, so credentials
//! only ever appear on the `CONNECT` line sent to the proxy.

use super::client::{McpClientError, McpErrorCode};
use super::protocol::{Inbound, parse_inbound_value};
use super::wire::{Wire, frame_text, lock};
use crate::runtime::{
    model::http::{
        ConnectDeadline, ProxyConfig, ProxyConnect, ProxyError, ProxySettings,
        proxy_https_connector, retry_after_header_ms,
    },
    network::{Exchange, ExchangeFuture, delay, exchange_continuing, node_timer_duration},
    tools::ToolCancellation,
};
use crate::{fetch_redirect, js_value};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, StatusCode, body::Incoming, header};
use hyper_rustls::HttpsConnector;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use pilotdeck_protocol::{JsValue, json_syntax, parse_json};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::oneshot;
use url::Url;

type HttpClient = Client<ConnectDeadline<HttpsConnector<ProxyConnect>>, Full<Bytes>>;

/// undici's `connectTimeout` default (10 000 ms) spans socket establishment
/// including the TLS handshake (and a proxy CONNECT when configured); the
/// shared `HttpConnector` only bounds its own TCP phase, so the MCP-local
/// deadline wraps the whole proxy/TLS connect.
const MCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// `DEFAULT_STREAMABLE_HTTP_RECONNECTION_OPTIONS`.
const INITIAL_RECONNECTION_DELAY_MS: f64 = 1000.0;
const MAX_RECONNECTION_DELAY_MS: f64 = 30_000.0;
const RECONNECTION_GROW_FACTOR: f64 = 1.5;
const MAX_RECONNECTION_RETRIES: u32 = 2;
/// `EventSource`'s initial reconnect interval.
const EVENT_SOURCE_RECONNECT_MS: u64 = 3000;
/// `networkFetch` retry statuses.
const RETRY_STATUSES: [u16; 8] = [408, 409, 425, 429, 500, 502, 503, 504];

/// Which transport an [`HttpLink`] speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HttpKind {
    Streamable,
    LegacySse,
}

/// Mutable transport state.
#[derive(Default)]
struct LinkState {
    session_id: Option<String>,
    protocol_version: Option<String>,
    /// SSE `retry:` value seen by the streamable transport.
    server_retry_ms: Option<f64>,
    /// Legacy SSE POST endpoint.
    endpoint: Option<Url>,
}

pub(super) struct HttpLink {
    kind: HttpKind,
    url: Url,
    headers: Vec<(String, String)>,
    post_timeout_ms: f64,
    get_timeout_ms: f64,
    client: HttpClient,
    /// The active proxy, if any; the connector is already built from it. Used
    /// to reproduce undici's per-dispatch `Proxy-Authorization` refusal.
    proxy: Option<ProxyConfig>,
    state: Mutex<LinkState>,
    aborted: ToolCancellation,
}

/// A fetch failure, already in the shape the SDK surfaces.
fn network_error(code: &'static str, message: String) -> McpClientError {
    McpClientError::with_code("NetworkFetchError", McpErrorCode::Text(code), message)
}

fn http_error(status: f64, message: String) -> McpClientError {
    McpClientError::with_code(
        "Error",
        McpErrorCode::Number(status),
        format!("Streamable HTTP error: {message}"),
    )
}

/// The URL check `buildTransport()` runs synchronously: `new URL(url)` plus
/// this port's scheme restriction. The baseline accepts any URL `new URL`
/// parses and only fails when its fetch runs; `http:`/`https:` are the
/// schemes MCP transports can reach, so anything else keeps the existing
/// fail-closed `mcp_unsupported_transport` (a deliberate, documented
/// difference for non-HTTP(S) schemes).
pub(super) fn parse_endpoint(raw: &str, server_id: &str) -> Result<Url, McpClientError> {
    let url = Url::parse(raw).map_err(|_| {
        McpClientError::with_code(
            "TypeError",
            McpErrorCode::Text("ERR_INVALID_URL"),
            "Invalid URL".into(),
        )
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(McpClientError::client(
            super::client::McpClientErrorCode::UnsupportedTransport,
            format!(
                "Unsupported URL scheme {}: only http and https MCP servers are supported",
                url.scheme()
            ),
            server_id,
        ));
    }
    Ok(url)
}

/// `response.text()`: UTF-8 with a leading BOM dropped.
fn decode_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.strip_prefix('\u{FEFF}').unwrap_or(&text).to_owned()
}

/// `Headers.get(name)` (values of repeated headers joined with ", ").
fn header_value(headers: &hyper::HeaderMap, name: &str) -> Option<String> {
    let values: Vec<String> = headers
        .get_all(name)
        .iter()
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

/// A `Headers` object built from a plain record and then edited with
/// `set`: names compare case-insensitively, values are kept in order.
#[derive(Clone, Default)]
struct HeaderList(Vec<(String, String)>);
impl HeaderList {
    fn append(&mut self, name: &str, value: &str) {
        let lower = name.to_ascii_lowercase();
        match self.0.iter_mut().find(|(key, _)| *key == lower) {
            Some((_, existing)) => {
                existing.push_str(", ");
                existing.push_str(value);
            }
            None => self.0.push((lower, value.to_owned())),
        }
    }
    fn set(&mut self, name: &str, value: &str) {
        let lower = name.to_ascii_lowercase();
        self.0.retain(|(key, _)| *key != lower);
        self.0.push((lower, value.to_owned()));
    }
}

/// `undici/lib/dispatcher/proxy-agent.js:323-341`: any header named
/// `Proxy-Authorization` (case-insensitively) is refused when the request is
/// dispatched through the proxy, so configured credentials can never leak into
/// the tunnel. The direct agent (`NO_PROXY`) performs no such check, and the
/// message is the undici `InvalidArgumentError` text.
const PROXY_AUTHORIZATION_HEADER: &str = "proxy-authorization";
const PROXY_AUTHORIZATION_REJECTED: &str =
    "Proxy-Authorization should be sent in ProxyAgent constructor";

fn reject_proxy_authorization(
    proxy: Option<&ProxyConfig>,
    request: &Request<Full<Bytes>>,
) -> Result<(), McpClientError> {
    let Some(proxy) = proxy else {
        return Ok(());
    };
    if !proxy.should_proxy(request.uri())
        || !request.headers().contains_key(PROXY_AUTHORIZATION_HEADER)
    {
        return Ok(());
    }
    Err(network_error(
        "network_fetch_failed",
        format!("fetch failed\u{0}{PROXY_AUTHORIZATION_REJECTED}"),
    ))
}

/// One `fetch` attempt; the response headers are awaited, not the body.
async fn fetch_once(
    client: &HttpClient,
    proxy: Option<&ProxyConfig>,
    method: Method,
    url: &Url,
    headers: &HeaderList,
    body: Option<String>,
) -> Result<hyper::Response<Incoming>, McpClientError> {
    fetch_with(method, url, headers, body, |request| async move {
        // Checked per redirect hop: every hop is a new dispatch, and a hop
        // that routes through the proxy must not carry caller credentials.
        reject_proxy_authorization(proxy, &request)?;
        let target = request.uri().clone();
        let response = client.request(request).await;
        let error = match response {
            Ok(response) => return Ok(response),
            Err(error) => error,
        };
        let cause = match classify_failure(&error) {
            // A proxy CONNECT rejection already carries the cause text the TS
            // fetch / EventSource stack reports; its socket error (if any)
            // would otherwise be attributed to the MCP target instead of the
            // proxy.
            FailureCause::Proxy(proxy) => Some(proxy.cause.clone()),
            // Node's address texts name the resolved addresses, never the
            // URL hostname, so the resolver is only consulted for these two
            // shapes; the other causes carry no address.
            FailureCause::Refused => Some(refused_cause(
                &target,
                resolved_attempts(&target).await.as_deref(),
            )),
            FailureCause::ConnectTimeout => Some(timeout_cause(
                &target,
                resolved_attempts(&target).await.as_deref(),
            )),
            FailureCause::Tls(tls) => tls_cause(tls),
        };
        let code = if cause
            .as_deref()
            .is_some_and(|cause| cause.starts_with("connect ECONNREFUSED"))
        {
            "network_connection_refused"
        } else {
            "network_fetch_failed"
        };
        match cause {
            Some(cause) => Err(network_error(code, format!("fetch failed\u{0}{cause}"))),
            None => Err(network_error("network_fetch_failed", "fetch failed".into())),
        }
    })
    .await
}

/// The URL's port, with the scheme's default (undici's `port = port || 443` /
/// `|| 80`) filled in.
fn effective_port(target: &hyper::Uri) -> u16 {
    let default_port = match target.scheme_str() {
        Some("https") => 443,
        _ => 80,
    };
    target.port_u16().unwrap_or(default_port)
}

/// The URL's own peer address in the form Node prints. `http::Uri::host()`
/// keeps IPv6 brackets (`[::1]`), but Node v22 renders the bare address in
/// both `connect ECONNREFUSED ::1:443` (net) and
/// `Connect Timeout Error (attempted address: ::1:443, …)` (undici), so the
/// brackets are stripped. Both forms were measured against the fixed TS
/// baseline; `https-sse-refused-ipv6` pins the refused one.
fn target_address(target: &hyper::Uri) -> String {
    let host = target.host().unwrap_or_default();
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    format!("{host}:{}", effective_port(target))
}

/// The failure path's re-resolution is bounded: the connect has already
/// failed, so the lookup only decorates the error text, and a resolver
/// stalled the same way that caused the failure must not stall the *report*
/// too (macOS / glibc system resolution timeouts run to tens of seconds,
/// past the outer handshake timer that would swap the message for
/// `MCP handshake timed out`). The bound is therefore an order of magnitude
/// below `MCP_CONNECT_TIMEOUT` and far below the handshake deadline. On
/// expiry the addresses are simply unknown and the URL hostname form is
/// used, as before the lookup existed.
const RESOLUTION_DEADLINE: Duration = Duration::from_secs(1);

/// The addresses a hostname URL resolves to, in the platform's `getaddrinfo`
/// order — the list Node's `dns.lookup(host, { all: true })` feeds to its
/// connect attempts (`lookupAndConnectMultiple`, node v22.22.0
/// `lib/net.js:1442-1541`). IP-literal targets skip the lookup: Node never
/// resolves those, and their address text is the URL's own host. The lookup
/// runs under [`RESOLUTION_DEADLINE`]; a stalled resolver cancels the wait
/// (the underlying blocking `getaddrinfo` thread may still finish on its
/// own, but nothing awaits it).
async fn resolved_attempts(target: &hyper::Uri) -> Option<Vec<SocketAddr>> {
    resolved_attempts_with(target, |host, port| async move {
        tokio::net::lookup_host((host, port))
            .await
            .map(|resolved| resolved.collect::<Vec<_>>())
    })
    .await
}

/// [`resolved_attempts`] with the resolver injected, so the deadline is
/// testable against a resolver that never answers.
async fn resolved_attempts_with<F, Fut>(
    target: &hyper::Uri,
    lookup: F,
) -> Option<Vec<SocketAddr>>
where
    F: FnOnce(String, u16) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<Vec<SocketAddr>>>,
{
    let host = target.host()?;
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    if host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    match tokio::time::timeout(
        RESOLUTION_DEADLINE,
        lookup(host.to_owned(), effective_port(target)),
    )
    .await
    {
        Ok(Ok(resolved)) => Some(resolved),
        // A resolver error or an expired deadline: no addresses known, and
        // both address texts fall back to the URL's own host.
        _ => None,
    }
}

/// Node's per-attempt address order (`lookupAndConnectMultiple`, node v22.22.0
/// `lib/net.js:1470-1541`): duplicates dropped, the first address's family
/// preferred, and the two families interleaved. Measured for `localhost`
/// (resolves `::1, 127.0.0.1` here, attempted in that order).
fn node_attempt_order(resolved: &[SocketAddr]) -> Vec<SocketAddr> {
    let prefer_v6 = resolved.first().is_some_and(SocketAddr::is_ipv6);
    let mut preferred: Vec<SocketAddr> = Vec::new();
    let mut fallback: Vec<SocketAddr> = Vec::new();
    for address in resolved {
        let group = if address.is_ipv6() == prefer_v6 {
            &mut preferred
        } else {
            &mut fallback
        };
        if !group.iter().any(|seen| seen.ip() == address.ip()) {
            group.push(*address);
        }
    }
    let mut out = Vec::with_capacity(preferred.len() + fallback.len());
    for index in 0..preferred.len().max(fallback.len()) {
        if let Some(address) = preferred.get(index) {
            out.push(*address);
        }
        if let Some(address) = fallback.get(index) {
            out.push(*address);
        }
    }
    out
}

/// The bare `ip:port` form Node prints for an attempted address — no IPv6
/// brackets (`connect ECONNREFUSED ::1:443`,
/// `attempted addresses: ::1:443, …`).
fn node_address(address: &SocketAddr) -> String {
    format!("{}:{}", address.ip(), address.port())
}

/// undici's refused-connection cause. A hostname URL connects through Node's
/// happy-eyeballs `net.connect`, whose failure is an `AggregateError` of one
/// `connect ECONNREFUSED ip:port` per attempted address; the legacy SSE
/// transport's `flattenError` joins them with `", "`. Measured 2026-10-25
/// (#681, legacy SSE to a closed `http://localhost:<port>/sse`):
/// `connect ECONNREFUSED ::1:60169, connect ECONNREFUSED 127.0.0.1:60169`.
/// An IP-literal URL (no resolution) keeps the single URL address the
/// `https-sse-refused` / `https-sse-refused-ipv6` goldens pin.
fn refused_cause(target: &hyper::Uri, resolved: Option<&[SocketAddr]>) -> String {
    let Some(resolved) = resolved.filter(|resolved| !resolved.is_empty()) else {
        return format!("connect ECONNREFUSED {}", target_address(target));
    };
    node_attempt_order(resolved)
        .iter()
        .map(|address| format!("connect ECONNREFUSED {}", node_address(address)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// undici's `ConnectTimeoutError` text. With several attempted addresses Node
/// lists them all (plural). Otherwise it names the URL's own host: an IP
/// literal, or a hostname whose lookup returned a single address — that path
/// switches back to the plain single connect (`toAttempt.length === 1`, node
/// v22.22.0 `lib/net.js:1536-1547`), never records the resolution, and
/// undici's `onConnectTimeout` falls back to the connector's URL hostname
/// (读码推断，未实测；the multi-address form was measured 2026-10-25, #681:
/// a stalled `https://localhost:<port>/sse` TLS handshake reports
/// `Connect Timeout Error (attempted addresses: ::1:60217, 127.0.0.1:60217,
/// timeout: 10000ms)`).
fn timeout_cause(target: &hyper::Uri, resolved: Option<&[SocketAddr]>) -> String {
    // Node decides singular vs. plural after its own dedup
    // (`toAttempt.length === 1`, node v22.22.0 `lib/net.js:1518`), so a
    // lookup that answers with the same IP twice (a repeated `/etc/hosts`
    // line or a duplicated A record) switches back to the plain single
    // connect and undici reports the URL hostname, singular. Measured
    // 2026-10-25 (#681 fix-r1, a `dns.lookup` patched to answer
    // `[127.0.0.1, 127.0.0.1]`): `Connect Timeout Error (attempted
    // address: dup-probe.invalid:64720, timeout: 10000ms)`.
    let Some(attempts) = resolved
        .map(node_attempt_order)
        .filter(|attempts| attempts.len() > 1)
    else {
        return format!(
            "Connect Timeout Error (attempted address: {}, timeout: {}ms)",
            target_address(target),
            MCP_CONNECT_TIMEOUT.as_millis()
        );
    };
    let addresses = attempts
        .iter()
        .map(node_address)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Connect Timeout Error (attempted addresses: {addresses}, timeout: {}ms)",
        MCP_CONNECT_TIMEOUT.as_millis()
    )
}

/// Every error reachable from `root`, following both `source()` and the
/// payload of `io::Error::new`: `io::Error` stores the wrapped TLS error as a
/// custom payload and its own `source()` chain stops at the `io::Error`, so
/// `get_ref()` is the only way to reach `rustls::Error` (pointer-checked to
/// avoid loops).
fn error_chain<'a>(
    root: &'a (dyn std::error::Error + 'static),
) -> Vec<&'a (dyn std::error::Error + 'static)> {
    fn visit<'a>(
        out: &mut Vec<&'a (dyn std::error::Error + 'static)>,
        item: &'a (dyn std::error::Error + 'static),
    ) {
        if out.iter().any(|seen| std::ptr::eq(*seen, item)) {
            return;
        }
        out.push(item);
        if let Some(inner) = item
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
        {
            visit(out, inner);
        }
        if let Some(source) = item.source() {
            visit(out, source);
        }
    }
    let mut out = Vec::new();
    visit(&mut out, root);
    out
}

/// undici surfaces a rejected fetch as `TypeError: fetch failed` whose cause
/// carries the socket error. The legacy SSE transport's `EventSource` flattens
/// that cause into the rejection (`eventsource@3` `flattenError`), so the text
/// must match Node's for the observable cases.
enum FailureCause<'a> {
    /// The proxy's own `CONNECT` failure; its cause text is already the one
    /// the TS fetch / EventSource stack reports, and its socket error (if
    /// any) must not be attributed to the MCP target.
    Proxy(&'a ProxyError),
    Refused,
    ConnectTimeout,
    Tls(Option<&'a rustls::Error>),
}

fn classify_failure(error: &hyper_util::client::legacy::Error) -> FailureCause<'_> {
    let source = error as &(dyn std::error::Error + 'static);
    let chain = error_chain(source);
    if let Some(proxy) = chain
        .iter()
        .find_map(|source| source.downcast_ref::<ProxyError>())
    {
        return FailureCause::Proxy(proxy);
    }
    let refused = chain.iter().any(|source| {
        source
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionRefused)
    }) || format!("{error:?}").contains("ConnectionRefused");
    if refused {
        return FailureCause::Refused;
    }
    // Both the MCP-local whole-connect deadline and the connector's own TCP
    // timer express undici's single `connectTimeout`; whichever fires first is
    // the same failure to the caller.
    let timed_out = chain.iter().any(|source| {
        source
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::TimedOut)
    });
    if timed_out {
        return FailureCause::ConnectTimeout;
    }
    FailureCause::Tls(
        chain
            .iter()
            .find_map(|source| source.downcast_ref::<rustls::Error>()),
    )
}

fn tls_cause(tls: Option<&rustls::Error>) -> Option<String> {
    match tls {
        // rustls reports every unverifiable chain as `UnknownIssuer`; Node
        // distinguishes `self-signed certificate` (DEPTH_ZERO_SELF_SIGNED_CERT)
        // from `unable to verify the first certificate` (an untrusted CA).
        Some(rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)) => {
            Some("self-signed certificate".into())
        }
        _ => None,
    }
}

/// The exchange is injectable so fetch policy is tested without TCP timing.
async fn fetch_with<B, F, Fut>(
    method: Method,
    url: &Url,
    headers: &HeaderList,
    body: Option<String>,
    send: F,
) -> Result<hyper::Response<B>, McpClientError>
where
    F: FnMut(Request<Full<Bytes>>) -> Fut,
    Fut: std::future::Future<Output = Result<hyper::Response<B>, McpClientError>>,
{
    let mut request = Request::builder();
    for (name, value) in &headers.0 {
        request = request.header(name.as_str(), value.as_str());
    }
    // undici's fetch defaults.
    if !headers.0.iter().any(|(name, _)| name == "accept") {
        request = request.header("accept", "*/*");
    }
    let request = request.body(()).map_err(|error| {
        McpClientError::with_code(
            "TypeError",
            McpErrorCode::Text("ERR_INVALID_ARG_VALUE"),
            error.to_string(),
        )
    })?;
    fetch_redirect::follow(
        url.clone(),
        method,
        request.into_parts().0.headers,
        Bytes::from(body.unwrap_or_default()),
        || network_error("network_fetch_failed", "fetch failed".into()),
        send,
    )
    .await
}

/// Legacy fetch keeps its abort signal attached while response.text() reads.
async fn legacy_post_response<B>(
    response: hyper::Response<B>,
    aborted: &ToolCancellation,
    before_abort: bool,
) -> Result<(), McpClientError>
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Send,
{
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let bytes = fetch_redirect::read_text_body_with_signal(
        response.into_body(),
        // The production value is `MAX_STRING_LENGTH`; the shared thread-local
        // knob lets unit tests shrink it.
        string_limit(),
        Some(crate::runtime::network::UNDICI_BODY_TIMEOUT),
        Some(aborted),
        before_abort,
    )
    .await;
    let text = bytes
        .map(|bytes| decode_text(&bytes))
        .map_err(|error| match error {
            // The SDK awaits the same undici `response.text()` for this POST
            // (sse.js:179); its byte→string conversion throws V8's plain
            // `Error`, not `RangeError` (#641 real-TS probe).
            fetch_redirect::BodyError::TooLong => {
                McpClientError::plain("Error", fetch_redirect::STRING_TOO_LONG)
            }
            fetch_redirect::BodyError::Terminated | fetch_redirect::BodyError::Aborted => {
                McpClientError::plain("TypeError", "terminated")
            }
        })
        .unwrap_or_else(|_| "null".into());
    Err(McpClientError::plain(
        "Error",
        format!(
            "Error POSTing to endpoint (HTTP {}): {text}",
            status.as_u16()
        ),
    ))
}

/// Splits the refused-connection detail kept for the legacy transport's
/// `flattenError` text off a fetch error message.
fn fetch_message(error: &McpClientError) -> (&str, Option<&str>) {
    match error.message().split_once('\u{0}') {
        Some((message, cause)) => (message, Some(cause)),
        None => (error.message(), None),
    }
}

fn strip_cause(error: McpClientError) -> McpClientError {
    match fetch_message(&error) {
        (message, Some(_)) => McpClientError::with_code(
            error.name(),
            error.code().cloned().unwrap_or(McpErrorCode::Text("")),
            message.to_owned(),
        ),
        _ => error,
    }
}

/// `jitteredBackoff(0, { baseDelayMs: 500, maxDelayMs: 5000 })`.
fn retry_delay(retry_after: Option<&str>) -> Duration {
    if let Some(ms) = retry_after_header_ms(retry_after) {
        return node_timer_duration(ms.min(5000.0));
    }
    let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]) % 125;
    Duration::from_millis(500 + jitter)
}

/// TS normalizeNetworkError: an existing NetworkFetchError owns its outcome;
/// only raw rejections recheck cancellation in their error continuation.
fn normalize_network_failure(
    error: McpClientError,
    signal: &ToolCancellation,
    before_abort: bool,
) -> McpClientError {
    if error.name() == "NetworkFetchError" {
        return error;
    }
    if !before_abort && signal.is_cancelled() {
        return network_error(
            "network_abort",
            "Network request aborted by parent signal.".into(),
        );
    }
    let code = match error.code() {
        Some(McpErrorCode::Text(code)) if code.starts_with("network_") => *code,
        code => {
            crate::runtime::model::http::NetworkFetchError::classify(
                error.message(),
                match code {
                    Some(McpErrorCode::Text(code)) | Some(McpErrorCode::Errno(code)) => Some(code),
                    _ => None,
                },
            )
            .code
        }
    };
    network_error(code, error.message().into())
}

/// How a request is fetched.
#[derive(Clone, Copy)]
enum Fetch {
    /// `networkFetch(url, init, { timeoutMs, retry: { maxRetries: 1, ... } })`
    /// (the streamable transport; only GETs are retried). `0` = no timeout.
    Network { timeout_ms: f64 },
    /// The global `fetch` (legacy SSE): no timeout, no retry.
    Plain,
}

#[allow(clippy::too_many_arguments)] // Request fields plus the incoming timer continuation.
async fn network_fetch(
    client: &HttpClient,
    proxy: Option<&ProxyConfig>,
    method: Method,
    url: &Url,
    headers: &HeaderList,
    body: Option<String>,
    mode: Fetch,
    aborted: &ToolCancellation,
    continuation_before_abort: bool,
) -> Result<hyper::Response<(Incoming, bool)>, McpClientError> {
    network_fetch_continuing_with(
        method.clone(),
        mode,
        aborted,
        continuation_before_abort,
        || {
            let (client, method, url, headers, body) = (
                client.clone(),
                method.clone(),
                url.clone(),
                headers.clone(),
                body.clone(),
            );
            let proxy = proxy.cloned();
            ExchangeFuture::io(async move {
                fetch_once(&client, proxy.as_ref(), method, &url, &headers, body)
                    .await
                    .map_err(|error| {
                        // fetch_once carries socket classification metadata in an
                        // SDK-shaped error. Network mode must normalize this raw
                        // fetch failure only after the await/cancellation race.
                        // Plain keeps its existing bare-fetch projection.
                        if matches!(mode, Fetch::Network { .. })
                            && error.name() == "NetworkFetchError"
                        {
                            McpClientError::with_code(
                                "TypeError",
                                error.code().cloned().unwrap(),
                                error.message().into(),
                            )
                        } else {
                            error
                        }
                    })
            })
        },
    )
    .await
}

#[cfg(test)]
async fn network_fetch_with<B: Send + 'static>(
    method: Method,
    mode: Fetch,
    aborted: &ToolCancellation,
    send: impl FnMut() -> ExchangeFuture<Result<hyper::Response<B>, McpClientError>>,
) -> Result<hyper::Response<(B, bool)>, McpClientError> {
    network_fetch_continuing_with(method, mode, aborted, false, send).await
}

/// Same policy as the socket path, with observable settlement for deterministic tests.
async fn network_fetch_continuing_with<B: Send + 'static>(
    method: Method,
    mode: Fetch,
    aborted: &ToolCancellation,
    mut continuation_before_abort: bool,
    mut send: impl FnMut() -> ExchangeFuture<Result<hyper::Response<B>, McpClientError>>,
) -> Result<hyper::Response<(B, bool)>, McpClientError> {
    let (timeout_ms, retries) = match mode {
        Fetch::Network { timeout_ms } => (timeout_ms, true),
        Fetch::Plain => (0.0, false),
    };
    let retryable_method = retries && method == Method::GET;
    for attempt in 0..=1 {
        if !continuation_before_abort && aborted.is_cancelled() {
            return Err(network_error(
                "network_abort",
                "Network request aborted by parent signal.".into(),
            ));
        }
        let deadline = (timeout_ms > 0.0)
            .then(|| tokio::time::Instant::now() + node_timer_duration(timeout_ms));
        let outcome = match exchange_continuing(
            send(),
            aborted.cancelled(),
            || aborted.abort_event_at(),
            deadline,
            continuation_before_abort,
        )
        .await
        {
            Exchange::Response(outcome, before_abort) => {
                continuation_before_abort = before_abort;
                if retries {
                    outcome.map_err(|error| normalize_network_failure(error, aborted, before_abort))
                } else {
                    outcome
                }
            }
            Exchange::Timeout { before_abort } => {
                continuation_before_abort = before_abort;
                Err(network_error(
                    "network_timeout",
                    format!(
                        "Network request timed out after {}ms.",
                        js_value::number_string(timeout_ms)
                    ),
                ))
            }
            Exchange::Aborted => Err(network_error(
                "network_abort",
                "Network request aborted by parent signal.".into(),
            )),
        };
        let last = attempt == 1 || !retryable_method;
        let wait = match outcome {
            Ok(response) if !last && RETRY_STATUSES.contains(&response.status().as_u16()) => {
                let retry_after = header_value(response.headers(), "retry-after");
                drop(response);
                retry_delay(retry_after.as_deref())
            }
            Ok(response) => return Ok(response.map(|body| (body, continuation_before_abort))),
            Err(error) => {
                let code = error.code().cloned();
                let retryable = !matches!(
                    code,
                    Some(McpErrorCode::Text("network_abort" | "network_tls_error"))
                );
                if last || !retryable {
                    return Err(error);
                }
                retry_delay(None)
            }
        };
        continuation_before_abort = match delay(
            wait,
            aborted.cancelled(),
            || aborted.abort_event_at(),
            None,
            continuation_before_abort,
        )
        .await
        {
            Exchange::Response((), before_abort) => before_abort,
            Exchange::Aborted => {
                return Err(network_error(
                    "network_abort",
                    "Network retry aborted.".into(),
                ));
            }
            Exchange::Timeout { .. } => {
                unreachable!("MCP retry waits have no surrounding deadline")
            }
        };
    }
    unreachable!("the last attempt returns")
}

/// `eventsource-parser@3.0.8` over decoded text.
#[derive(Default)]
pub(super) struct SseParser {
    pending: String,
    id: Option<String>,
    data: String,
    data_lines: usize,
    data_units: usize,
    event: Option<String>,
}

#[derive(Debug, PartialEq)]
pub(super) enum SseItem {
    Event {
        id: Option<String>,
        event: Option<String>,
        data: String,
    },
    Retry(f64),
}

impl SseParser {
    #[cfg(test)]
    pub(super) fn feed(&mut self, chunk: &str, out: &mut Vec<SseItem>) {
        assert!(self.feed_limited(chunk, out, fetch_redirect::MAX_STRING_LENGTH));
    }
    fn feed_limited(&mut self, chunk: &str, out: &mut Vec<SseItem>, limit: usize) -> bool {
        if self
            .pending
            .encode_utf16()
            .count()
            .saturating_add(chunk.encode_utf16().count())
            > limit
        {
            return false;
        }
        self.pending.push_str(chunk);
        let input = std::mem::take(&mut self.pending);
        let bytes = input.as_bytes();
        let mut start = 0;
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'\n' => {
                    self.line(&input[start..index], out);
                    if self.data_units > limit {
                        return false;
                    }
                    index += 1;
                    start = index;
                }
                b'\r' => {
                    if index + 1 == bytes.len() {
                        // A trailing CR may be the first half of CRLF.
                        break;
                    }
                    self.line(&input[start..index], out);
                    if self.data_units > limit {
                        return false;
                    }
                    index += 1;
                    if bytes.get(index) == Some(&b'\n') {
                        index += 1;
                    }
                    start = index;
                }
                _ => index += 1,
            }
        }
        self.pending = input[start..].to_owned();
        true
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseItem>) {
        if line.is_empty() {
            if self.data_lines > 0 {
                out.push(SseItem::Event {
                    id: self.id.take(),
                    event: self.event.take(),
                    data: std::mem::take(&mut self.data),
                });
            }
            self.id = None;
            self.event = None;
            self.data.clear();
            self.data_lines = 0;
            self.data_units = 0;
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.find(':') {
            Some(at) => {
                let value = &line[at + 1..];
                (&line[..at], value.strip_prefix(' ').unwrap_or(value))
            }
            None => (line, ""),
        };
        match field {
            "event" => self.event = (!value.is_empty()).then(|| value.to_owned()),
            "data" => {
                if self.data_lines > 0 {
                    self.data.push('\n');
                    self.data_units = self.data_units.saturating_add(1);
                }
                self.data.push_str(value);
                self.data_units = self.data_units.saturating_add(value.encode_utf16().count());
                self.data_lines += 1;
            }
            "id" => self.id = (!value.contains('\0')).then(|| value.to_owned()),
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                out.push(SseItem::Retry(value.parse::<f64>().unwrap_or(f64::NAN)));
            }
            _ => {}
        }
    }
}

/// Incremental UTF-8 decoding of a body (`TextDecoderStream`: BOM dropped,
/// invalid bytes replaced, split characters carried over).
#[derive(Default)]
struct Utf8Stream {
    carry: Vec<u8>,
    started: bool,
}
impl Utf8Stream {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.carry.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.carry) {
                Ok(text) => {
                    out.push_str(text);
                    self.carry.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    out.push_str(std::str::from_utf8(&self.carry[..valid]).expect("valid prefix"));
                    match error.error_len() {
                        Some(len) => {
                            out.push('\u{FFFD}');
                            self.carry.drain(..valid + len);
                        }
                        None => {
                            self.carry.drain(..valid);
                            break;
                        }
                    }
                }
            }
        }
        if !self.started && !out.is_empty() {
            self.started = true;
            if let Some(rest) = out.strip_prefix('\u{FEFF}') {
                return rest.to_owned();
            }
        }
        out
    }
}

/// Production reads always use Node's global string bound (V8
/// `MAX_STRING_LENGTH`). Unit tests can shrink it on their current thread to
/// exercise the actual body call sites without allocating a 512 MiB string.
///
/// There is one function and one default tail expression for both builds. A
/// test only overrides it explicitly through [`StringLimitGuard`]; with no
/// override the test build runs the same `fetch_redirect::MAX_STRING_LENGTH`
/// expression as production. The production build does not compile the
/// override branch.
fn string_limit() -> usize {
    #[cfg(test)]
    {
        if let Some(limit) = STRING_LIMIT.with(std::cell::Cell::get) {
            return limit;
        }
    }
    fetch_redirect::MAX_STRING_LENGTH
}

#[cfg(test)]
thread_local! {
    /// `None` (the default) means "use the shared V8 bound"; only
    /// [`StringLimitGuard`] sets a shrunken test limit, and nested guards
    /// restore the previous value on drop.
    static STRING_LIMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Both streamable response.text() and response.json() use undici's body
/// idle timer and V8 string bound; an over-limit body is drained to EOF
/// before the string error is raised, so a body failure wins (#641/#647
/// real-TS probes). Their callers retain the SDK's distinct failure
/// projection (error POST substitutes null; JSON propagates failure).
async fn streamable_text<B>(
    body: B,
    aborted: &ToolCancellation,
    before_abort: bool,
) -> Result<String, McpClientError>
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Send,
{
    fetch_redirect::read_text_body_with_signal(
        body,
        string_limit(),
        Some(crate::runtime::network::UNDICI_BODY_TIMEOUT),
        Some(aborted),
        before_abort,
    )
    .await
    .map(|bytes| decode_text(&bytes))
    .map_err(|error| match error {
        // Both SDK call sites run undici's `Response.text()` /
        // `Response.json()` (streamableHttp.js:317/398), and `json()` parses
        // the same `utf8DecodeBytes` string, so the over-limit conversion
        // throws V8's plain `Error`, not `RangeError` (#641 real-TS probe).
        fetch_redirect::BodyError::TooLong => {
            McpClientError::plain("Error", fetch_redirect::STRING_TOO_LONG)
        }
        fetch_redirect::BodyError::Terminated | fetch_redirect::BodyError::Aborted => {
            network_error("network_connection_reset", "terminated".into())
        }
    })
}

async fn streamable_error_response<B>(
    response: hyper::Response<B>,
    aborted: &ToolCancellation,
    before_abort: bool,
) -> McpClientError
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Send,
{
    let status = response.status();
    let text = streamable_text(response.into_body(), aborted, before_abort)
        .await
        .unwrap_or_else(|_| "null".into());
    http_error(
        f64::from(status.as_u16()),
        format!("Error POSTing to endpoint: {text}"),
    )
}

/// Reads an event stream to its end (or until the transport aborts).
/// Returns `false` when it ended because of an abort or a read error.
async fn read_events<B: hyper::body::Body<Data = Bytes>>(
    body: B,
    aborted: &ToolCancellation,
    mut continuation: bool,
    mut on_item: impl FnMut(SseItem),
) -> bool {
    let mut body = std::pin::pin!(body);
    let mut decoder = Utf8Stream::default();
    let mut parser = SseParser::default();
    loop {
        let frame = match crate::runtime::network::body_frame(
            &mut body,
            Some(crate::runtime::network::UNDICI_BODY_TIMEOUT),
            Some(aborted),
            &mut continuation,
        )
        .await
        {
            Exchange::Response(frame, _) => frame,
            Exchange::Aborted | Exchange::Timeout { .. } => return false,
        };
        match frame {
            None => return true,
            Some(Err(_)) => return false,
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    let mut items = Vec::new();
                    if !parser.feed_limited(&decoder.push(&data), &mut items, string_limit()) {
                        return false;
                    }
                    for item in items {
                        on_item(item);
                        if !continuation && aborted.is_cancelled() {
                            return false;
                        }
                    }
                }
            }
        }
    }
}

/// `JSONRPCMessageSchema.parse(JSON.parse(text))`.
fn parse_message(text: &str) -> Result<Inbound, McpClientError> {
    if let Some(message) = json_syntax::parse_error(text) {
        return Err(McpClientError::plain("SyntaxError", message));
    }
    let value =
        parse_json(text).map_err(|error| McpClientError::plain("SyntaxError", error.message))?;
    parse_inbound_value(&value).map_err(|message| McpClientError::plain("ZodError", message))
}

impl HttpLink {
    /// Unit-test construction over the bundled webpki roots. Production goes
    /// through [`Self::with_config`] so `McpClient` can supply its trust
    /// configuration without changing the shared connector.
    #[cfg(test)]
    pub(super) fn new(
        kind: HttpKind,
        url: Url,
        headers: Vec<(String, String)>,
        post_timeout_ms: f64,
        get_timeout_ms: f64,
    ) -> Self {
        Self::with_config(
            kind,
            url,
            headers,
            post_timeout_ms,
            get_timeout_ms,
            crate::runtime::model::http::webpki_client_config(),
            None,
        )
    }

    /// Trust exactly `config` (the `McpClient` test seam passes the fixture
    /// CA in through `rooted_client_config`). The shared connector keeps its
    /// own TCP timeout; the MCP-local deadline adds undici's whole-connect
    /// bound around it. `proxy` is the `pilotdeck.yaml` source; env-only
    /// behavior is `None`.
    pub(super) fn with_config(
        kind: HttpKind,
        url: Url,
        headers: Vec<(String, String)>,
        post_timeout_ms: f64,
        get_timeout_ms: f64,
        config: rustls::ClientConfig,
        proxy: Option<&ProxySettings>,
    ) -> Self {
        let proxy = ProxyConfig::from_settings(proxy);
        let connector = ConnectDeadline::new(
            proxy_https_connector(config, proxy.clone()),
            MCP_CONNECT_TIMEOUT,
        );
        Self {
            kind,
            url,
            headers,
            post_timeout_ms,
            get_timeout_ms,
            client: Client::builder(TokioExecutor::new()).build(connector),
            proxy,
            state: Mutex::new(LinkState::default()),
            aborted: ToolCancellation::default(),
        }
    }

    pub(super) fn set_protocol_version(&self, version: String) {
        lock(&self.state).protocol_version = Some(version);
    }

    /// `_commonHeaders()`.
    fn common_headers(&self) -> HeaderList {
        let state = lock(&self.state);
        // The record `{ ...headers, ...extraHeaders }`: exact keys overwrite,
        // then `new Headers(record)` appends case-insensitive duplicates.
        let mut record: Vec<(String, String)> = Vec::new();
        let mut put = |key: &str, value: &str| match record.iter_mut().find(|(k, _)| k == key) {
            Some((_, existing)) => *existing = value.to_owned(),
            None => record.push((key.to_owned(), value.to_owned())),
        };
        if self.kind == HttpKind::Streamable
            && let Some(session) = state.session_id.as_deref().filter(|s| !s.is_empty())
        {
            put("mcp-session-id", session);
        }
        if let Some(version) = state.protocol_version.as_deref().filter(|v| !v.is_empty()) {
            put("mcp-protocol-version", version);
        }
        for (key, value) in &self.headers {
            put(key, value);
        }
        let mut headers = HeaderList::default();
        for (key, value) in &record {
            headers.append(key, value);
        }
        headers
    }

    /// `transport.close()`: abort the controller and every open stream.
    pub(super) fn close(&self) {
        self.aborted.cancel_synchronously();
    }

    fn is_aborted(&self) -> bool {
        self.aborted.is_cancelled()
    }

    /// `transport.start()`. Streamable HTTP does no I/O; the legacy
    /// transport opens its `EventSource` and waits for the `endpoint` event.
    pub(super) async fn start(&self, wire: &Arc<Wire>) -> Result<(), McpClientError> {
        if self.kind == HttpKind::Streamable {
            return Ok(());
        }
        let (ready, outcome) = oneshot::channel();
        tokio::spawn(event_source(Arc::downgrade(wire), ready));
        outcome
            .await
            .unwrap_or_else(|_| Err(McpClientError::connection_closed()))
    }

    /// `transport.send(message)`.
    pub(super) async fn send(
        &self,
        wire: &Arc<Wire>,
        message: JsValue,
    ) -> Result<(), McpClientError> {
        match self.kind {
            HttpKind::Streamable => self.send_streamable(wire, message).await,
            HttpKind::LegacySse => self.send_legacy(message).await,
        }
    }

    async fn send_streamable(
        &self,
        wire: &Arc<Wire>,
        message: JsValue,
    ) -> Result<(), McpClientError> {
        let mut headers = self.common_headers();
        headers.set("content-type", "application/json");
        headers.set("accept", "application/json, text/event-stream");
        let body = frame_text(&message)?;
        let response = network_fetch(
            &self.client,
            self.proxy.as_ref(),
            Method::POST,
            &self.url,
            &headers,
            Some(body),
            Fetch::Network {
                timeout_ms: self.post_timeout_ms,
            },
            &self.aborted,
            false,
        )
        .await
        .map_err(strip_cause)?;
        let before_abort = response.body().1;
        let response = response.map(|(body, _)| body);
        if let Some(session) =
            header_value(response.headers(), "mcp-session-id").filter(|s| !s.is_empty())
        {
            lock(&self.state).session_id = Some(session);
        }
        let status = response.status();
        if !status.is_success() {
            return Err(streamable_error_response(response, &self.aborted, before_abort).await);
        }
        if status == StatusCode::ACCEPTED {
            drop(response);
            let initialized = message
                .get("method")
                .and_then(JsValue::as_string)
                .is_some_and(|method| method.equals_str("notifications/initialized"))
                && message.get("id").is_none();
            if initialized {
                tokio::spawn(start_sse(Arc::downgrade(wire), None, before_abort));
            }
            return Ok(());
        }
        let has_request = message.get("method").is_some()
            && message.get("id").is_some_and(|id| !id.is_undefined());
        if !has_request {
            return Ok(());
        }
        let content_type = header_value(response.headers(), header::CONTENT_TYPE.as_str());
        let content = content_type.as_deref().unwrap_or_default();
        if content.contains("text/event-stream") {
            tokio::spawn(handle_sse_stream(
                Arc::downgrade(wire),
                response.into_body(),
                before_abort,
                false,
            ));
            return Ok(());
        }
        if content.contains("application/json") {
            let text = streamable_text(response.into_body(), &self.aborted, before_abort).await?;
            if let Some(message) = json_syntax::parse_error(&text) {
                return Err(McpClientError::plain("SyntaxError", message));
            }
            let data = parse_json(&text)
                .map_err(|error| McpClientError::plain("SyntaxError", error.message))?;
            let items = match &data {
                JsValue::Array(items) => items.iter().collect::<Vec<_>>(),
                single => vec![single],
            };
            let mut messages = Vec::with_capacity(items.len());
            for item in items {
                messages.push(
                    parse_inbound_value(item)
                        .map_err(|message| McpClientError::plain("ZodError", message))?,
                );
            }
            wire.dispatch_http(messages);
            return Ok(());
        }
        Err(http_error(
            -1.0,
            format!(
                "Unexpected content type: {}",
                content_type.as_deref().unwrap_or("null")
            ),
        ))
    }

    async fn send_legacy(&self, message: JsValue) -> Result<(), McpClientError> {
        let Some(endpoint) = lock(&self.state).endpoint.clone() else {
            return Err(McpClientError::not_connected());
        };
        let mut headers = self.common_headers();
        headers.set("content-type", "application/json");
        let body = frame_text(&message)?;
        // Global `fetch`: no timeout, no retry; aborted with the transport.
        let response = network_fetch(
            &self.client,
            self.proxy.as_ref(),
            Method::POST,
            &endpoint,
            &headers,
            Some(body),
            Fetch::Plain,
            &self.aborted,
            false,
        )
        .await
        .map_err(|error| {
            let (message, _) = fetch_message(&error);
            McpClientError::plain("TypeError", message.to_owned())
        })?;
        let before_abort = response.body().1;
        let response = response.map(|(body, _)| body);
        legacy_post_response(response, &self.aborted, before_abort).await
    }

    /// `_getNextReconnectionDelay(attempt)`.
    fn reconnection_delay(&self, attempt: u32) -> Duration {
        let ms = lock(&self.state).server_retry_ms.unwrap_or_else(|| {
            (INITIAL_RECONNECTION_DELAY_MS * RECONNECTION_GROW_FACTOR.powi(attempt as i32))
                .min(MAX_RECONNECTION_DELAY_MS)
        });
        super::wire::node_delay(ms)
    }
}

fn link_of(wire: &Arc<Wire>) -> &HttpLink {
    wire.http_link()
        .expect("HTTP transport task on an HTTP wire")
}

/// `_startOrAuthSse({ resumptionToken })`: open the GET event stream; a 405
/// means the server offers none. Errors only reach `onerror`.
async fn start_sse(
    wire: Weak<Wire>,
    resumption: Option<String>,
    continuation_before_abort: bool,
) -> Result<(), McpClientError> {
    let Some(strong) = wire.upgrade() else {
        return Ok(());
    };
    let link = link_of(&strong);
    let mut headers = link.common_headers();
    headers.set("Accept", "text/event-stream");
    if let Some(token) = resumption.as_deref().filter(|token| !token.is_empty()) {
        headers.set("last-event-id", token);
    }
    let response = network_fetch(
        &link.client,
        link.proxy.as_ref(),
        Method::GET,
        &link.url,
        &headers,
        None,
        Fetch::Network {
            timeout_ms: link.get_timeout_ms,
        },
        &link.aborted,
        continuation_before_abort,
    )
    .await?;
    let before_abort = response.body().1;
    let response = response.map(|(body, _)| body);
    let status = response.status();
    if !status.is_success() {
        if status == StatusCode::METHOD_NOT_ALLOWED {
            return Ok(());
        }
        return Err(http_error(
            f64::from(status.as_u16()),
            format!(
                "Failed to open SSE stream: {}",
                status.canonical_reason().unwrap_or_default()
            ),
        ));
    }
    drop(strong);
    tokio::spawn(handle_sse_stream(
        wire,
        response.into_body(),
        before_abort,
        true,
    ));
    Ok(())
}

/// `_handleSseStream(stream, options, isReconnectable)`.
async fn handle_sse_stream(
    wire: Weak<Wire>,
    body: Incoming,
    before_abort: bool,
    reconnectable: bool,
) {
    let Some(strong) = wire.upgrade() else {
        return;
    };
    let aborted = link_of(&strong).aborted.clone();
    drop(strong);
    let mut last_event_id: Option<String> = None;
    let mut primed = false;
    let mut received_response = false;
    read_events(body, &aborted, before_abort, |item| {
        let Some(wire) = wire.upgrade() else {
            return;
        };
        match item {
            SseItem::Retry(ms) => lock(&link_of(&wire).state).server_retry_ms = Some(ms),
            SseItem::Event { id, event, data } => {
                if let Some(id) = id.filter(|id| !id.is_empty()) {
                    last_event_id = Some(id);
                    primed = true;
                }
                if data.is_empty() || event.as_deref().is_some_and(|e| e != "message") {
                    return;
                }
                if let Ok(message) = parse_message(&data) {
                    if matches!(message, Inbound::Result { .. }) {
                        received_response = true;
                    }
                    wire.dispatch_http(vec![message]);
                }
            }
        }
    })
    .await;
    if (reconnectable || primed) && !received_response && !aborted.is_cancelled() {
        schedule_reconnection(wire, last_event_id, 0);
    }
}

/// `_scheduleReconnection(options, attemptCount)`.
fn schedule_reconnection(wire: Weak<Wire>, resumption: Option<String>, attempt: u32) {
    if attempt >= MAX_RECONNECTION_RETRIES {
        return;
    }
    let Some(strong) = wire.upgrade() else {
        return;
    };
    let link = link_of(&strong);
    let delay = link.reconnection_delay(attempt);
    let aborted = link.aborted.clone();
    drop(strong);
    tokio::spawn(async move {
        let Some(before_abort) = reconnect_wait(delay, &aborted).await else {
            return;
        };
        if start_sse(wire.clone(), resumption.clone(), before_abort)
            .await
            .is_err()
        {
            schedule_reconnection(wire, resumption, attempt + 1);
        }
    });
}

/// All SSE reconnect paths share the same ordered, cancellable timer.
async fn reconnect_wait(duration: Duration, aborted: &ToolCancellation) -> Option<bool> {
    match delay(
        duration,
        aborted.cancelled(),
        || aborted.abort_event_at(),
        None,
        false,
    )
    .await
    {
        Exchange::Response((), before_abort) => Some(before_abort),
        Exchange::Aborted => None,
        Exchange::Timeout { .. } => unreachable!("SSE reconnect has no surrounding deadline"),
    }
}

fn settle(
    ready: &mut Option<oneshot::Sender<Result<(), McpClientError>>>,
    outcome: Result<(), McpClientError>,
) {
    if let Some(ready) = ready.take() {
        let _ = ready.send(outcome);
    }
}

/// The legacy transport's `EventSource`: connect, report the first outcome
/// through `ready`, dispatch messages, reconnect after a clean end.
async fn event_source(wire: Weak<Wire>, ready: oneshot::Sender<Result<(), McpClientError>>) {
    let mut ready = Some(ready);
    let mut interval = Duration::from_millis(EVENT_SOURCE_RECONNECT_MS);
    let mut continuation_before_abort = false;
    let sse_error =
        |message: String| McpClientError::plain("Error", format!("SSE error: {message}"));
    loop {
        let Some(strong) = wire.upgrade() else {
            return;
        };
        let link = link_of(&strong);
        if !continuation_before_abort && link.is_aborted() {
            return;
        }
        let mut headers = link.common_headers();
        headers.set("Accept", "text/event-stream");
        let fetched = network_fetch(
            &link.client,
            link.proxy.as_ref(),
            Method::GET,
            &link.url,
            &headers,
            None,
            Fetch::Plain,
            &link.aborted,
            continuation_before_abort,
        )
        .await;
        let response = match fetched {
            Ok(response) => response,
            Err(error) => {
                if link.is_aborted() {
                    return;
                }
                let message = match fetch_message(&error) {
                    (message, Some(cause)) => format!("TypeError: {message}: {cause}"),
                    (message, None) => format!("TypeError: {message}"),
                };
                // The first failure rejects start(); later ones retry.
                if ready.is_some() {
                    settle(&mut ready, Err(sse_error(message)));
                    return;
                }
                let aborted = link.aborted.clone();
                drop(strong);
                let Some(before_abort) = reconnect_wait(interval, &aborted).await else {
                    return;
                };
                continuation_before_abort = before_abort;
                continue;
            }
        };
        let before_abort = response.body().1;
        let response = response.map(|(body, _)| body);
        let status = response.status();
        let fail = if status == StatusCode::NO_CONTENT {
            Some("Server sent HTTP 204, not reconnecting".to_owned())
        } else if status != StatusCode::OK {
            Some(format!("Non-200 status code ({})", status.as_u16()))
        } else if !header_value(response.headers(), header::CONTENT_TYPE.as_str())
            .unwrap_or_default()
            .starts_with("text/event-stream")
        {
            Some("Invalid content type, expected \"text/event-stream\"".to_owned())
        } else {
            None
        };
        if let Some(message) = fail {
            settle(&mut ready, Err(sse_error(message)));
            return;
        }
        let aborted = link.aborted.clone();
        let base = link.url.clone();
        drop(strong);
        let retry = legacy_stream(
            &wire,
            response.into_body(),
            &aborted,
            &base,
            &mut ready,
            before_abort,
        )
        .await;
        if aborted.is_cancelled() {
            return;
        }
        if let Some(ms) = retry {
            interval = super::wire::node_delay(ms);
        }
        let Some(before_abort) = reconnect_wait(interval, &aborted).await else {
            return;
        };
        continuation_before_abort = before_abort;
    }
}

/// Handle one EventSource body. Endpoint errors close immediately, including
/// after the initial endpoint has resolved start().
async fn legacy_stream<B: hyper::body::Body<Data = Bytes>>(
    wire: &Weak<Wire>,
    body: B,
    aborted: &ToolCancellation,
    base: &Url,
    ready: &mut Option<oneshot::Sender<Result<(), McpClientError>>>,
    before_abort: bool,
) -> Option<f64> {
    let mut retry = None;
    let mut endpoint_failure = None;
    read_events(body, aborted, before_abort, |item| {
        let Some(wire) = wire.upgrade() else {
            return;
        };
        match item {
            SseItem::Retry(ms) => retry = Some(ms),
            SseItem::Event { event, data, .. } => match event.as_deref() {
                Some("endpoint") => {
                    let resolved = match base.join(&data) {
                        Err(_) => Err(McpClientError::with_code(
                            "TypeError",
                            McpErrorCode::Text("ERR_INVALID_URL"),
                            "Invalid URL".into(),
                        )),
                        Ok(url) if url.origin() != base.origin() => Err(McpClientError::plain(
                            "Error",
                            format!(
                                "Endpoint origin does not match connection origin: {}",
                                url.origin().ascii_serialization()
                            ),
                        )),
                        Ok(url) => Ok(url),
                    };
                    match resolved {
                        Ok(url) => {
                            lock(&link_of(&wire).state).endpoint = Some(url);
                            settle(ready, Ok(()));
                        }
                        Err(error) => {
                            settle(ready, Err(error));
                            endpoint_failure = Some(());
                            // EventSource.close() aborts now, even after
                            // start() resolved and even if the peer never
                            // ends this stream.
                            link_of(&wire).close();
                        }
                    }
                }
                None | Some("message") => {
                    if let Ok(message) = parse_message(&data) {
                        wire.dispatch_http(vec![message]);
                    }
                }
                Some(_) => {}
            },
        }
    })
    .await;
    if endpoint_failure.is_some() {
        // `void this.close()`.
        if let Some(wire) = wire.upgrade() {
            wire.close().await;
        }
    }
    retry
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn poll_pending<F: std::future::Future>(mut future: std::pin::Pin<&mut F>) {
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }

    async fn poll_ready<F: std::future::Future>(mut future: std::pin::Pin<&mut F>) -> F::Output {
        std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
            std::task::Poll::Ready(result) => std::task::Poll::Ready(result),
            std::task::Poll::Pending => panic!("must complete without advancing the paused clock"),
        })
        .await
    }

    /// The fetch result carries headers ordering through JSON, SSE and both
    /// POST-error projections. No socket or wall-clock scheduling is involved.
    async fn assert_headers_reach_body(kind: &str) {
        use http_body_util::combinators::BoxBody;
        for (parent_ms, sync, stalled, expected) in [
            (30, false, false, true),
            (10, false, false, false),
            (30, true, false, false),
            (30, false, true, false),
        ] {
            let signal = ToolCancellation::new();
            let started = tokio::time::Instant::now();
            let mut pending = Box::pin(network_fetch_with(
                Method::POST,
                Fetch::Plain,
                &signal,
                || {
                    let body: BoxBody<Bytes, std::convert::Infallible> = if stalled {
                        StalledBody.boxed()
                    } else {
                        Full::new(Bytes::from_static(if kind == "sse" {
                            b"data: FIRST\n\ndata: SECOND\n\n"
                        } else {
                            b"FIRST"
                        }))
                        .boxed()
                    };
                    ExchangeFuture::after(
                        Duration::from_millis(20),
                        Ok(hyper::Response::builder()
                            .status(if kind.ends_with("error") { 400 } else { 200 })
                            .body(body)
                            .unwrap()),
                    )
                },
            ));
            poll_pending(pending.as_mut()).await;
            tokio::time::advance(Duration::from_millis(70)).await;
            if sync {
                signal.cancel_synchronously();
            } else {
                signal.cancel_at(started + Duration::from_millis(parent_ms));
            }
            let fetched = poll_ready(pending.as_mut()).await;
            if let Err(error) = fetched {
                assert!(!expected);
                assert_eq!(error.code(), Some(&McpErrorCode::Text("network_abort")));
                continue;
            }
            let response = fetched.unwrap();
            let before_abort = response.body().1;
            let response = response.map(|(body, _)| body);
            let text = match kind {
                "json" => streamable_text(response.into_body(), &signal, before_abort)
                    .await
                    .ok(),
                "streamable-error" => Some(
                    streamable_error_response(response, &signal, before_abort)
                        .await
                        .to_string(),
                ),
                "legacy-error" => Some(
                    legacy_post_response(response, &signal, before_abort)
                        .await
                        .unwrap_err()
                        .to_string(),
                ),
                "sse" => {
                    let mut events = Vec::new();
                    read_events(response.into_body(), &signal, before_abort, |event| {
                        events.push(event)
                    })
                    .await;
                    if expected {
                        assert_eq!(
                            events.len(),
                            2,
                            "one ready frame drains both events before the later timer"
                        );
                    }
                    Some(format!("{events:?}"))
                }
                _ => unreachable!(),
            };
            assert_eq!(
                text.as_deref().is_some_and(|s| s.contains("FIRST")),
                expected,
                "kind={kind}, sync={sync}, stalled={stalled}: {text:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn streamable_headers_continue_into_json() {
        assert_headers_reach_body("json").await;
    }
    #[tokio::test(start_paused = true)]
    async fn streamable_headers_continue_into_error_text() {
        assert_headers_reach_body("streamable-error").await;
    }
    #[tokio::test(start_paused = true)]
    async fn streamable_headers_continue_into_sse() {
        assert_headers_reach_body("sse").await;
    }
    #[tokio::test(start_paused = true)]
    async fn legacy_headers_continue_into_error_text() {
        assert_headers_reach_body("legacy-error").await;
    }

    /// Covers the timer shared by streamable reconnection, EventSource EOF
    /// and EventSource fetch failure. Poll once at a time: bare sleep must not
    /// hide cancellation through paused-time auto-advance.
    #[tokio::test(start_paused = true)]
    async fn sse_reconnect_wait_orders_timer_and_abort() {
        for (parent_ms, sync, expected) in [
            (10, false, None),
            (20, false, Some(true)),
            (30, false, Some(true)),
            (30, true, None),
        ] {
            let signal = ToolCancellation::new();
            let started = tokio::time::Instant::now();
            let mut wait = Box::pin(reconnect_wait(Duration::from_millis(20), &signal));
            poll_pending(wait.as_mut()).await;
            tokio::time::advance(Duration::from_millis(70)).await;
            if sync {
                signal.cancel_synchronously();
            } else {
                signal.cancel_at(started + Duration::from_millis(parent_ms));
            }
            assert_eq!(
                poll_ready(wait.as_mut()).await,
                expected,
                "parent={parent_ms}, sync={sync}"
            );
        }
        let signal = ToolCancellation::new();
        let mut wait = Box::pin(reconnect_wait(Duration::from_secs(30), &signal));
        poll_pending(wait.as_mut()).await;
        signal.cancel_synchronously();
        assert_eq!(poll_ready(wait.as_mut()).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn sse_reconnect_continuation_reaches_next_fetch() {
        for ready in [true, false] {
            let signal = ToolCancellation::new();
            let started = tokio::time::Instant::now();
            let mut wait = Box::pin(reconnect_wait(Duration::from_millis(20), &signal));
            poll_pending(wait.as_mut()).await;
            tokio::time::advance(Duration::from_millis(70)).await;
            signal.cancel_at(started + Duration::from_millis(30));
            let before_abort = poll_ready(wait.as_mut()).await.unwrap();
            let mut fetch = Box::pin(network_fetch_continuing_with(
                Method::GET,
                Fetch::Plain,
                &signal,
                before_abort,
                || {
                    ExchangeFuture::io(async move {
                        if !ready {
                            std::future::pending::<()>().await;
                        }
                        Ok(hyper::Response::new(()))
                    })
                },
            ));
            assert_eq!(
                poll_ready(fetch.as_mut()).await.is_ok(),
                ready,
                "ready headers continue; pending socket must yield to abort"
            );
        }
    }

    /// Restores the thread's limit even if the test's assertion panics.
    struct StringLimitGuard(Option<usize>);
    impl StringLimitGuard {
        fn set(limit: usize) -> Self {
            Self(STRING_LIMIT.with(|cell| cell.replace(Some(limit))))
        }
    }
    impl Drop for StringLimitGuard {
        fn drop(&mut self) {
            STRING_LIMIT.with(|cell| cell.set(self.0));
        }
    }

    /// Pin V8's real contract literal (Node 22.22
    /// `buffer.constants.MAX_STRING_LENGTH`), not the constant against
    /// itself: a mutation of the shared default tail must fail here (#642).
    #[test]
    fn string_limit_defaults_to_v8_max() {
        assert_eq!(string_limit(), 0x1fffffe8);
    }

    /// Nested guards restore the previous override, and the outermost drop
    /// returns to the shared production default rather than leaving an
    /// override behind.
    #[test]
    fn string_limit_guard_nests_and_drop_restores_the_shared_default() {
        assert_eq!(string_limit(), 0x1fffffe8);
        {
            let _outer = StringLimitGuard::set(4);
            assert_eq!(string_limit(), 4);
            {
                let _inner = StringLimitGuard::set(8);
                assert_eq!(string_limit(), 8);
            }
            assert_eq!(string_limit(), 4);
        }
        assert_eq!(string_limit(), 0x1fffffe8);
    }

    /// A panicking test body still restores the default: the guard's Drop
    /// runs while unwinding, so the override cannot leak past the test.
    #[test]
    fn string_limit_guard_restores_the_default_across_a_panic() {
        let result = std::panic::catch_unwind(|| {
            let _limit = StringLimitGuard::set(4);
            // The override is visible, so this literal assert must panic.
            assert_eq!(string_limit(), 0x1fffffe8);
        });
        assert!(
            result.is_err(),
            "the override must make the inner assert fail"
        );
        assert_eq!(string_limit(), 0x1fffffe8);
    }

    #[tokio::test]
    async fn streamable_body_uses_global_string_limit() {
        // Literal V8 contract, not the constant against itself (#642).
        assert_eq!(string_limit(), 0x1fffffe8);
        let _limit = StringLimitGuard::set(8);
        let signal = ToolCancellation::default();
        let exact = streamable_text(Full::new(Bytes::from_static(b"12345678")), &signal, false)
            .await
            .unwrap();
        assert_eq!(exact, "12345678");
        let error = streamable_text(Full::new(Bytes::from_static(b"123456789")), &signal, false)
            .await
            .unwrap_err();
        assert_eq!(error.name(), "Error");
        assert_eq!(error.to_string(), fetch_redirect::STRING_TOO_LONG);
        // response.text() on an error POST uses the same bound and the SDK's
        // null fallback, while response.json() propagates the plain V8 Error.
        let error = streamable_error_response(
            hyper::Response::builder()
                .status(400)
                .body(Full::new(Bytes::from_static(b"123456789")))
                .unwrap(),
            &signal,
            false,
        )
        .await;
        assert_eq!(
            error.to_string(),
            "Streamable HTTP error: Error POSTing to endpoint: null"
        );
    }

    #[tokio::test]
    async fn sse_body_uses_global_string_limit() {
        // Literal V8 contract, not the constant against itself (#642).
        assert_eq!(string_limit(), 0x1fffffe8);
        let _limit = StringLimitGuard::set(16);
        let signal = ToolCancellation::default();
        // A pending line is bounded even before its newline arrives.
        assert!(
            read_events(
                Full::new(Bytes::from_static(b"1234567890123456")),
                &signal,
                false,
                |_| panic!("no event")
            )
            .await
        );
        assert!(
            !read_events(
                Full::new(Bytes::from_static(b"12345678901234567")),
                &signal,
                false,
                |_| panic!("no event")
            )
            .await
        );
        // Each frame is below the bound, but data accumulates across frames.
        // Seven emoji plus two inserted newlines are exactly 16 UTF-16 units.
        for overflow in [false, true] {
            let (sender, receiver) = mpsc::unbounded_channel();
            for chunk in ["data: 😀😀😀\n", "data: 😀😀😀\n", "data: 😀\n"] {
                sender.send(Bytes::from(chunk)).unwrap();
            }
            if overflow {
                sender.send(Bytes::from_static(b"data: a\n")).unwrap();
            }
            sender.send(Bytes::from_static(b"\n")).unwrap();
            drop(sender);
            let mut events = Vec::new();
            assert_eq!(
                read_events(EventBody(receiver), &signal, false, |item| events
                    .push(item))
                .await,
                !overflow
            );
            if overflow {
                assert!(events.is_empty());
            } else {
                assert_eq!(
                    events,
                    vec![SseItem::Event {
                        id: None,
                        event: None,
                        data: "😀😀😀\n😀😀😀\n😀".into()
                    }]
                );
            }
        }
    }

    struct StalledBody;
    impl hyper::body::Body for StalledBody {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            std::task::Poll::Pending
        }
    }

    #[tokio::test(start_paused = true)]
    async fn legacy_error_body_stall_uses_global_body_timeout() {
        let signal = ToolCancellation::default();
        let response = hyper::Response::builder()
            .status(400)
            .body(StalledBody)
            .unwrap();
        let mut pending = Box::pin(legacy_post_response(response, &signal, false));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(599_999)).await;
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(1)).await;
        // SDK legacy catches response.text() failure and substitutes null.
        assert_eq!(
            poll_ready(pending.as_mut()).await.unwrap_err().to_string(),
            "Error POSTing to endpoint (HTTP 400): null"
        );
    }

    async fn assert_streamable_body_stall(error_body: bool) {
        let signal = ToolCancellation::default();
        let mut pending = Box::pin(async {
            if error_body {
                streamable_error_response(
                    hyper::Response::builder()
                        .status(400)
                        .body(StalledBody)
                        .unwrap(),
                    &signal,
                    false,
                )
                .await
            } else {
                streamable_text(StalledBody, &signal, false)
                    .await
                    .unwrap_err()
            }
        });
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(599_999)).await;
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(
            poll_ready(pending.as_mut()).await.to_string(),
            if error_body {
                "Streamable HTTP error: Error POSTing to endpoint: null"
            } else {
                "terminated"
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn streamable_error_body_stall_uses_global_body_timeout() {
        assert_streamable_body_stall(true).await;
    }
    #[tokio::test(start_paused = true)]
    async fn streamable_json_body_stall_uses_global_body_timeout() {
        assert_streamable_body_stall(false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn streamable_json_body_orders_timeout_and_parent() {
        for sync in [true, false] {
            let signal = ToolCancellation::default();
            let started = tokio::time::Instant::now();
            let mut pending = Box::pin(streamable_text(StalledBody, &signal, false));
            poll_pending(pending.as_mut()).await;
            tokio::time::advance(Duration::from_millis(600_001)).await;
            if sync {
                signal.cancel_synchronously();
            } else {
                signal.cancel_at(started + Duration::from_millis(600_001));
            }
            // Both errors are projected as terminated, but neither may hang.
            assert_eq!(
                poll_ready(pending.as_mut()).await.unwrap_err().to_string(),
                "terminated"
            );
        }
        let signal = ToolCancellation::default();
        let _limit = StringLimitGuard::set(4);
        let error = streamable_text(Full::new(Bytes::from_static(b"12345")), &signal, false)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), fetch_redirect::STRING_TOO_LONG);
        assert_eq!(
            streamable_text(Full::new(Bytes::from_static(b"1234")), &signal, false)
                .await
                .unwrap(),
            "1234"
        );
    }

    /// The over-limit frame must not end the read: response.text() drains the
    /// body to EOF before the byte→string conversion raises its error.
    #[tokio::test]
    async fn streamable_text_drains_overlong_body_to_eof_before_the_string_error() {
        let signal = ToolCancellation::default();
        let _limit = StringLimitGuard::set(4);
        let (body, eof) = frames_body(vec![Ok(b"12345"), Ok(b"6789")]);
        let error = streamable_text(body, &signal, false).await.unwrap_err();
        assert_eq!(error.to_string(), fetch_redirect::STRING_TOO_LONG);
        assert!(
            eof.load(std::sync::atomic::Ordering::Relaxed),
            "the read ended before the body reached EOF"
        );
    }

    /// A body failure after the cap was crossed still wins, exactly like the
    /// model reader: the drained read reports the transport error, not the
    /// string error (an early-return reader would report the string error
    /// here instead).
    #[tokio::test]
    async fn streamable_text_body_error_beats_string_overflow_after_drain() {
        let signal = ToolCancellation::default();
        let _limit = StringLimitGuard::set(4);
        let (body, _eof) = frames_body(vec![Ok(b"12345"), Err("reset")]);
        let error = streamable_text(body, &signal, false).await.unwrap_err();
        assert_eq!(error.to_string(), "terminated");
    }

    /// The legacy POST's non-2xx `response.text()` uses the same drained
    /// reader; the SDK's catch then substitutes its null body either way, so
    /// only the EOF flag distinguishes the drain.
    #[tokio::test]
    async fn legacy_error_body_over_limit_is_drained_to_eof() {
        let signal = ToolCancellation::default();
        let _limit = StringLimitGuard::set(4);
        let (body, eof) = frames_body(vec![Ok(b"12345"), Ok(b"6789")]);
        let response = hyper::Response::builder().status(400).body(body).unwrap();
        assert_eq!(
            legacy_post_response(response, &signal, false)
                .await
                .unwrap_err()
                .to_string(),
            "Error POSTing to endpoint (HTTP 400): null"
        );
        assert!(
            eof.load(std::sync::atomic::Ordering::Relaxed),
            "the read ended before the body reached EOF"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn sse_stall_uses_global_body_timeout() {
        let signal = ToolCancellation::default();
        let mut pending = Box::pin(read_events(StalledBody, &signal, false, |_| {
            panic!("no data")
        }));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(599_999)).await;
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(!poll_ready(pending.as_mut()).await);
    }

    #[tokio::test(start_paused = true)]
    async fn sse_body_abort_is_immediate_and_live_strings_are_bounded() {
        let signal = ToolCancellation::default();
        let mut pending = Box::pin(read_events(StalledBody, &signal, false, |_| {
            panic!("no data")
        }));
        poll_pending(pending.as_mut()).await;
        signal.cancel_synchronously();
        assert!(!poll_ready(pending.as_mut()).await);
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        assert!(parser.feed_limited("1234", &mut out, 4));
        assert!(!parser.feed_limited("5", &mut out, 4));
        let mut parser = SseParser::default();
        for _ in 0..4 {
            assert!(parser.feed_limited("data:a\n", &mut out, 8));
        }
        assert!(!parser.feed_limited("data:a\n", &mut out, 8));
        let mut parser = SseParser::default();
        for _ in 0..3 {
            assert!(parser.feed_limited("data:😀\n", &mut out, 8));
        }
        assert!(!parser.feed_limited("data:😀\n", &mut out, 8));
        let mut parser = SseParser::default();
        for _ in 0..20 {
            assert!(parser.feed_limited("data:a\n", &mut out, 8));
            assert!(parser.feed_limited("\n", &mut out, 8));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn sse_idle_deadline_resets_after_each_frame() {
        struct Drip {
            timer: std::pin::Pin<Box<tokio::time::Sleep>>,
            remaining: usize,
        }
        impl hyper::body::Body for Drip {
            type Data = Bytes;
            type Error = std::convert::Infallible;
            fn poll_frame(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>>
            {
                if self.remaining == 0 {
                    return std::task::Poll::Pending;
                }
                std::task::ready!(self.timer.as_mut().poll(cx));
                self.remaining -= 1;
                self.timer
                    .as_mut()
                    .reset(tokio::time::Instant::now() + Duration::from_millis(300_000));
                std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from_static(
                    b"data: x\n\n",
                )))))
            }
        }
        let signal = ToolCancellation::default();
        let count = std::cell::Cell::new(0);
        let body = Drip {
            timer: Box::pin(tokio::time::sleep(Duration::from_millis(300_000))),
            remaining: 2,
        };
        let mut pending = Box::pin(read_events(body, &signal, false, |_| {
            count.set(count.get() + 1)
        }));
        poll_pending(pending.as_mut()).await;
        for expected in 1..=2 {
            tokio::time::advance(Duration::from_millis(300_000)).await;
            poll_pending(pending.as_mut()).await;
            assert_eq!(count.get(), expected);
        }
        tokio::time::advance(Duration::from_millis(599_999)).await;
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(!poll_ready(pending.as_mut()).await);
    }

    fn retry_outcome(status_retry: bool) -> Result<hyper::Response<()>, McpClientError> {
        if status_retry {
            Ok(hyper::Response::builder()
                .status(429)
                .header("retry-after", "5")
                .body(())
                .unwrap())
        } else {
            Err(network_error(
                "network_connection_reset",
                "socket hang up".into(),
            ))
        }
    }

    fn assert_retry_aborted(result: Result<hyper::Response<((), bool)>, McpClientError>) {
        let error = result.unwrap_err();
        assert_eq!(error.code(), Some(&McpErrorCode::Text("network_abort")));
        assert_eq!(error.to_string(), "Network retry aborted.");
    }

    /// Both the Retry-After and network-error backoff must wake on close().
    /// A single poll prevents paused-time auto-advance from hiding bare sleep.
    #[tokio::test(start_paused = true)]
    async fn network_fetch_retry_wait_close_aborts_immediately() {
        for status_retry in [true, false] {
            let link = HttpLink::new(
                HttpKind::Streamable,
                Url::parse("http://example.com/").unwrap(),
                Vec::new(),
                100.0,
                100.0,
            );
            let attempts = std::cell::Cell::new(0);
            let started = tokio::time::Instant::now();
            let mut pending = Box::pin(network_fetch_with(
                Method::GET,
                Fetch::Network { timeout_ms: 100.0 },
                &link.aborted,
                || {
                    attempts.set(attempts.get() + 1);
                    ExchangeFuture::ready(retry_outcome(status_retry))
                },
            ));
            poll_pending(pending.as_mut()).await;
            tokio::time::advance(Duration::from_millis(100)).await;
            link.close();
            assert_retry_aborted(poll_ready(pending.as_mut()).await);
            assert_eq!(attempts.get(), 1);
            assert_eq!(started.elapsed(), Duration::from_millis(100));
        }
    }

    /// Real TS delay removes its listener when its already registered timer
    /// fires. A later parent timer (including a tie) cannot reject that wait;
    /// a synchronous close before dispatch still can.
    #[tokio::test(start_paused = true)]
    async fn network_fetch_retry_wait_orders_expiry_and_parent_abort() {
        for (parent_ms, synchronous, succeeds) in [
            (4999, false, false),
            (5000, false, true),
            (5001, false, true),
            (5000, true, false),
        ] {
            let signal = ToolCancellation::default();
            let started = tokio::time::Instant::now();
            let attempts = std::cell::Cell::new(0);
            let mut pending = Box::pin(network_fetch_with(
                Method::GET,
                Fetch::Network { timeout_ms: 100.0 },
                &signal,
                || {
                    attempts.set(attempts.get() + 1);
                    ExchangeFuture::ready(if attempts.get() == 1 {
                        retry_outcome(true)
                    } else {
                        Ok(hyper::Response::new(()))
                    })
                },
            ));
            poll_pending(pending.as_mut()).await;
            tokio::time::advance(Duration::from_millis(parent_ms.max(5000))).await;
            if synchronous {
                signal.cancel_synchronously();
            } else {
                signal.cancel_at(started + Duration::from_millis(parent_ms));
            }
            let result = poll_ready(pending.as_mut()).await;
            if succeeds {
                assert_eq!(result.unwrap().status(), 200);
                assert_eq!(attempts.get(), 2);
            } else {
                assert_retry_aborted(result);
                assert_eq!(attempts.get(), 1);
            }
        }
    }

    /// Cancel after fetch is entered but before delay starts. Unlike a parent
    /// cancelled before the whole request, TS reports "Network retry aborted.".
    #[tokio::test(start_paused = true)]
    async fn network_fetch_retry_wait_rejects_already_cancelled_parent() {
        for status_retry in [true, false] {
            let signal = ToolCancellation::default();
            let started = tokio::time::Instant::now();
            let attempts = std::cell::Cell::new(0);
            let mut pending = Box::pin(network_fetch_with(
                Method::GET,
                Fetch::Network { timeout_ms: 100.0 },
                &signal,
                || {
                    attempts.set(attempts.get() + 1);
                    signal.cancel_synchronously();
                    ExchangeFuture::ready(retry_outcome(status_retry))
                },
            ));
            assert_retry_aborted(poll_ready(pending.as_mut()).await);
            assert_eq!(attempts.get(), 1);
            assert_eq!(started.elapsed(), Duration::ZERO);
        }
    }

    /// Real TS networkFetch probe: response/timeout/parent timers retain their
    /// ordering when the event loop resumes late. Exercise the production MCP
    /// policy, not just the shared dispatcher; no socket scheduling is involved.
    #[tokio::test(start_paused = true)]
    async fn network_fetch_orders_response_timeout_and_later_abort() {
        for (budget, parent_ms, expected) in [
            (100.0, Some(110), None),
            (100.0, Some(60), None),
            (100.0, Some(30), Some("network_abort")),
            (100.0, None, None),
            (60.0, None, Some("network_timeout")),
            (60.9, None, Some("network_timeout")),
            (61.1, None, None),
            (30.0, Some(110), Some("network_timeout")),
        ] {
            let signal = ToolCancellation::default();
            let started = tokio::time::Instant::now();
            let mut pending = Box::pin(network_fetch_with(
                Method::POST,
                Fetch::Network { timeout_ms: budget },
                &signal,
                || ExchangeFuture::after(Duration::from_millis(60), Ok(hyper::Response::new(()))),
            ));
            poll_pending(pending.as_mut()).await;
            tokio::time::advance(Duration::from_millis(120)).await;
            if let Some(parent_ms) = parent_ms {
                // A later timer callback, not abort() in a blocked call stack.
                signal.cancel_at(started + Duration::from_millis(parent_ms));
            }
            match pending.await {
                Ok(response) => {
                    assert_eq!(expected, None, "budget={budget}, parent={parent_ms:?}");
                    assert_eq!(response.status(), 200);
                }
                Err(error) => assert_eq!(error.code(), expected.map(McpErrorCode::Text).as_ref()),
            }
        }
    }

    /// Negative control: close() delivers abort synchronously, so overdue but
    /// undispatched response/timeout callbacks must not replace it.
    #[tokio::test(start_paused = true)]
    async fn network_fetch_close_precedes_undispatched_response() {
        let link = HttpLink::new(
            HttpKind::Streamable,
            Url::parse("http://example.com/").unwrap(),
            Vec::new(),
            100.0,
            100.0,
        );
        let mut pending = Box::pin(network_fetch_with(
            Method::POST,
            Fetch::Network { timeout_ms: 100.0 },
            &link.aborted,
            || ExchangeFuture::after(Duration::from_millis(60), Ok(hyper::Response::new(()))),
        ));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(120)).await;
        link.close();
        assert_eq!(
            pending.await.unwrap_err().code(),
            Some(&McpErrorCode::Text("network_abort"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn network_fetch_queued_response_precedes_overdue_timeout() {
        let signal = ToolCancellation::default();
        let mut pending = Box::pin(network_fetch_with(
            Method::POST,
            Fetch::Network { timeout_ms: 1.0 },
            &signal,
            || {
                ExchangeFuture::microtask(async {
                    tokio::task::yield_now().await;
                    Ok(hyper::Response::new(()))
                })
            },
        ));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(120)).await;
        assert_eq!(pending.await.unwrap().status(), 200);
    }

    /// TS resolve(response); controller.abort() still fulfills networkFetch.
    /// The promise has settled; only its queued continuation has not run.
    #[tokio::test(start_paused = true)]
    async fn network_fetch_settled_response_survives_later_close() {
        let link = HttpLink::new(
            HttpKind::Streamable,
            Url::parse("http://example.com/").unwrap(),
            Vec::new(),
            100.0,
            100.0,
        );
        let mut pending = Box::pin(network_fetch_with(
            Method::POST,
            Fetch::Network { timeout_ms: 100.0 },
            &link.aborted,
            || {
                ExchangeFuture::microtask(async {
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    Ok(hyper::Response::new(()))
                })
            },
        ));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(120)).await;
        link.close();
        poll_pending(pending.as_mut()).await;
        assert_eq!(pending.await.unwrap().status(), 200);

        // Negative control: an abort delivered before fetch is started must
        // prevent even a synchronously fulfilled response from being used.
        let error = network_fetch_with(
            Method::POST,
            Fetch::Network { timeout_ms: 100.0 },
            &link.aborted,
            || ExchangeFuture::ready(Ok(hyper::Response::new(()))),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), Some(&McpErrorCode::Text("network_abort")));
    }

    #[test]
    fn retry_delay_accepts_http_date_and_caps_like_ts() {
        assert_eq!(
            retry_delay(Some("Wed, 21 Oct 2099 07:28:00 GMT")),
            Duration::from_millis(5000)
        );
        assert_eq!(retry_delay(Some("6")), Duration::from_millis(5000));
        assert_eq!(retry_delay(Some("0.0609")), Duration::from_millis(61));
        assert_eq!(retry_delay(Some("0")), Duration::from_millis(1));
        // Expired/malformed/empty headers must still use jittered backoff.
        for header in [
            None,
            Some(""),
            Some("bad"),
            Some("Wed, 21 Oct 2015 07:28:00 GMT"),
        ] {
            assert!(
                (Duration::from_millis(500)..Duration::from_millis(625))
                    .contains(&retry_delay(header))
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn network_fetch_retries_http_date_after_capped_delay() {
        let signal = ToolCancellation::default();
        let mut attempts = 0;
        let mut pending = Box::pin(network_fetch_with(
            Method::GET,
            Fetch::Network { timeout_ms: 100.0 },
            &signal,
            || {
                attempts += 1;
                ExchangeFuture::ready(Ok(if attempts == 1 {
                    hyper::Response::builder()
                        .status(429)
                        .header("retry-after", "Wed, 21 Oct 2099 07:28:00 GMT")
                        .body(())
                        .unwrap()
                } else {
                    hyper::Response::new(())
                }))
            },
        ));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(4999)).await;
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(pending.await.unwrap().status(), 200);
        assert_eq!(attempts, 2);
    }
    use std::{
        convert::Infallible,
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::sync::mpsc;

    /// A stream that remains open while its sender exists; no clock or socket
    /// timing is involved. Dropping the body is observable at the sender.
    struct EventBody(mpsc::UnboundedReceiver<Bytes>);
    impl hyper::body::Body for EventBody {
        type Data = Bytes;
        type Error = Infallible;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Infallible>>> {
            self.0
                .poll_recv(cx)
                .map(|data| data.map(|data| Ok(hyper::body::Frame::data(data))))
        }
    }

    /// Scripted frames; an `Err` entry simulates a mid-read body failure and
    /// `eof` records that the reader polled the stream to its end, so the
    /// cap tests can assert the drain instead of timing the frames.
    struct FramesBody {
        frames: std::collections::VecDeque<Result<Bytes, String>>,
        eof: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl hyper::body::Body for FramesBody {
        type Data = Bytes;
        type Error = String;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, String>>> {
            match self.frames.pop_front() {
                None => {
                    self.eof.store(true, std::sync::atomic::Ordering::Relaxed);
                    Poll::Ready(None)
                }
                Some(Ok(data)) => Poll::Ready(Some(Ok(hyper::body::Frame::data(data)))),
                Some(Err(error)) => Poll::Ready(Some(Err(error))),
            }
        }
    }
    fn frames_body(
        frames: Vec<Result<&'static [u8], &'static str>>,
    ) -> (FramesBody, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let eof = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        (
            FramesBody {
                frames: frames
                    .into_iter()
                    .map(|frame| frame.map(Bytes::from_static).map_err(str::to_owned))
                    .collect(),
                eof: eof.clone(),
            },
            eof,
        )
    }

    /// Real SDK + networkFetch records the two requests. A body with no EOF
    /// proves redirect following never waits for response.text().
    #[tokio::test]
    async fn fetch_follows_redirect_without_waiting_for_its_body() {
        for status in [301, 302, 303, 307, 308] {
            let url = Url::parse("http://mcp.invalid/redirect").unwrap();
            let (keep_open, body) = mpsc::unbounded_channel();
            keep_open.send(Bytes::from_static(b"partial")).unwrap();
            let responses = Mutex::new(std::collections::VecDeque::from([
                hyper::Response::builder()
                    .status(status)
                    .header("location", "/mcp")
                    .body(EventBody(body))
                    .unwrap(),
                hyper::Response::new(EventBody(mpsc::unbounded_channel().1)),
            ]));
            let requests = Mutex::new(Vec::new());
            let mut headers = HeaderList::default();
            headers.set("content-type", "application/json");
            let message = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
            let response = tokio::time::timeout(
                Duration::from_secs(5),
                fetch_with(
                    Method::POST,
                    &url,
                    &headers,
                    Some(message.into()),
                    |request| async {
                        let (parts, body) = request.into_parts();
                        let body = body.collect().await.unwrap().to_bytes();
                        lock(&requests).push((parts, body));
                        Ok(lock(&responses).pop_front().expect("at most two exchanges"))
                    },
                ),
            )
            .await
            .expect("redirect body must not block")
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(keep_open.is_closed(), "redirect body must be dropped");
            let requests = requests.into_inner().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].0.method, Method::POST);
            assert_eq!(requests[0].0.uri.path(), "/redirect");
            assert_eq!(requests[0].1, message);
            let (second, body) = &requests[1];
            assert_eq!(second.uri.path(), "/mcp");
            if matches!(status, 307 | 308) {
                assert_eq!(second.method, Method::POST);
                assert_eq!(second.headers["content-type"], "application/json");
                assert_eq!(body, message);
            } else {
                assert_eq!(second.method, Method::GET);
                assert!(!second.headers.contains_key("content-type"));
                assert!(body.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn invalid_legacy_endpoint_closes_an_open_stream_before_and_after_ready() {
        for after_ready in [false, true] {
            for endpoint in ["http://[", "http://other.invalid/messages"] {
                let url = Url::parse("http://mcp.invalid/sse").unwrap();
                let wire = Wire::http(HttpLink::new(
                    HttpKind::LegacySse,
                    url.clone(),
                    vec![],
                    0.0,
                    0.0,
                ));
                let (ready, outcome) = oneshot::channel();
                let (events, body) = mpsc::unbounded_channel();
                let weak = Arc::downgrade(&wire);
                let aborted = link_of(&wire).aborted.clone();
                let task = tokio::spawn(async move {
                    legacy_stream(
                        &weak,
                        EventBody(body),
                        &aborted,
                        &url,
                        &mut Some(ready),
                        false,
                    )
                    .await
                });
                if after_ready {
                    events
                        .send(Bytes::from_static(b"event: endpoint\ndata: /messages\n\n"))
                        .unwrap();
                    outcome.await.unwrap().unwrap();
                    events.send(Bytes::from(format!(
                        "event: endpoint\ndata: {endpoint}\n\nevent: endpoint\ndata: /must-not-install\n\n"
                    ))).unwrap();
                } else {
                    events.send(Bytes::from(format!(
                        "event: endpoint\ndata: {endpoint}\n\nevent: endpoint\ndata: /must-not-install\n\n"
                    ))).unwrap();
                    let error = outcome.await.unwrap().unwrap_err();
                    assert_eq!(
                        error.message(),
                        if endpoint == "http://[" {
                            "Invalid URL"
                        } else {
                            "Endpoint origin does not match connection origin: http://other.invalid"
                        }
                    );
                }
                let finished = tokio::time::timeout(Duration::from_secs(5), task).await;
                if finished.is_err() {
                    wire.close().await;
                }
                finished.expect("invalid endpoint waited for EOF").unwrap();
                assert!(
                    events.is_closed(),
                    "EventSource body must be dropped without EOF"
                );
                assert!(link_of(&wire).is_aborted());
                let endpoint = lock(&link_of(&wire).state).endpoint.clone();
                assert_eq!(
                    endpoint.as_ref().map(Url::path),
                    after_ready.then_some("/messages")
                );
                assert_eq!(
                    wire.notification("ping").await.unwrap_err().message(),
                    "Not connected"
                );
            }
        }
    }

    /// An observable Body poll, rather than a delay, establishes that
    /// response.text() is waiting when close arrives.
    #[tokio::test]
    async fn legacy_post_error_body_is_dropped_on_close() {
        use std::{
            convert::Infallible,
            pin::Pin,
            task::{Context, Poll},
        };
        struct PendingBody {
            polled: Option<oneshot::Sender<()>>,
            dropped: Option<oneshot::Sender<()>>,
        }
        impl hyper::body::Body for PendingBody {
            type Data = Bytes;
            type Error = Infallible;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Infallible>>> {
                if let Some(polled) = self.polled.take() {
                    polled.send(()).unwrap();
                    return Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from_static(
                        b"partial",
                    )))));
                }
                Poll::Pending
            }
        }
        impl Drop for PendingBody {
            fn drop(&mut self) {
                if let Some(dropped) = self.dropped.take() {
                    let _ = dropped.send(());
                }
            }
        }
        let link = HttpLink::new(
            HttpKind::LegacySse,
            Url::parse("http://localhost/mcp").unwrap(),
            vec![],
            0.0,
            0.0,
        );
        let (polled, waiting) = oneshot::channel();
        let (dropped, freed) = oneshot::channel();
        let response = hyper::Response::builder()
            .status(500)
            .body(PendingBody {
                polled: Some(polled),
                dropped: Some(dropped),
            })
            .unwrap();
        let mut read = Box::pin(legacy_post_response(response, &link.aborted, false));
        tokio::select! {
            biased;
            result = &mut read => panic!("unfinished body returned {result:?}"),
            result = waiting => result.unwrap(),
        }
        link.close();
        let result = tokio::time::timeout(Duration::from_secs(5), &mut read).await;
        drop(read);
        assert_eq!(
            result
                .expect("close must cancel response.text()")
                .unwrap_err()
                .message(),
            "Error POSTing to endpoint (HTTP 500): null"
        );
        freed.await.expect("body must be dropped");
    }

    fn events(chunks: &[&str]) -> Vec<SseItem> {
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        for chunk in chunks {
            parser.feed(chunk, &mut out);
        }
        out
    }

    fn event(id: Option<&str>, event: Option<&str>, data: &str) -> SseItem {
        SseItem::Event {
            id: id.map(Into::into),
            event: event.map(Into::into),
            data: data.into(),
        }
    }

    #[test]
    fn sse_parser_follows_eventsource_parser() {
        assert_eq!(
            events(&[
                "data: a\n",
                "data:b\n\n",
                "id: 7\nevent: x\ndata: c\r\n\r\n"
            ]),
            [event(None, None, "a\nb"), event(Some("7"), Some("x"), "c")]
        );
        // CR split from its LF across chunks, comments, retry, no-data events.
        assert_eq!(
            events(&["retry: 50\r", "\n: hi\rid: p\r\n\r\ndata: z\r", "\n\r\n"]),
            [SseItem::Retry(50.0), event(None, None, "z")]
        );
        // `id` with NUL is ignored; empty `event:` means the default type.
        assert_eq!(
            events(&["id: a\0b\nevent:\ndata\n\n"]),
            [event(None, None, "")]
        );
        assert_eq!(events(&["retry: x\nfoo: bar\ndata: q"]), []);
    }

    #[test]
    fn utf8_stream_carries_split_characters_and_drops_bom() {
        let mut stream = Utf8Stream::default();
        let bytes = "\u{FEFF}é😀".as_bytes();
        let mut out = String::new();
        for byte in bytes {
            out.push_str(&stream.push(std::slice::from_ref(byte)));
        }
        assert_eq!(out, "é😀");
        assert_eq!(Utf8Stream::default().push(b"a\xffb"), "a\u{FFFD}b");
    }

    #[test]
    fn common_headers_merge_like_a_headers_record() {
        let link = HttpLink::new(
            HttpKind::Streamable,
            Url::parse("http://127.0.0.1:1/").unwrap(),
            vec![
                ("X-A".into(), "1".into()),
                ("x-a".into(), "2".into()),
                ("mcp-session-id".into(), "user".into()),
            ],
            1.0,
            1.0,
        );
        lock(&link.state).session_id = Some("server".into());
        let headers = link.common_headers();
        assert_eq!(
            headers.0,
            [
                ("mcp-session-id".to_owned(), "user".to_owned()),
                ("x-a".to_owned(), "1, 2".to_owned()),
            ]
        );
    }

    #[test]
    fn proxy_authorization_is_refused_only_on_a_proxied_dispatch() {
        let config = |no_proxy: &str| {
            crate::runtime::model::http::ProxyConfig::from_lookup(|key| match key {
                "HTTP_PROXY" => Some("http://proxy:3128".to_owned()),
                "no_proxy" => (!no_proxy.is_empty()).then(|| no_proxy.to_owned()),
                _ => None,
            })
            .expect("proxy config")
        };
        let request = |uri: &str, header: Option<&str>| {
            let mut builder = Request::builder().uri(uri);
            if let Some(header) = header {
                builder = builder.header(header, "Basic abc");
            }
            builder.body(Full::new(Bytes::new())).unwrap()
        };
        let proxy = config("");
        // Case-insensitive, like `isProxyAuthorizationHeader`.
        for header in ["proxy-authorization", "Proxy-Authorization"] {
            let error = reject_proxy_authorization(
                Some(&proxy),
                &request("http://example.com/mcp", Some(header)),
            )
            .expect_err("proxied request must be refused");
            assert_eq!(
                error.message(),
                "fetch failed\u{0}Proxy-Authorization should be sent in ProxyAgent constructor"
            );
        }
        assert!(
            reject_proxy_authorization(
                Some(&proxy),
                &request("http://example.com/mcp", Some("authorization")),
            )
            .is_ok()
        );
        assert!(
            reject_proxy_authorization(Some(&proxy), &request("http://example.com/mcp", None))
                .is_ok()
        );
        // `NO_PROXY` selects undici's direct agent, which has no such check.
        let direct = config("example.com");
        assert!(
            reject_proxy_authorization(
                Some(&direct),
                &request("http://example.com/mcp", Some("proxy-authorization")),
            )
            .is_ok()
        );
        assert!(
            reject_proxy_authorization(
                None,
                &request("http://example.com/mcp", Some("proxy-authorization"))
            )
            .is_ok()
        );
    }

    #[test]
    fn failure_addresses_use_the_scheme_default_and_node_form_for_ipv6() {
        let uri = |raw: &str| -> hyper::Uri { raw.parse().unwrap() };
        assert_eq!(
            target_address(&uri("https://127.0.0.1/sse")),
            "127.0.0.1:443"
        );
        assert_eq!(target_address(&uri("http://127.0.0.1/sse")), "127.0.0.1:80");
        // Node prints the bare address (no IPv6 brackets) in both the
        // ECONNREFUSED and the connect-timeout texts.
        assert_eq!(target_address(&uri("https://[::1]/sse")), "::1:443");
        assert_eq!(target_address(&uri("http://[::1]:8080/sse")), "::1:8080");
    }

    fn sock(host: &str, port: u16) -> SocketAddr {
        SocketAddr::new(host.parse().unwrap(), port)
    }

    /// TS 金样（#681，2026-10-25 实测，证据
    /// `_tmp/20261025-issue-681/ts-probe.out`）：legacy SSE 连
    /// `http://localhost:<closed>/sse` 的拒连文案是
    /// `SSE error: TypeError: fetch failed: connect ECONNREFUSED ::1:60169,
    /// connect ECONNREFUSED 127.0.0.1:60169` — 解析出的每个地址一条，按
    /// Node 的尝试顺序逗号拼接，而不是 URL 主机名。
    #[test]
    fn hostname_refusals_list_every_resolved_address() {
        let uri = |raw: &str| -> hyper::Uri { raw.parse().unwrap() };
        let localhost = [sock("::1", 60169), sock("127.0.0.1", 60169)];
        assert_eq!(
            refused_cause(&uri("http://localhost:60169/sse"), Some(&localhost)),
            "connect ECONNREFUSED ::1:60169, connect ECONNREFUSED 127.0.0.1:60169"
        );
        // 单地址主机名：拒连同样显示解析地址，而非主机名。
        let single = [sock("127.0.0.1", 60169)];
        assert_eq!(
            refused_cause(&uri("http://localhost:60169/sse"), Some(&single)),
            "connect ECONNREFUSED 127.0.0.1:60169"
        );
        // IP 字面量（无解析）：`https-sse-refused` / `https-sse-refused-ipv6`
        // 金样钉住的形状不变。
        assert_eq!(
            refused_cause(&uri("https://127.0.0.1/sse"), None),
            "connect ECONNREFUSED 127.0.0.1:443"
        );
        assert_eq!(
            refused_cause(&uri("https://[::1]/sse"), None),
            "connect ECONNREFUSED ::1:443"
        );
    }

    /// Node 的尝试顺序（`lookupAndConnectMultiple`，node v22.22.0
    /// `lib/net.js:1470-1541`）：去重，首地址的地址族优先，两族交错。
    #[test]
    fn node_attempt_order_dedupes_and_interleaves_families() {
        assert_eq!(
            node_attempt_order(&[
                sock("192.0.2.1", 443),
                sock("192.0.2.1", 443),
                sock("2001:db8::1", 443)
            ]),
            vec![sock("192.0.2.1", 443), sock("2001:db8::1", 443)]
        );
        assert_eq!(
            node_attempt_order(&[
                sock("192.0.2.1", 443),
                sock("192.0.2.2", 443),
                sock("2001:db8::1", 443)
            ]),
            vec![
                sock("192.0.2.1", 443),
                sock("2001:db8::1", 443),
                sock("192.0.2.2", 443)
            ]
        );
        assert_eq!(
            node_attempt_order(&[sock("::1", 443), sock("127.0.0.1", 443)]),
            vec![sock("::1", 443), sock("127.0.0.1", 443)]
        );
    }

    /// TS 金样（#681，同上证据）：legacy SSE 连
    /// `https://localhost:<stall>/sse`（TCP 通、TLS 停滞）10 s 后是
    /// `Connect Timeout Error (attempted addresses: ::1:60217,
    /// 127.0.0.1:60217, timeout: 10000ms)` — 复数、解析地址列表。
    /// 单地址主机名（含重复解析去重后只剩一个）的超时显示 URL 主机名：
    /// Node 在去重后才判单复数（`toAttempt.length === 1`，node v22.22.0
    /// `lib/net.js:1518`），单地址走普通单连接路径、不记录解析，undici 的
    /// `onConnectTimeout` 回退到连接器收到的 URL 主机名。#681 fix-r1 实测
    /// （`_tmp/20261025-issue-681/fix-r1/ts-probe-dup.out`，把 `dns.lookup`
    /// 注入为返回重复 / 单条 127.0.0.1 记录，下游 net.js 与 undici 均为真实
    /// 代码）。
    #[test]
    fn hostname_connect_timeouts_list_attempted_addresses() {
        let uri = |raw: &str| -> hyper::Uri { raw.parse().unwrap() };
        let localhost = [sock("::1", 60217), sock("127.0.0.1", 60217)];
        assert_eq!(
            timeout_cause(&uri("https://localhost:60217/sse"), Some(&localhost)),
            "Connect Timeout Error (attempted addresses: ::1:60217, 127.0.0.1:60217, timeout: 10000ms)"
        );
        let single = [sock("192.0.2.1", 8443)];
        assert_eq!(
            timeout_cause(&uri("https://example.com:8443/sse"), Some(&single)),
            "Connect Timeout Error (attempted address: example.com:8443, timeout: 10000ms)"
        );
        // 重复解析（去重前两条、去重后一条）与单条同形：主机名单数。
        let duplicated = [sock("127.0.0.1", 64720), sock("127.0.0.1", 64720)];
        assert_eq!(
            timeout_cause(
                &uri("https://dup-probe.invalid:64720/sse"),
                Some(&duplicated)
            ),
            "Connect Timeout Error (attempted address: dup-probe.invalid:64720, timeout: 10000ms)"
        );
        // IP 字面量：`https-sse-handshake-stall` 金样钉住的形状不变。
        assert_eq!(
            timeout_cause(&uri("https://127.0.0.1:9443/sse"), None),
            "Connect Timeout Error (attempted address: 127.0.0.1:9443, timeout: 10000ms)"
        );
    }

    /// #681 fix-r1 实测（同上证据）：解析出重复地址的拒连只报一条去重后的
    /// `connect ECONNREFUSED 127.0.0.1:64718`（Node 先按 IP 去重再连接）。
    #[test]
    fn duplicate_resolutions_dedup_in_refusal_texts() {
        let uri = |raw: &str| -> hyper::Uri { raw.parse().unwrap() };
        let duplicated = [sock("127.0.0.1", 64718), sock("127.0.0.1", 64718)];
        assert_eq!(
            refused_cause(&uri("http://dup-probe.invalid:64718/sse"), Some(&duplicated)),
            "connect ECONNREFUSED 127.0.0.1:64718"
        );
    }

    /// 失败路径的重解析有界：解析器停滞（正是导致连接失败的那种停滞）时，
    /// 不得在报错前再等一轮系统解析超时，到 [`RESOLUTION_DEADLINE`] 即回退
    /// `None`（文案回退到 URL 主机名形状）。IP 字面量则根本不查。
    #[tokio::test(start_paused = true)]
    async fn stalled_resolution_falls_back_within_the_deadline() {
        let uri = |raw: &str| -> hyper::Uri { raw.parse().unwrap() };
        let stalled = |_host: String, _port: u16| async {
            std::future::pending::<std::io::Result<Vec<SocketAddr>>>().await
        };
        let resolved = tokio::time::timeout(
            // 停滞的解析器若没有被界住，外层这层哨钟就是唯一会到期的计时器，
            // 测试随之失败（负控：删掉内部 `tokio::time::timeout` 即触发）。
            RESOLUTION_DEADLINE + Duration::from_millis(50),
            resolved_attempts_with(&uri("http://slow-dns.example:8080/sse"), stalled),
        )
        .await
        .expect("the stalled lookup must be cut off by RESOLUTION_DEADLINE");
        assert_eq!(resolved, None);
        // 解析报错同样回退 None（Node 对查找失败报 ENOTFOUND，不进地址文案）。
        let erroring = |_host: String, _port: u16| async {
            Err::<Vec<SocketAddr>, _>(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "dns stalled probe",
            ))
        };
        assert_eq!(
            resolved_attempts_with(&uri("http://no-dns.example:8080/sse"), erroring).await,
            None
        );
        // 正常解析照常透传。
        let answering = |_host: String, port: u16| async move {
            Ok::<Vec<SocketAddr>, std::io::Error>(vec![sock("127.0.0.1", port)])
        };
        assert_eq!(
            resolved_attempts_with(&uri("http://answers.example:8080/sse"), answering).await,
            Some(vec![sock("127.0.0.1", 8080)])
        );
        // IP 字面量不查：解析器被调用即 panic。
        assert_eq!(
            resolved_attempts_with(&uri("https://[::1]:9443/sse"), |host, port| async move {
                panic!("ip literals are never resolved: {host}:{port}")
            })
            .await,
            None
        );
    }

    #[test]
    fn http_and_https_urls_are_served() {
        assert!(parse_endpoint("http://127.0.0.1:1/mcp", "s").is_ok());
        assert!(parse_endpoint("https://example.com/mcp", "s").is_ok());
        let other = parse_endpoint("ftp://example.com/mcp", "s").unwrap_err();
        assert_eq!(
            other.client_code().map(|c| c.as_str()),
            Some("mcp_unsupported_transport")
        );
        assert_eq!(
            parse_endpoint("not a url", "s").unwrap_err().message(),
            "Invalid URL"
        );
    }
}

#[cfg(test)]
#[tokio::test(start_paused = true)]
async fn final_network_error_rechecks_parent_like_real_ts() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/network-error-order/oracle.json"
    ));
    let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
        panic!("network cases")
    };
    for case in cases {
        let mode = case
            .get("mode")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        let signal = ToolCancellation::new();
        let calls = std::cell::Cell::new(0);
        let start = tokio::time::Instant::now();
        let mut pending = Box::pin(network_fetch_with(
            if mode.starts_with("last-get") {
                Method::GET
            } else {
                Method::POST
            },
            Fetch::Network { timeout_ms: 0.0 },
            &signal,
            || {
                calls.set(calls.get() + 1);
                let error = if mode == "normalized-sync-abort" {
                    network_error("network_tls_error", "already normalized".into())
                } else {
                    McpClientError::new(
                        "TypeError",
                        Some(McpErrorCode::Errno("ECONNRESET")),
                        "socket hang up",
                    )
                };
                if mode.contains("before-") {
                    return ExchangeFuture::<Result<hyper::Response<()>, McpClientError>>::after(
                        Duration::from_millis(10),
                        Err(error),
                    );
                }
                if mode == "ready-rejection-sync-abort"
                    || mode == "normalized-sync-abort"
                    || (mode.starts_with("last-get") && calls.get() == 2)
                {
                    signal.cancel_synchronously();
                }
                ExchangeFuture::ready(Err(error))
            },
        ));
        if mode.contains("before-") {
            std::future::poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            tokio::time::advance(Duration::from_millis(40)).await;
            signal.cancel_at(
                start
                    + Duration::from_millis(if mode == "timer-error-before-parent" {
                        20
                    } else {
                        5
                    }),
            );
        }
        let error = pending.await.unwrap_err();
        let expected = case.get("expected").unwrap();
        let string = |key| {
            expected
                .get(key)
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap()
        };
        assert_eq!(error.name(), string("name"), "{mode}");
        let Some(McpErrorCode::Text(code)) = error.code() else {
            panic!("expected network error code")
        };
        assert_eq!(*code, string("code"), "{mode}");
        assert_eq!(error.message(), string("message"), "{mode}");
        assert_eq!(
            calls.get() as f64,
            case.get("calls").unwrap().as_number().unwrap(),
            "{mode}"
        );
    }
    // Legacy/SSE bare fetch never gets Network mode's new normalization/recheck.
    let signal = ToolCancellation::new();
    let error = network_fetch_with::<()>(Method::POST, Fetch::Plain, &signal, || {
        signal.cancel_synchronously();
        ExchangeFuture::ready(Err(McpClientError::plain("TypeError", "socket hang up")))
    })
    .await
    .unwrap_err();
    assert_eq!(error.name(), "TypeError");
    assert_eq!(error.message(), "socket hang up");
}
