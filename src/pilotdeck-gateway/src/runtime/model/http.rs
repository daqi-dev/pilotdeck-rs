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
//! audit staging (`options.invocation`, A6.4-pr10) and `PILOTDECK_DUMP_REQUEST`.
//! The outgoing HTTP proxy (`cli/proxy.ts`) is implemented here and wired into
//! MCP's `streamable_http` / `sse` transports only; model and hook call sites
//! still use [`https_connector`] and therefore stay direct. MCP clients pass
//! the `pilotdeck.yaml` `proxy` source through [`ProxySettings`]; `None`
//! resolves the environment exactly as before.
#![allow(clippy::result_large_err)]

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{Request, Response};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::{TokioExecutor, TokioIo};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

use super::count_body::{array, field, is, object, put, text as header_text};
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
    Request {
        code: JsString,
        message: JsString,
        details: Option<Box<JsValue>>,
    },
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
            ModelStreamError::Request { code, message, .. } => Self {
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

/// Shared event source with the provider transport's response type.
pub type TransportFuture = crate::runtime::network::ExchangeFuture<TransportResult>;

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

/// One deadline around a connector's whole connect future, DNS and TLS
/// handshake included. undici's `connectTimeout` (10 s by default) spans
/// socket establishment *including* the TLS handshake, while hyper's
/// `HttpConnector::set_connect_timeout` only bounds the TCP phase
/// (`node_modules/undici/lib/core/connect.js:69,126`). Call sites that need
/// undici's semantics wrap the shared connector with this without changing
/// [`https_connector`] itself. hyper-util's `Connect` is blanket-implemented
/// for `tower_service::Service<Uri>`, so this wrapper is the seam.
#[derive(Clone)]
pub(crate) struct ConnectDeadline<C> {
    inner: C,
    timeout: Duration,
}
impl<C> ConnectDeadline<C> {
    pub(crate) fn new(inner: C, timeout: Duration) -> Self {
        Self { inner, timeout }
    }
}
impl<C> tower_service::Service<hyper::Uri> for ConnectDeadline<C>
where
    C: tower_service::Service<hyper::Uri> + Send + Sync + 'static,
    C::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    C::Future: Send + 'static,
{
    type Response = C::Response;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    fn poll_ready(
        &mut self,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context).map_err(Into::into)
    }
    fn call(&mut self, destination: hyper::Uri) -> Self::Future {
        let timeout = self.timeout;
        let connect = self.inner.call(destination);
        Box::pin(async move {
            match tokio::time::timeout(timeout, connect).await {
                Ok(result) => result.map_err(Into::into),
                // TS reports this as `TypeError: fetch failed` (cause
                // `UND_ERR_CONNECT_TIMEOUT`, probe measured ~10.5 s); callers
                // map the message the same way they map any fetch failure.
                Err(_) => Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connect timeout",
                ))
                    as Box<dyn std::error::Error + Send + Sync>),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Outgoing HTTP proxy (TS `src/cli/proxy.ts`)
//
// The baseline installs an undici `EnvHttpProxyAgent` as the process-wide
// dispatcher at CLI startup, so every `fetch` (model, hook, MCP) uses it. This
// port keeps the agent semantics but exposes them as an opt-in connector: MCP
// `streamable_http` / `sse` use [`proxy_https_connector`], model and hook
// transports keep calling [`https_connector`] and stay direct until a separate
// decision wires them.
//
// Sources and priority copied from `src/cli/proxy.ts:20-31` and
// `pilotdeck.ts:28,48-51,150`:
// - URL: `PILOTDECK_PROXY > https_proxy > HTTPS_PROXY > http_proxy >
//   HTTP_PROXY`; at startup an env URL wins over `proxy.url`, on hot reload
//   `proxy.url` wins. Empty strings are falsy.
// - noProxy: `no_proxy || NO_PROXY`; the config `noProxy` is appended only on
//   hot reload (`pilotdeck.ts:150`), not by startup's
//   `installGlobalProxy(config.url)`. `buildNoProxy` (`proxy.ts:159-166`) always appends
//   `127.0.0.1` and `localhost`, so loopback MCP endpoints never use a proxy.
//   Entries split on commas or whitespace, an optional `*.`/`.` prefix is
//   stripped, and an entry matches its exact host or a `.<entry>` subdomain,
//   optionally pinned to a port
//   (`node_modules/undici/lib/dispatcher/env-http-proxy-agent.js:82-140`).
//
// Only `http:` proxies are tunnelled (the required CONNECT path); `https:` and
// `socks5:`/`socks:` proxy URLs are supported by undici 8.3 but not by this
// connector and fall back to a direct connection, like an unparsable URL does
// in `installGlobalProxy`. See RESULT for the six-field candidate.
//
// Every proxied request, including plain `http:` targets, is sent through
// `CONNECT` because undici's ProxyAgent defaults to `proxyTunnel: true`.
// Proxy credentials come from the URL userinfo and become
// `Proxy-Authorization: Basic base64(decodeURIComponent(user):decodeURIComponent(password))`.

/// A rejected CONNECT, shaped like the undici cause chain the TS transport
/// flattens (`fetch failed: <cause>` in the legacy SSE rejection).
#[derive(Debug)]
pub(crate) struct ProxyError {
    pub(crate) cause: String,
}
impl ProxyError {
    fn new(cause: String) -> Self {
        Self { cause }
    }
}
impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.cause)
    }
}
impl std::error::Error for ProxyError {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NoProxyEntry {
    hostname: String,
    /// `Number.parseInt(port, 10)` (`env-http-proxy-agent.js:126`): `None` is
    /// JS `0`, which is falsy and therefore matches every port. `Some` must
    /// equal the target port. A numeric overflow stays nonzero, so (like JS's
    /// unbounded Number) it can never equal a 16-bit port.
    port: Option<u64>,
}

/// Proxy sources from `pilotdeck.yaml` (`proxy` in `src/pilot/cli/config`).
///
/// Mirrors `parseProxyConfig` (`src/pilot/config/loadPilotConfig.ts:856-911`):
/// a bare string is a URL, an object needs a non-empty `url`, and `noProxy`
/// is kept only when non-empty. `mode` selects the TS install path:
/// [`ProxyInstallMode::Startup`] is `installGlobalProxy` from `pilotdeck.ts:48`
/// (env URL wins, config is the fallback) and [`ProxyInstallMode::Reload`] is
/// `reinstallGlobalProxy` from `pilotdeck.ts:150` (config URL wins and its
/// `noProxy` is appended to `buildNoProxy`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxySettings {
    pub url: String,
    pub no_proxy: Option<String>,
    pub mode: ProxyInstallMode,
}

/// Which TS install path the config source came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProxyInstallMode {
    /// `installGlobalProxy(config.url)` during `server` startup.
    #[default]
    Startup,
    /// `reinstallGlobalProxy(config.url, config.noProxy)` on config change.
    Reload,
}

impl ProxySettings {
    /// Parse a `proxy` config value (string or `{url, noProxy}`). Empty URLs
    /// are absent, exactly like `parseProxyConfig` and the `if
    /// (snapshot.config.proxy?.url)` guard in `pilotdeck.ts:48`.
    pub fn from_config(value: &JsValue) -> Option<Self> {
        if let Some(url) = value.as_string() {
            let url = url.to_utf8().ok()?;
            return (!url.is_empty()).then(|| Self {
                url,
                ..Self::default()
            });
        }
        let object = value.as_object()?;
        let url = object.get("url")?.as_string()?.to_utf8().ok()?;
        if url.is_empty() {
            return None;
        }
        let no_proxy = object
            .get("noProxy")
            .and_then(|value| value.as_string())
            .and_then(|value| value.to_utf8().ok())
            .filter(|value| !value.is_empty());
        Some(Self {
            url,
            no_proxy,
            mode: ProxyInstallMode::Startup,
        })
    }
}

/// `||` truthiness for a config/env string: `""` is absent.
fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}
fn non_empty_opt(value: Option<&str>) -> Option<String> {
    value.and_then(non_empty)
}

/// The active proxy (undici `EnvHttpProxyAgent` + `buildNoProxy`).
#[derive(Clone, Debug)]
pub(crate) struct ProxyConfig {
    proxy: url::Url,
    /// The joined value the baseline logs and matches `"*"` against.
    no_proxy: String,
    entries: Vec<NoProxyEntry>,
    /// The CONNECT `Proxy-Authorization` value, built at construction like
    /// `ProxyAgent`'s constructor. `decodeURIComponent` throws on malformed
    /// userinfo, which makes the TS install fail; here that is `None` from
    /// the constructor instead.
    authorization: Option<String>,
}
impl ProxyConfig {
    /// Testable seam over the same lookup order, so env precedence is pinned
    /// without mutating the process environment.
    #[cfg(test)]
    pub(crate) fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        Self::from_settings_lookup(None, lookup)
    }

    /// Resolve the config source against the env source. `None` or
    /// [`ProxyInstallMode::Startup`] keeps the env URL ahead of config and
    /// ignores `noProxy`; [`ProxyInstallMode::Reload`] is the TS hot-reload
    /// path where the config URL and `noProxy` replace the env ones.
    pub(crate) fn from_settings(settings: Option<&ProxySettings>) -> Option<Self> {
        Self::from_settings_lookup(settings, |key| std::env::var(key).ok())
    }

    fn from_settings_lookup(
        settings: Option<&ProxySettings>,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Option<Self> {
        let env_url = [
            "PILOTDECK_PROXY",
            "https_proxy",
            "HTTPS_PROXY",
            "http_proxy",
            "HTTP_PROXY",
        ]
        .into_iter()
        .find_map(|key| lookup(key).filter(|value| !value.is_empty()));
        let (proxy_url, extra_no_proxy) = match settings {
            None => (env_url?, None),
            Some(settings) if settings.mode == ProxyInstallMode::Reload => {
                // `reinstallGlobalProxy`: config URL is authoritative and its
                // noProxy is passed as `extraNoProxy`.
                let url = non_empty(&settings.url)?;
                (url, non_empty_opt(settings.no_proxy.as_deref()))
            }
            Some(settings) => match env_url {
                // `installGlobalProxy`: an env-sourced dispatcher wins over
                // the first config install (`proxy.ts:64-69`).
                Some(url) => (url, None),
                None => {
                    // `installGlobalProxy(config.url)` only passes the URL;
                    // `config.noProxy` is first honored on the reload path.
                    let url = non_empty(&settings.url)?;
                    (url, None)
                }
            },
        };
        let user_no_proxy = lookup("no_proxy")
            .filter(|value| !value.is_empty())
            .or_else(|| lookup("NO_PROXY").filter(|value| !value.is_empty()))
            .unwrap_or_default();
        Self::new_with_extra(&proxy_url, &user_no_proxy, extra_no_proxy.as_deref())
    }

    #[cfg(test)]
    fn new(proxy_url: &str, user_no_proxy: &str) -> Option<Self> {
        Self::new_with_extra(proxy_url, user_no_proxy, None)
    }

    fn new_with_extra(
        proxy_url: &str,
        user_no_proxy: &str,
        extra_no_proxy: Option<&str>,
    ) -> Option<Self> {
        let proxy = url::Url::parse(proxy_url).ok()?;
        if proxy.host_str().is_none() || proxy.scheme() != "http" {
            return None;
        }
        let no_proxy = [
            Some(user_no_proxy),
            extra_no_proxy,
            Some("127.0.0.1"),
            Some("localhost"),
        ]
        .into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(",");
        let mut entries = Vec::new();
        for entry in no_proxy.split(|character: char| character == ',' || character.is_whitespace())
        {
            if entry.is_empty() {
                continue;
            }
            // JS `/^(.+):(\d+)$/`: a trailing numeric port, anything else is
            // part of the hostname (`[::1]` stays bracketed). `Number.parseInt`
            // has no 32-bit limit, so an overflowing numeric port must stay a
            // nonzero value that never equals a real port instead of becoming
            // the wildcard `0`.
            let (host, port) = match entry.rsplit_once(':') {
                Some((host, port))
                    if !host.is_empty()
                        && !port.is_empty()
                        && port.bytes().all(|byte| byte.is_ascii_digit()) =>
                {
                    (host, Some(port.parse::<u64>().unwrap_or(u64::MAX)))
                }
                _ => (entry, None),
            };
            let hostname = host
                .strip_prefix("*.")
                .or_else(|| host.strip_prefix('.'))
                .unwrap_or(host)
                .to_ascii_lowercase();
            entries.push(NoProxyEntry {
                hostname,
                // JS `0` is falsy: it matches every port.
                port: port.filter(|port| *port != 0),
            });
        }
        let authorization = proxy_authorization(&proxy).ok()?;
        Some(Self {
            proxy,
            no_proxy,
            entries,
            authorization,
        })
    }

    /// `EnvHttpProxyAgent.#shouldProxy` against a hyper URI.
    pub(crate) fn should_proxy(&self, uri: &hyper::Uri) -> bool {
        let Some(host) = uri.host() else {
            return false;
        };
        let host = host.to_ascii_lowercase();
        let port = u64::from(uri.port_u16().unwrap_or(match uri.scheme_str() {
            Some("https") => 443,
            _ => 80,
        }));
        if self.entries.is_empty() {
            return true;
        }
        if self.no_proxy == "*" {
            return false;
        }
        !self.entries.iter().any(|entry| {
            if entry.port.is_some_and(|entry_port| entry_port != port) {
                return false;
            }
            host == entry.hostname || host.ends_with(&format!(".{}", entry.hostname))
        })
    }

    fn host_port(&self) -> (String, u16) {
        let host = match self.proxy.host() {
            Some(url::Host::Ipv6(address)) => address.to_string(),
            Some(url::Host::Ipv4(address)) => address.to_string(),
            Some(url::Host::Domain(name)) => name.to_owned(),
            None => String::new(),
        };
        (host, self.proxy.port_or_known_default().unwrap_or(80))
    }

    /// `host:port` as Node's socket errors print it (IPv6 without brackets).
    fn address(&self) -> String {
        let (host, port) = self.host_port();
        format!("{host}:{port}")
    }

    /// `Proxy-Authorization` from URL userinfo (`undici/lib/dispatcher/
    /// proxy-agent.js:113-121`); no header when there is no username.
    fn authorization(&self) -> Option<String> {
        self.authorization.clone()
    }

    fn connect_error(&self, error: std::io::Error) -> ProxyError {
        let address = self.address();
        let cause = match error.kind() {
            std::io::ErrorKind::ConnectionRefused => {
                format!("connect ECONNREFUSED {address}")
            }
            std::io::ErrorKind::TimedOut => {
                format!("Connect Timeout Error (attempted address: {address}, timeout: 10000ms)")
            }
            _ if error
                .to_string()
                .contains("failed to lookup address information") =>
            {
                format!("getaddrinfo ENOTFOUND {}", self.host_port().0)
            }
            _ => format!("connect {address}"),
        };
        ProxyError::new(cause)
    }

    fn response_error(status: u16) -> ProxyError {
        // eventsource@3 flattens the fetch rejection as
        // `<DOMException name>: <message>: <cause.message>`; the fetch layer
        // produced a "Request was cancelled." DOMException whose cause is the
        // undici ProxyAgent tunnel error.
        ProxyError::new(format!(
            "Error: Request was cancelled.: Proxy response ({status}) !== 200 when HTTP Tunneling"
        ))
    }

    /// Open one `CONNECT` tunnel; the returned stream is the raw tunnel plus
    /// any bytes the proxy already sent after its response headers.
    async fn tunnel(
        &self,
        target: &hyper::Uri,
    ) -> Result<(tokio::net::TcpStream, Vec<u8>), ProxyError> {
        let (host, port) = self.host_port();
        let mut stream = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|error| self.connect_error(error))?;
        let (authority, host_header) = connect_target(target);
        let mut request = format!(
            "CONNECT {authority} HTTP/1.1\r\nhost: {host_header}\r\nproxy-connection: keep-alive\r\n"
        );
        if let Some(auth) = self.authorization() {
            request.push_str(&format!("proxy-authorization: {auth}\r\n"));
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|error| self.connect_error(error))?;
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            if let Some(at) = find_header_end(&buffer) {
                break at;
            }
            if buffer.len() > 64 * 1024 {
                return Err(ProxyError::new(
                    "Proxy response (0) !== 200 when HTTP Tunneling".into(),
                ));
            }
            let read = stream
                .read(&mut chunk)
                .await
                .map_err(|error| self.connect_error(error))?;
            if read == 0 {
                return Err(ProxyError::new(
                    "Proxy response (0) !== 200 when HTTP Tunneling".into(),
                ));
            }
            buffer.extend_from_slice(&chunk[..read]);
        };
        let head = String::from_utf8_lossy(&buffer[..header_end]);
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse::<u16>().ok())
            .unwrap_or(0);
        if status != 200 {
            return Err(Self::response_error(status));
        }
        Ok((stream, buffer[header_end + 4..].to_vec()))
    }
}

/// `CONNECT` target and `host` header for one URL, matching undici
/// `proxy-agent.js:183-196`: `opts.host` is the normalized `URL.host` (default
/// port omitted, IPv6 brackets kept), `requestedPath` appends `:defaultPort`
/// only when the URL had no port, and the header is `opts.host` unchanged.
fn connect_target(target: &hyper::Uri) -> (String, String) {
    let host = target.host().unwrap_or_default();
    let default_port = match target.scheme_str() {
        Some("https") => 443,
        _ => 80,
    };
    let port = target.port_u16().unwrap_or(default_port);
    let authority = format!("{host}:{port}");
    let header = if port == default_port {
        host.to_owned()
    } else {
        authority.clone()
    };
    (authority, header)
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// `ProxyAgent`'s constructor header: `Basic base64(decodeURIComponent(user):
/// decodeURIComponent(password))`, or `None` for no username. An invalid
/// escape or a percent-decoded byte sequence that is not UTF-8 makes
/// `decodeURIComponent` throw `URIError: URI malformed` in TS, which aborts
/// the whole proxy install; `Err` here makes the caller fall back to direct
/// exactly like that failed install (no proxy, so no credentials are sent).
fn proxy_authorization(proxy: &url::Url) -> Result<Option<String>, ()> {
    let username = proxy.username();
    if username.is_empty() {
        return Ok(None);
    }
    let username = percent_decode(username)?;
    let password = match proxy.password() {
        Some(password) => percent_decode(password)?,
        None => String::new(),
    };
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    Ok(Some(format!("Basic {token}")))
}

/// `decodeURIComponent` for proxy userinfo: `%XX` escapes are decoded and the
/// result must be valid UTF-8. Malformed escapes (`%ZZ`, a truncated `%A`) and
/// invalid UTF-8 (`%FF`) are errors, matching the `URIError` the TS install
/// raises (`undici/lib/dispatcher/proxy-agent.js:129,131`).
fn percent_decode(input: &str) -> Result<String, ()> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let (Some(high), Some(low)) = (
                bytes.get(index + 1).copied().and_then(hex_digit),
                bytes.get(index + 2).copied().and_then(hex_digit),
            ) else {
                return Err(());
            };
            out.push(high << 4 | low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Socket bytes already read past the CONNECT response, then the socket.
/// Both the tcp and proxy paths return this same type so
/// `HttpsConnector` can wrap one connector type.
pub(crate) struct PrefixedStream<S> {
    inner: S,
    prefix: Vec<u8>,
    position: usize,
}
impl<S> PrefixedStream<S> {
    fn new(inner: S, prefix: Vec<u8>) -> Self {
        Self {
            inner,
            prefix,
            position: 0,
        }
    }
}
impl<S: hyper::rt::Read + Unpin> hyper::rt::Read for PrefixedStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        mut buffer: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.position < this.prefix.len() {
            let count = (this.prefix.len() - this.position).min(buffer.remaining());
            buffer.put_slice(&this.prefix[this.position..this.position + count]);
            this.position += count;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(context, buffer)
    }
}
impl<S> hyper_util::client::legacy::connect::Connection for PrefixedStream<S> {
    fn connected(&self) -> hyper_util::client::legacy::connect::Connected {
        hyper_util::client::legacy::connect::Connected::new()
    }
}
impl<S: hyper::rt::Write + Unpin> hyper::rt::Write for PrefixedStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(context, buffer)
    }
    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(context)
    }
    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }
}

/// TCP connector that either delegates to hyper's `HttpConnector` or opens an
/// HTTP `CONNECT` tunnel first. `HttpsConnector` still terminates TLS to the
/// target on top of the returned socket.
#[derive(Clone)]
pub(crate) struct ProxyConnect {
    inner: HttpConnector,
    proxy: Option<Arc<ProxyConfig>>,
}
impl tower_service::Service<hyper::Uri> for ProxyConnect {
    type Response = PrefixedStream<TokioIo<tokio::net::TcpStream>>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context).map_err(Into::into)
    }
    fn call(&mut self, destination: hyper::Uri) -> Self::Future {
        if let Some(proxy) = self
            .proxy
            .clone()
            .filter(|proxy| proxy.should_proxy(&destination))
        {
            return Box::pin(async move {
                let (stream, prefix) = proxy
                    .tunnel(&destination)
                    .await
                    .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
                Ok(PrefixedStream::new(TokioIo::new(stream), prefix))
            });
        }
        let direct = self.inner.call(destination);
        Box::pin(async move {
            direct
                .await
                .map(|stream| PrefixedStream::new(stream, Vec::new()))
                .map_err(Into::into)
        })
    }
}

/// Shared connector for MCP transports: [`https_connector`] plus the outgoing
/// proxy (env-configured; `None` keeps the direct path).
pub(crate) fn proxy_https_connector(
    config: rustls::ClientConfig,
    proxy: Option<ProxyConfig>,
) -> hyper_rustls::HttpsConnector<ProxyConnect> {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_connect_timeout(Some(Duration::from_secs(10)));
    hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_or_http()
        .enable_http1()
        .wrap_connector(ProxyConnect {
            inner: http,
            proxy: proxy.map(Arc::new),
        })
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
    /// Production always uses `UNDICI_BODY_TIMEOUT_MS`; undici's zero disables
    /// this timeout, but zero is not supported by our Node timer normalization.
    pub body_timeout_ms: f64,
    /// The consumer pull boundary of the module port that owns this stream.
    /// `None` for router-internal callers, whose sinks are driven to
    /// completion without a consumer pacing them.
    pub yield_gate: Option<YieldGate>,
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
            yield_gate: None,
        }
    }
}
impl StreamOptions {
    /// Suspend at the TS yield boundary: after an event becomes
    /// consumer-visible, the provider stream stays suspended until the
    /// consumer pulls it. A no-op without a gate or without outstanding
    /// events, so pre-content buffering never blocks. The abort flag also
    /// ends the suspension — TS registers the abort listener directly on the
    /// response body (`streamModel.ts`'s `cancelReader`), so cancellation
    /// tears the request down even while the generator is suspended at a
    /// yield; the transport's next read observes the flag and unwinds, which
    /// drops the connection like a cancelled reader. The post-termination
    /// side effects of the Router must not follow that release: they use
    /// [`Self::terminal_yield_boundary`], which only a pull opens.
    pub async fn yield_boundary(&self) {
        if let Some(gate) = &self.yield_gate {
            tokio::select! {
                () = gate.demand() => {}
                () = wait_abort(&self.abort) => {}
            }
        }
    }

    /// The Router's terminal boundary: like [`Self::yield_boundary`] it holds
    /// the code after the yield behind the pulls that consume the already
    /// delivered events, but a turn cancel does not release it. TS's
    /// `execute_failed`, `observeRequest` and the terminal error event run
    /// only when the generator resumes on a pull (`RouterRuntime.ts:962-998`
    /// sits behind the per-event yields of `:873-877`); a cancel only tears
    /// the provider connection down (`streamModel.ts`'s `cancelReader`) — it
    /// never advances a suspended generator past its yield, so publishing the
    /// failure without the consuming pulls would diverge from TS. The body
    /// task stays suspended here until the consumer pulls again or closes
    /// the stream, like a TS generator nobody pulls.
    pub async fn terminal_yield_boundary(&self) {
        if let Some(gate) = &self.yield_gate {
            gate.demand().await;
        }
    }
}

/// The TS generator pull boundary between a provider stream and its
/// consumer. A TS async generator suspends at every consumer-facing
/// `yield`, so provider reads, retry handling and terminal side effects
/// stay behind the pull that consumes the last delivered event
/// (`RouterRuntime.ts:775-808`). The push-based Rust port reproduces that
/// with two counters: the producer `deliver()`s when an event becomes
/// consumer-visible and `demand()`s the boundary before producing more;
/// the consumer `pull()`s when the consumer asks for the next event —
/// deferred past the queue-take to the start of the following `next()` call,
/// since a TS generator resumes from its `yield` only on the next pull.
#[derive(Clone, Default)]
pub struct YieldGate {
    /// `(delivered, pulled)`.
    state: Arc<watch::Sender<(u64, u64)>>,
}
impl YieldGate {
    pub fn new() -> Self {
        Self::default()
    }
    /// An event became consumer-visible (`execute`'s sink).
    pub fn deliver(&self) {
        self.state.send_if_modified(|state| {
            state.0 += 1;
            true
        });
    }
    /// The consumer took a delivered event.
    pub fn pull(&self) {
        self.state.send_if_modified(|state| {
            state.1 += 1;
            true
        });
    }
    /// Wait until every delivered event has been pulled.
    pub async fn demand(&self) {
        let mut receiver = self.state.subscribe();
        loop {
            {
                let state = receiver.borrow_and_update();
                if state.0 <= state.1 {
                    return;
                }
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

const DEFAULT_STREAM_IDLE_TIMEOUT_MS: f64 = UNDICI_BODY_TIMEOUT_MS;
/// `UNDICI_TRANSPORT_TIMEOUT_MS` (`cli/proxy.ts`, = LITELLM_COMPLETION_HTTP_FALLBACK_MS).
pub use crate::runtime::network::UNDICI_BODY_TIMEOUT_MS;
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
    crate::runtime::network::node_timer_duration(ms)
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
pub(super) async fn delay(ms: f64, options: &StreamOptions) -> Result<bool, ModelStreamError> {
    use crate::runtime::network::{Exchange, delay};
    match delay(
        duration(ms),
        wait_abort(&options.abort),
        || options.abort.as_ref().and_then(AbortFlag::abort_event_at),
        None,
        false,
    )
    .await
    {
        Exchange::Response((), before_abort) => Ok(before_abort),
        Exchange::Aborted => Err(ModelStreamError::abort()),
        Exchange::Timeout { .. } => {
            unreachable!("provider retry waits have no surrounding deadline")
        }
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

/// Preserve each redirect hop's completion source for both provider transports.
/// Errors retain whether their continuation precedes a later parent timer, so
/// Google's outer catch can use the same ordering as a successful response.
async fn provider_exchange(
    transport: &dyn ModelHttpTransport,
    request: Request<Full<Bytes>>,
    options: &StreamOptions,
    deadline: tokio::time::Instant,
    failure: impl FnOnce(TransportFailure) -> ModelStreamError,
    timeout: impl FnOnce() -> ModelStreamError,
) -> Result<Response<(ModelBody, bool)>, (ModelStreamError, bool)> {
    use crate::runtime::network::{Exchange, exchange_with_prearmed_parent};
    let send = transport.send(request);
    match exchange_with_prearmed_parent(
        send,
        wait_abort(&options.abort),
        || options.abort.as_ref().and_then(AbortFlag::abort_event_at),
        Some(deadline),
    )
    .await
    {
        Exchange::Response(result, before_abort) => result
            .map(|response| response.map(|body| (body, before_abort)))
            .map_err(|error| {
                // TS :749 (Google :502) observes same-stack abort even for a
                // rejected Promise. A later timer cannot interrupt the error
                // callback's microtasks, including when both timers are overdue.
                let error = if !before_abort && aborted(options) {
                    ModelStreamError::abort()
                } else {
                    failure(error)
                };
                // Google rechecks cancellation after SDK normalization too.
                (error, before_abort)
            }),
        Exchange::Aborted => Err((ModelStreamError::abort(), false)),
        Exchange::Timeout { before_abort } => Err((timeout(), before_abort)),
    }
}

/// `sendProviderRequest` + `sendWithEndpointFallback` + `networkFetch`.
async fn send_provider_request(
    transport: &dyn ModelHttpTransport,
    provider: &JsValue,
    body: &JsValue,
    options: &StreamOptions,
) -> Result<Response<(ModelBody, bool)>, ModelStreamError> {
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
    // This timer surrounds every endpoint and redirect. Race each observable
    // transport exchange before follow() can erase its scheduled_at() source.
    let deadline = tokio::time::Instant::now() + duration(timeout_ms);
    let mut last = None;
    for endpoint in &endpoints {
        let (url, header_map) = build_http_request(endpoint, &headers).map_err(to_provider)?;
        let response = crate::fetch_redirect::follow(
            url,
            hyper::Method::POST,
            header_map,
            encoded.clone(),
            || to_provider(NetworkFetchError::classify("fetch failed", None)),
            |request| async {
                provider_exchange(
                    transport,
                    request,
                    options,
                    deadline,
                    |failure| {
                        to_provider(NetworkFetchError::classify(
                            &failure.message,
                            failure.cause_code,
                        ))
                    },
                    || {
                        to_provider(NetworkFetchError::new(
                            "network_timeout",
                            format!(
                                "Stream idle timeout: no data received for {}ms",
                                crate::js_value::number_string(timeout_ms)
                            ),
                            JsValue::Undefined,
                        ))
                    },
                )
                .await
                .map_err(|(error, _)| error)
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
    last.ok_or_else(|| to_provider(NetworkFetchError::classify("fetch failed", None)))
}

/// `await response.text()`: UTF-8 with BOM stripped.
async fn response_text(
    body: &mut ModelBody,
    options: &StreamOptions,
    mut continuation: bool,
) -> Result<String, ModelStreamError> {
    // TS reads this body after the request timer was cleared and the abort
    // listener detached; only undici's `bodyTimeout` bounds a stalled body
    // (node probe: `TypeError: terminated`, cause UND_ERR_BODY_TIMEOUT).
    // Google wraps this body read in its still-pending SDK promise below.
    let mut decoder = sse::Utf8Decoder::default();
    let mut text = String::new();
    let mut units = 0usize;
    loop {
        use crate::runtime::network::{Exchange, body_frame};
        match body_frame(
            body,
            Some(duration(options.body_timeout_ms)),
            None,
            &mut continuation,
        )
        .await
        {
            Exchange::Aborted => return Err(ModelStreamError::abort()),
            Exchange::Timeout { .. } | Exchange::Response(Some(Err(_)), _) => {
                return Err(ModelStreamError::thrown("TypeError", "terminated"));
            }
            Exchange::Response(None, _) => break,
            Exchange::Response(Some(Ok(frame)), _) => {
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

/// Read one body frame under the idle timeout and the abort signal. Both a
/// response and a timeout carry their continuation order through Google's
/// outer abort recheck, including when the body stalls after partial text.
async fn read_frame(
    body: &mut ModelBody,
    idle_ms: f64,
    options: &StreamOptions,
    continuation: &mut bool,
) -> Result<Option<Bytes>, ModelStreamError> {
    // undici's bodyTimeout races the idle watchdog; the shorter one wins.
    // Equal durations favor the watchdog: undici timers above 1 s run on its
    // coarse fast-timer tick and fire late, while `withIdleTimeout` uses a
    // native setTimeout (real TS over TCP: 5000/5000 tie -> idle 5/5; default
    // 600000 tie -> "Stream idle timeout", _tmp/20261021-issue-396/arb/).
    let body_first = duration(options.body_timeout_ms) < duration(idle_ms);
    let wait = if body_first {
        options.body_timeout_ms
    } else {
        idle_ms
    };
    loop {
        use crate::runtime::network::{Exchange, body_frame_with_prearmed_parent};
        match body_frame_with_prearmed_parent(
            body,
            Some(duration(wait)),
            options.abort.as_ref(),
            continuation,
        )
        .await
        {
            Exchange::Aborted => return Err(ModelStreamError::abort()),
            Exchange::Timeout { .. } if body_first => {
                return Err(ModelStreamError::thrown("TypeError", "terminated"));
            }
            Exchange::Timeout { .. } => {
                return Err(ModelStreamError::thrown(
                    "StreamIdleTimeoutError",
                    format!(
                        "Stream idle timeout: no data received for {}ms",
                        crate::js_value::number_string(idle_ms)
                    ),
                ));
            }
            Exchange::Response(None, _) => return Ok(None),
            Exchange::Response(Some(Err(_)), _) => {
                return Err(ModelStreamError::thrown("TypeError", "terminated"));
            }
            Exchange::Response(Some(Ok(frame)), _) => {
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
) -> Result<Response<(ModelBody, bool)>, (ModelStreamError, bool)> {
    let timeout_ms = idle_timeout_ms(provider, options);
    let final_body = providers::google::final_body(body, provider);
    let headers = providers::google::headers(provider).map_err(|error| (error, false))?;
    let encoded = crate::js_value::json_stringify(&final_body)
        .map_err(|_| {
            (
                ModelStreamError::thrown("RangeError", crate::js_value::StackOverflow::MESSAGE),
                false,
            )
        })?
        .unwrap_or_default();
    let endpoint = providers::google::endpoint(provider, model).map_err(|error| (error, false))?;
    let (url, header_map) = build_http_request(&endpoint, &headers)
        .map_err(|error| (ModelStreamError::thrown("TypeError", error.message), false))?;
    let deadline = tokio::time::Instant::now() + duration(timeout_ms);
    crate::fetch_redirect::follow(
        url,
        hyper::Method::POST,
        header_map,
        Bytes::from(encoded),
        || (ModelStreamError::thrown("TypeError", "fetch failed"), false),
        |request| async move {
            provider_exchange(
                transport,
                request,
                options,
                deadline,
                |failure| ModelStreamError::thrown("TypeError", failure.message),
                || {
                    ModelStreamError::thrown(
                        "StreamIdleTimeoutError",
                        format!(
                            "Stream idle timeout: no data received for {}ms",
                            crate::js_value::number_string(timeout_ms)
                        ),
                    )
                },
            )
            .await
        },
    )
    .await
}

/// `throwErrorIfNotOK` + `toProviderError` for the SDK. A JSON content type is
/// parsed like `Response.json()`; any other content type is synthesized into
/// `{error:{message,code,status}}` before `JSON.stringify`.
async fn google_response_error(
    response: Response<(ModelBody, bool)>,
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
    let (mut body, before_abort) = response.into_body();
    let text = response_text(&mut body, options, before_abort).await?;
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
) -> (Result<(), ModelStreamError>, bool) {
    // Error normalization is also a queued continuation: a later parent timer
    // must not replace an already-read SDK error with AbortError.
    let mut body_before_abort = false;
    let sdk_deadline = tokio::time::Instant::now() + duration(idle_timeout_ms(provider, options));
    let outcome = async {
        let response = send_google_request(transport, provider, model, body, options)
            .await
            .map_err(|(error, before_abort)| {
                body_before_abort = before_abort;
                error
            })?;
        body_before_abort = response.body().1;
        let status = response.status().as_u16();
        if matches!(status, 204 | 205) {
            // fetch exposes a null body for null-body statuses; the SDK's stream
            // reader throws before yielding any chunk.
            return Err(ModelStreamError::thrown("Error", "Response body is empty"));
        }
        if !(200..300).contains(&status) {
            use crate::runtime::network::{Exchange, exchange_prearmed_continuing};
            let idle_ms = idle_timeout_ms(provider, options);
            let provider = provider.clone();
            let read_options = options.clone();
            let read = crate::runtime::network::ExchangeFuture::io(async move {
                google_response_error(response, &read_options, &provider).await
            });
            // Preserve the SDK error-body promise's catch continuation.
            return match exchange_prearmed_continuing(
                read,
                wait_abort(&options.abort),
                || options.abort.as_ref().and_then(AbortFlag::abort_event_at),
                Some(sdk_deadline),
                body_before_abort,
            )
            .await
            {
                Exchange::Response(result, before_abort) => {
                    body_before_abort = before_abort;
                    Err(result?)
                }
                Exchange::Aborted => Err(ModelStreamError::abort()),
                Exchange::Timeout { before_abort } => {
                    body_before_abort = before_abort;
                    Err(ModelStreamError::thrown(
                        "StreamIdleTimeoutError",
                        format!(
                            "Stream idle timeout: no data received for {}ms",
                            crate::js_value::number_string(idle_ms)
                        ),
                    ))
                }
            };
        }
        let (mut body, _) = response.into_body();
        let mut state = providers::google::StreamState::default();
        let idle_ms = idle_timeout_ms(provider, options);
        let mut guard = StreamGuard::new(provider, (options.clock)());
        let mut buffer = sse::SseBuffer::with_limit(sse_limit());
        loop {
            if !body_before_abort {
                throw_if_aborted(options)?;
            }
            let chunk = read_frame(&mut body, idle_ms, options, &mut body_before_abort).await?;
            if !body_before_abort {
                throw_if_aborted(options)?;
            }
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
                    options.yield_boundary().await;
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
    .await;
    (outcome, body_before_abort)
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

/// `throwIfGoogleAbort`: preserve abort-like errors, while an observed parent
/// cancellation takes precedence. A non-abort error continues into retry handling.
fn throw_if_google_abort(
    error: ModelStreamError,
    options: &StreamOptions,
    before_abort: bool,
) -> Result<ModelStreamError, ModelStreamError> {
    if !before_abort && aborted(options) {
        return Err(ModelStreamError::abort());
    }
    if error.is_abort() {
        return Err(error);
    }
    Ok(error)
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
    let mut continuation_before_abort = false;
    while attempt <= max_retries {
        if !std::mem::take(&mut continuation_before_abort) {
            throw_if_aborted(options)?;
        }
        sink(StreamOutput::Event(request_started_event(
            provider, &current,
        )?));
        options.yield_boundary().await;

        let (_, model) = providers::validate_model_request(&current, config)?;
        let model_id = field(&current, "model").clone();
        let (outcome, before_abort) = match providers::google::build_request(&current, &model) {
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
            Err(error) => (Err(error), false),
        };
        let Err(error) = outcome else {
            return Ok(());
        };
        let error = throw_if_google_abort(error, options, before_abort)?;
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
            continuation_before_abort = delay(delay_ms, options).await?;
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
            continuation_before_abort = delay(delay_ms, options).await?;
            attempt += 1.0;
            continue;
        }
        let mut canonical = canonical;
        if retryable {
            canonical.insert("streamInterruption".into(), checkpoint.interruption());
        }
        sink(StreamOutput::Event(error_event(JsValue::Object(canonical))));
        options.yield_boundary().await;
        return Ok(());
    }
    Ok(())
}

/// `parseModelResponse(protocol, raw, providerId)` — converts provider-specific
/// complete response to canonical format. Ports `parseModelResponse.ts` and
/// provider-specific `parse*Response` functions.
fn parse_complete_response(protocol: &JsValue, raw: &JsValue, provider_id: &JsValue) -> JsValue {
    let protocol_str = protocol
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_default();

    match protocol_str.as_str() {
        "anthropic" => parse_anthropic_complete_response(raw),
        "openai-responses" => parse_openai_responses_complete_response(raw, provider_id),
        _ => parse_openai_complete_response(raw, provider_id), // default: openai
    }
}

/// Parses Anthropic complete response to canonical format.
/// Ports `parseAnthropicResponse` from `anthropic/response.ts`.
fn parse_anthropic_complete_response(raw: &JsValue) -> JsValue {
    let response = if matches!(raw, JsValue::Object(_)) {
        raw
    } else {
        &JsValue::Object(JsObject::new())
    };

    let content_array = field(response, "content");
    let content: Vec<JsValue> = if matches!(content_array, JsValue::Array(_)) {
        array(content_array)
            .iter()
            .flat_map(|block| {
                let block_obj = if matches!(block, JsValue::Object(_)) {
                    block
                } else {
                    return Vec::new();
                };
                let block_type = field(block_obj, "type");
                let block_type_str = block_type
                    .as_string()
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default();

                match block_type_str.as_str() {
                    "text" => {
                        let text = field(block_obj, "text")
                            .as_string()
                            .map_or("".into(), |s| JsValue::String(s.clone()));
                        vec![object([("type", "text".into()), ("text", text)])]
                    }
                    "thinking" => {
                        let thinking_text = field(block_obj, "thinking")
                            .as_string()
                            .or_else(|| field(block_obj, "text").as_string())
                            .map_or("".into(), |s| JsValue::String(s.clone()));
                        let mut thinking_block =
                            object([("type", "thinking".into()), ("text", thinking_text)]);
                        if let Some(sig) = field(block_obj, "signature").as_string() {
                            put(
                                &mut thinking_block,
                                "signature",
                                JsValue::String(sig.clone()),
                            );
                        }
                        vec![thinking_block]
                    }
                    "tool_use" => {
                        let id = field(block_obj, "id")
                            .as_string()
                            .map_or("".into(), |s| JsValue::String(s.clone()));
                        let name = field(block_obj, "name")
                            .as_string()
                            .map_or("".into(), |s| JsValue::String(s.clone()));
                        vec![object([
                            ("type", "tool_call".into()),
                            ("id", id),
                            ("name", name),
                            ("input", field(block_obj, "input").clone()),
                            ("raw", block.clone()),
                        ])]
                    }
                    _ => Vec::new(),
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    // Inline normalize Anthropic usage
    let usage_raw = field(response, "usage");
    let usage = if matches!(usage_raw, JsValue::Object(_)) {
        let mut usage_obj = JsObject::new();
        if let Some(input_tokens) = field(usage_raw, "input_tokens").as_number() {
            usage_obj.insert("inputTokens".into(), JsValue::Number(input_tokens));
        }
        if let Some(output_tokens) = field(usage_raw, "output_tokens").as_number() {
            usage_obj.insert("outputTokens".into(), JsValue::Number(output_tokens));
        }
        if let Some(cache_read) = field(usage_raw, "cache_read_input_tokens").as_number() {
            usage_obj.insert("cacheReadTokens".into(), JsValue::Number(cache_read));
        }
        if let Some(cache_write) = field(usage_raw, "cache_creation_input_tokens").as_number() {
            usage_obj.insert("cacheWriteTokens".into(), JsValue::Number(cache_write));
        }
        if !usage_obj.is_empty() {
            JsValue::Object(usage_obj)
        } else {
            JsValue::Undefined
        }
    } else {
        JsValue::Undefined
    };

    let finish_reason = normalize_anthropic_finish_reason(field(response, "stop_reason"));

    object([
        ("role", "assistant".into()),
        ("content", JsValue::Array(content)),
        ("usage", usage),
        ("finishReason", finish_reason),
        ("raw", raw.clone()),
    ])
}

/// Parses OpenAI complete response to canonical format.
/// Ports `parseOpenAIResponse` from `openai/response.ts`.
fn parse_openai_complete_response(raw: &JsValue, _provider_id: &JsValue) -> JsValue {
    let response = if matches!(raw, JsValue::Object(_)) {
        raw
    } else {
        &JsValue::Object(JsObject::new())
    };

    let empty_obj = JsValue::Object(JsObject::new());

    let choices = field(response, "choices");
    let choices_array: &[JsValue] = if matches!(choices, JsValue::Array(_)) {
        array(choices)
    } else {
        &[]
    };

    let first_choice = choices_array.first().unwrap_or(&empty_obj);
    let first_choice = if matches!(first_choice, JsValue::Object(_)) {
        first_choice
    } else {
        &empty_obj
    };

    let message = field(first_choice, "message");
    let message = if matches!(message, JsValue::Object(_)) {
        message
    } else {
        &empty_obj
    };

    let mut content: Vec<JsValue> = Vec::new();

    // Handle reasoning/thinking content
    if let Some(reasoning) = field(message, "reasoning_content")
        .as_string()
        .or_else(|| field(message, "reasoning").as_string())
    {
        if !reasoning.is_empty() {
            content.push(object([
                ("type", "thinking".into()),
                ("text", JsValue::String(reasoning.clone())),
                ("reasoningContent", JsValue::String(reasoning.clone())),
            ]));
        }
    }

    // Handle text content
    if let Some(text) = field(message, "content").as_string() {
        if !text.is_empty() {
            content.push(object([
                ("type", "text".into()),
                ("text", JsValue::String(text.clone())),
            ]));
        }
    }

    // Handle tool calls
    let tool_calls = field(message, "tool_calls");
    if matches!(tool_calls, JsValue::Array(_)) {
        for call in array(tool_calls) {
            let call_obj = if matches!(call, JsValue::Object(_)) {
                call
            } else {
                continue;
            };
            let id = field(call_obj, "id")
                .as_string()
                .map_or("".into(), |s| JsValue::String(s.clone()));
            let function = field(call_obj, "function");
            let name = field(function, "name")
                .as_string()
                .map_or("".into(), |s| JsValue::String(s.clone()));
            let arguments = field(function, "arguments");
            let input = parse_json(
                arguments
                    .as_string()
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default()
                    .as_str(),
            )
            .unwrap_or(JsValue::Object(JsObject::new()));

            content.push(object([
                ("type", "tool_call".into()),
                ("id", id),
                ("name", name),
                ("input", input),
                ("raw", call.clone()),
            ]));
        }
    }

    let usage = providers::openai::normalize_usage(field(response, "usage"));
    let finish_reason = normalize_openai_finish_reason(field(first_choice, "finish_reason"));

    object([
        ("role", "assistant".into()),
        ("content", JsValue::Array(content)),
        ("usage", usage.unwrap_or(JsValue::Undefined)),
        ("finishReason", finish_reason),
        ("raw", raw.clone()),
    ])
}

/// Parses OpenAI-responses complete response to canonical format.
/// Ports `parseOpenAIResponsesResponse` from `openai-responses/response.ts`.
fn parse_openai_responses_complete_response(raw: &JsValue, _provider_id: &JsValue) -> JsValue {
    // openai-responses typically has the same structure as openai for complete responses
    parse_openai_complete_response(raw, _provider_id)
}

/// Normalizes Anthropic finish reason to canonical format.
fn normalize_anthropic_finish_reason(stop_reason: &JsValue) -> JsValue {
    let reason = stop_reason
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_default();
    match reason.as_str() {
        "end_turn" => "stop".into(),
        "max_tokens" => "length".into(),
        "tool_use" => "tool_calls".into(),
        "stop_sequence" => "stop".into(),
        _ if !reason.is_empty() => JsValue::String(JsString::from(reason.as_str())),
        _ => JsValue::Undefined,
    }
}

/// Normalizes OpenAI finish reason to canonical format.
fn normalize_openai_finish_reason(finish_reason: &JsValue) -> JsValue {
    let reason = finish_reason
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_default();
    match reason.as_str() {
        "stop" | "end_turn" => "stop".into(),
        "length" | "max_tokens" => "length".into(),
        "tool_calls" | "function_call" => "tool_calls".into(),
        "content_filter" => "content_filter".into(),
        _ if !reason.is_empty() => JsValue::String(JsString::from(reason.as_str())),
        _ => JsValue::Undefined,
    }
}

/// `complete(request, config, options)` — non-streaming HTTP path.
/// `streamModel.ts:76`: sends `stream: false`, reads the full body, returns the
/// parsed response object. Shares the retry policy with the streaming path.
pub async fn complete_model(
    transport: &dyn ModelHttpTransport,
    request: &JsValue,
    config: &JsValue,
    options: &StreamOptions,
) -> Result<JsValue, ModelStreamError> {
    let mut current = request.clone();
    put(&mut current, "stream", JsValue::Bool(false));
    let (provider, _) = providers::validate_model_request(&current, config)?;

    // P1-2: Google protocol not supported in complete path
    if is(field(&provider, "protocol"), "google") {
        return Err(ModelStreamError::thrown(
            "UnsupportedProtocol",
            "complete(): Google protocol is not supported in non-streaming mode",
        ));
    }

    let (max_retries, _) = retry_decide(&provider, 0.0, None, 0.0);
    let mut attempt = 0.0;
    loop {
        throw_if_aborted(options)?;
        let (provider_now, model_now) = providers::validate_model_request(&current, config)?;
        let body = providers::build_model_request(&current, &provider_now, &model_now)?;
        let response = match send_provider_request(transport, &provider, &body, options).await {
            Ok(r) => r,
            Err(error) => {
                if attempt < max_retries && is_retryable_stream_error(&error) {
                    let (_, delay_ms) = retry_decide(&provider, attempt, None, (options.random)());
                    let _ = delay(delay_ms, options).await?;
                    attempt += 1.0;
                    continue;
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
        let (mut body_stream, before_abort) = response.into_body();
        let text = response_text(&mut body_stream, options, before_abort).await?;

        // P1-5: Handle retryable non-2xx responses (429/5xx)
        if !(200..300).contains(&status) {
            let raw = parse_json(&text).unwrap_or(JsValue::Null);
            let mut error = normalize_model_error(
                &provider_id(&provider),
                &protocol(&provider),
                ErrorInput::Value(&raw),
                Some(f64::from(status)),
            );
            // Consume Retry-After header if not already in error
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
                let _ = delay(delay_ms, options).await?;
                attempt += 1.0;
                continue;
            }
            return Err(ModelStreamError::Provider(error));
        }
        let raw = parse_json(&text).map_err(|_| {
            ModelStreamError::thrown("SyntaxError", "complete(): response is not valid JSON")
        })?;

        // P1-1: Parse response to canonical format
        let protocol_value = field(&provider, "protocol").clone();
        let provider_id_value = field(&provider, "id").clone();
        let canonical = parse_complete_response(&protocol_value, &raw, &provider_id_value);
        return Ok(canonical);
    }
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
    let mut continuation_before_abort = false;
    while attempt <= max_retries {
        if !std::mem::take(&mut continuation_before_abort) {
            throw_if_aborted(options)?;
        }
        sink(StreamOutput::Event(request_started_event(
            &provider, &current,
        )?));
        options.yield_boundary().await;
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
                    continuation_before_abort = delay(delay_ms, options).await?;
                    attempt += 1.0;
                    continue;
                }
                if is_retryable_stream_error(&error) && !checkpoint.is_empty() {
                    sink(StreamOutput::Event(error_event(interruption_error(
                        &provider,
                        &error,
                        &checkpoint,
                    ))));
                    options.yield_boundary().await;
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
        let (mut body, mut body_before_abort) = response.into_body();
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
            options.yield_boundary().await;
            return Ok(());
        }
        if !(200..300).contains(&status) {
            let text = response_text(&mut body, options, body_before_abort).await?;
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
                continuation_before_abort = delay(delay_ms, options).await?;
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
                options.yield_boundary().await;
                return Ok(());
            }
            sink(StreamOutput::Event(error_event(JsValue::Object(error))));
            options.yield_boundary().await;
            return Ok(());
        }

        let mut state = StreamState::new(&provider);
        let mut sentinel = false;
        let idle_ms = idle_timeout_ms(&provider, options);
        let mut guard = StreamGuard::new(&provider, (options.clock)());
        let mut buffer = sse::SseBuffer::with_limit(sse_limit());
        let outcome: Result<(), ModelStreamError> = async {
            loop {
                if !body_before_abort {
                    throw_if_aborted(options)?;
                }
                let chunk = read_frame(&mut body, idle_ms, options, &mut body_before_abort).await?;
                if !body_before_abort {
                    throw_if_aborted(options)?;
                }
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
                        options.yield_boundary().await;
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
            continuation_before_abort = delay(delay_ms, options).await?;
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
            continuation_before_abort = delay(delay_ms, options).await?;
            attempt += 1.0;
            continue;
        }
        if retryable {
            sink(StreamOutput::Event(error_event(interruption_error(
                &provider,
                &error,
                &checkpoint,
            ))));
            options.yield_boundary().await;
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "http_timing_tests.rs"]
mod timing_tests;

#[cfg(test)]
mod proxy_tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn proxy_env_priority_matches_the_ts_cli() {
        // `PILOTDECK_PROXY > https_proxy > HTTPS_PROXY > http_proxy > HTTP_PROXY`
        let config = ProxyConfig::from_lookup(lookup(&[
            ("HTTP_PROXY", "http://http:1"),
            ("http_proxy", "http://http-lower:2"),
            ("HTTPS_PROXY", "http://https-upper:3"),
            ("https_proxy", "http://https-lower:4"),
            ("PILOTDECK_PROXY", "http://pilotdeck:5"),
        ]))
        .expect("proxy configured");
        assert_eq!(config.host_port(), ("pilotdeck".into(), 5));

        let config = ProxyConfig::from_lookup(lookup(&[
            ("HTTP_PROXY", "http://http:1"),
            ("http_proxy", "http://http-lower:2"),
            ("HTTPS_PROXY", "http://https-upper:3"),
            ("https_proxy", "http://https-lower:4"),
        ]))
        .expect("proxy configured");
        assert_eq!(config.host_port(), ("https-lower".into(), 4));

        let config = ProxyConfig::from_lookup(lookup(&[
            ("HTTP_PROXY", "http://http:1"),
            ("http_proxy", "http://http-lower:2"),
            ("HTTPS_PROXY", "http://https-upper:3"),
        ]))
        .expect("proxy configured");
        assert_eq!(config.host_port(), ("https-upper".into(), 3));

        // Empty values are falsy in `env.X || env.Y` and fall through.
        let config = ProxyConfig::from_lookup(lookup(&[
            ("HTTP_PROXY", "http://http:1"),
            ("HTTPS_PROXY", ""),
            ("PILOTDECK_PROXY", ""),
        ]))
        .expect("proxy configured");
        assert_eq!(config.host_port(), ("http".into(), 1));

        assert!(ProxyConfig::from_lookup(lookup(&[])).is_none());
        // An unparsable URL keeps the baseline's direct dispatcher.
        assert!(ProxyConfig::from_lookup(lookup(&[("HTTP_PROXY", "127.0.0.1:1")])).is_none());
    }

    #[test]
    fn proxy_config_source_priority_matches_ts_install_paths() {
        let settings = |url: &str, no_proxy: Option<&str>, mode| ProxySettings {
            url: url.into(),
            no_proxy: no_proxy.map(Into::into),
            mode,
        };
        let env = lookup(&[("HTTP_PROXY", "http://env:1"), ("no_proxy", "env.example")]);

        // Startup: the env dispatcher wins and the config source (including
        // its noProxy) is not installed (`proxy.ts:64-69`).
        let config = ProxyConfig::from_settings_lookup(
            Some(&settings(
                "http://config:9",
                Some("config.example"),
                ProxyInstallMode::Startup,
            )),
            env,
        )
        .expect("env proxy");
        assert_eq!(config.host_port(), ("env".into(), 1));
        assert_eq!(config.no_proxy, "env.example,127.0.0.1,localhost");

        // Startup: without an env URL the config URL is the fallback, but the
        // config noProxy is not passed to `installGlobalProxy(config.url)`.
        let config = ProxyConfig::from_settings_lookup(
            Some(&settings(
                "http://config:9",
                Some("config.example"),
                ProxyInstallMode::Startup,
            )),
            lookup(&[("HTTP_PROXY", ""), ("no_proxy", "env.example")]),
        )
        .expect("config proxy");
        assert_eq!(config.host_port(), ("config".into(), 9));
        assert_eq!(config.no_proxy, "env.example,127.0.0.1,localhost");

        // Reload: `reinstallGlobalProxy` replaces the env dispatcher and
        // appends the config noProxy (`pilotdeck.ts:150`).
        let config = ProxyConfig::from_settings_lookup(
            Some(&settings(
                "http://config:9",
                Some("config.example"),
                ProxyInstallMode::Reload,
            )),
            lookup(&[("HTTP_PROXY", "http://env:1"), ("no_proxy", "env.example")]),
        )
        .expect("reloaded config proxy");
        assert_eq!(config.host_port(), ("config".into(), 9));
        assert_eq!(
            config.no_proxy,
            "env.example,config.example,127.0.0.1,localhost"
        );

        // Reload with no config URL removes the proxy (`reinstallGlobalProxy`
        // applies the direct dispatcher).
        assert!(
            ProxyConfig::from_settings_lookup(
                Some(&settings("", None, ProxyInstallMode::Reload)),
                lookup(&[("HTTP_PROXY", "http://env:1")]),
            )
            .is_none()
        );
    }

    #[test]
    fn proxy_settings_from_config_match_parse_proxy_config() {
        let parse = |text: &str| ProxySettings::from_config(&parse_json(text).unwrap());
        assert_eq!(
            parse("\"http://proxy:1\""),
            Some(ProxySettings {
                url: "http://proxy:1".into(),
                no_proxy: None,
                mode: ProxyInstallMode::Startup,
            })
        );
        assert_eq!(
            parse("{\"url\":\"http://proxy:2\",\"noProxy\":\"a.example,b.example\"}"),
            Some(ProxySettings {
                url: "http://proxy:2".into(),
                no_proxy: Some("a.example,b.example".into()),
                mode: ProxyInstallMode::Startup,
            })
        );
        // Empty strings are absent, exactly like `parseProxyConfig`.
        assert_eq!(parse("{\"url\":\"\"}"), None);
        assert_eq!(parse("{}"), None);
        assert_eq!(
            parse("{\"url\":\"http://p\",\"noProxy\":\"\"}"),
            Some(ProxySettings {
                url: "http://p".into(),
                no_proxy: None,
                mode: ProxyInstallMode::Startup,
            })
        );
        assert_eq!(parse("42"), None);
    }

    #[test]
    fn no_proxy_parsing_and_matching_match_undici() {
        let config = ProxyConfig::new("http://proxy:3128", "").expect("config");
        assert_eq!(config.no_proxy, "127.0.0.1,localhost");
        assert!(config.should_proxy(&"http://example.com/".parse().unwrap()));
        assert!(!config.should_proxy(&"http://127.0.0.1:9/".parse().unwrap()));
        assert!(!config.should_proxy(&"https://LOCALHOST/".parse().unwrap()));
        assert!(!config.should_proxy(&"http://sub.localhost/".parse().unwrap()));

        let config = ProxyConfig::new(
            "http://proxy:3128",
            " example.com:8080,\t.internal\n*.corp,127.0.0.9:1 ",
        )
        .expect("config");
        assert_eq!(
            config.no_proxy,
            " example.com:8080,\t.internal\n*.corp,127.0.0.9:1 ,127.0.0.1,localhost"
        );
        let bypass = |uri: &str| !config.should_proxy(&uri.parse().unwrap());
        assert!(bypass("http://example.com:8080/"));
        assert!(!bypass("http://example.com:8081/"));
        assert!(bypass("http://a.example.com:8080/"));
        assert!(bypass("http://a.internal/"));
        assert!(bypass("http://a.corp/"));
        assert!(bypass("http://corp/"));
        assert!(!bypass("http://notcorp/"));
        assert!(bypass("http://127.0.0.9:1/"));
        assert!(!bypass("http://127.0.0.9:2/"));

        // TS `buildNoProxy` joins entries, so a user `*` never equals `"*"`;
        // the wildcard branch is unreachable through the env path. Pin it.
        let config = ProxyConfig::new("http://proxy", "*").expect("config");
        assert_eq!(config.no_proxy, "*,127.0.0.1,localhost");
        assert!(config.should_proxy(&"http://example.com/".parse().unwrap()));
    }

    #[test]
    fn proxy_auth_uses_basic_credentials_from_the_url() {
        let config = ProxyConfig::new("http://us%20er:p%40ss@127.0.0.1:3128", "").expect("config");
        assert_eq!(
            config.authorization().as_deref(),
            Some("Basic dXMgZXI6cEBzcw==") // base64("us er:p@ss")
        );
        let username_only = ProxyConfig::new("http://user@127.0.0.1:3128", "").expect("config");
        assert_eq!(
            username_only.authorization().as_deref(),
            Some("Basic dXNlcjo=") // base64("user:")
        );
        let anonymous = ProxyConfig::new("http://127.0.0.1:3128", "").expect("config");
        assert!(anonymous.authorization().is_none());
    }

    #[test]
    fn no_proxy_port_parsing_matches_number_parse_int() {
        // `example.com:4294967296` is a truthy number that never equals a real
        // target port, so the entry must not bypass; the old `u32` overflow
        // to 0 made it a wildcard.
        let config = ProxyConfig::new(
            "http://proxy:3128",
            "example.com:4294967296,big.test:999999999999999999999999999999",
        )
        .expect("config");
        for uri in [
            "http://example.com/",
            "http://example.com:8080/",
            "http://example.com:65535/",
            "http://big.test:8080/",
        ] {
            assert!(
                config.should_proxy(&uri.parse().unwrap()),
                "{uri} must go through the proxy"
            );
        }

        // `key:0` is falsy in `if (entry.port && ...)`, so it is the wildcard
        // form and bypasses every port of that host.
        let config = ProxyConfig::new("http://proxy:3128", "example.com:0").expect("config");
        for uri in ["http://example.com/", "http://example.com:8080/"] {
            assert!(
                !config.should_proxy(&uri.parse().unwrap()),
                "{uri} must bypass (port 0 wildcard)"
            );
        }

        // Anything failing `/^(.+):(\d+)$/` is part of the hostname: none of
        // these entries can match `example.com`.
        let config = ProxyConfig::new(
            "http://proxy:3128",
            "example.com:-1,example.com:abc,example.com:",
        )
        .expect("config");
        for uri in ["http://example.com/", "http://example.com:8080/"] {
            assert!(
                config.should_proxy(&uri.parse().unwrap()),
                "{uri} must go through the proxy"
            );
        }
    }

    #[test]
    fn proxy_auth_decode_matches_decode_uri_component() {
        let authorization = |url: &str| {
            ProxyConfig::new(url, "").map(|config| config.authorization().unwrap_or_default())
        };
        assert_eq!(
            authorization("http://us%20er:p%40ss@127.0.0.1:3128").as_deref(),
            Some("Basic dXMgZXI6cEBzcw==")
        );
        // `%2F` and a multi-byte UTF-8 sequence decode like
        // decodeURIComponent before Base64.
        assert_eq!(
            authorization("http://u%2Fser:p%C3%A4ss@127.0.0.1:3128").as_deref(),
            Some("Basic dS9zZXI6cMOkc3M=")
        );
        // `%ZZ`, a truncated escape and `%FF` (invalid UTF-8) throw
        // `URIError: URI malformed` in TS, failing the whole install; here the
        // config is absent, so no proxy and no credentials are used.
        assert!(authorization("http://user%ZZ:pass@127.0.0.1:3128").is_none());
        assert!(authorization("http://user%2:pass@127.0.0.1:3128").is_none());
        assert!(authorization("http://user:%FF@127.0.0.1:3128").is_none());
        assert!(authorization("http://user:pass%FF@127.0.0.1:3128").is_none());
    }

    #[test]
    fn connect_target_uses_the_normalized_url_host() {
        let target = |uri: &str| {
            let uri: hyper::Uri = uri.parse().unwrap();
            connect_target(&uri)
        };
        assert_eq!(
            target("http://example.com/"),
            ("example.com:80".into(), "example.com".into())
        );
        // An explicit default port is normalized away from `URL.host`.
        assert_eq!(
            target("http://example.com:80/"),
            ("example.com:80".into(), "example.com".into())
        );
        assert_eq!(
            target("http://example.com:8080/"),
            ("example.com:8080".into(), "example.com:8080".into())
        );
        assert_eq!(
            target("https://example.com/"),
            ("example.com:443".into(), "example.com".into())
        );
        assert_eq!(target("http://[::1]/"), ("[::1]:80".into(), "[::1]".into()));
    }
}
