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
//! audit staging (`options.invocation`, A6.4-pr10). Both provider paths
//! support `PILOTDECK_DUMP_REQUEST` for inspecting outgoing request bodies,
//! on the streaming sends only — TS never dumps from `complete()`.
//! The outgoing HTTP proxy (`cli/proxy.ts`) is implemented here and wired into
//! all provider-side HTTP transports (MCP, model, count, and hook call sites)
//! via [`proxy_https_connector`].
//! MCP clients pass the `pilotdeck.yaml` `proxy` source through [`ProxySettings`];
//! `None` resolves the environment exactly as before.
#![allow(clippy::result_large_err)]

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{Request, Response};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::{TokioExecutor, TokioIo};
use pilotdeck_protocol::{JsObject, JsString, JsValue, js_math::min as js_math_min, parse_json};
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

#[cfg(test)]
use super::count_body::text as header_text;
use super::count_body::{field, is, object, put};
use super::providers::{self, StreamState};
use super::router::AbortFlag;
use crate::host::ModuleError;

mod complete;
pub(crate) mod encoding;
mod errors;
mod google;
mod sse;
mod tls;
pub use complete::complete_model;
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
use google::stream_google_model;

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
/// A response body read failure. `Terminated` is undici's
/// `TypeError: terminated` (socket failure or body idle timeout);
/// `StringTooLong` is the V8 error replay of a drained over-limit shape body
/// on the last candidate (real TS `response.text()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyFailure {
    Terminated,
    StringTooLong,
}
impl std::fmt::Display for BodyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Terminated => f.write_str("terminated"),
            Self::StringTooLong => f.write_str(crate::fetch_redirect::STRING_TOO_LONG),
        }
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
    client: Client<hyper_rustls::HttpsConnector<ProxyConnect>, Full<Bytes>>,
}
impl HyperModelTransport {
    /// Bundled roots plus Node-compatible explicit CA files; see `tls`.
    pub fn new() -> Self {
        Self::from_config(webpki_client_config(), ProxyConfig::from_settings(None))
    }
    /// Trust exactly `roots` (tests use a throwaway CA).
    pub fn with_roots(roots: rustls::RootCertStore) -> Self {
        Self::from_config(
            rooted_client_config(roots),
            ProxyConfig::from_settings(None),
        )
    }
    fn builder() -> rustls::ConfigBuilder<rustls::ClientConfig, rustls::WantsVerifier> {
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
    }
    fn from_config(config: rustls::ClientConfig, proxy: Option<ProxyConfig>) -> Self {
        Self {
            client: Client::builder(TokioExecutor::new())
                .build(proxy_https_connector(config, proxy)),
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
// port keeps the agent semantics and exposes them through [`proxy_https_connector`],
// which all HTTP transports (MCP, model, and hook) now use.
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
// HTTP/HTTPS CONNECT and SOCKS5 (including `socks:`) share this connector.
// A structurally invalid proxy fails closed, including NO_PROXY destinations.
// At startup only, otherwise valid URLs with undecodable userinfo leave the
// direct dispatcher active, matching TS; reload failures still fail closed.
//
// Every HTTP/HTTPS-proxied request, including plain `http:` targets, uses
// `CONNECT` because undici's ProxyAgent defaults to `proxyTunnel: true`.
// Proxy credentials come from the URL userinfo and become
// `Proxy-Authorization: Basic base64(decodeURIComponent(user):decodeURIComponent(password))`.

/// The environment keys the gateway's proxy configuration reads, in lookup
/// order: the five URL candidates first, then the `no_proxy` pair. Single
/// source of this list — `tests/common/proxy_env.rs` re-exports it so the
/// child-process env scrubbing cannot drift from what production reads.
pub const PROXY_ENV_KEYS: [&str; 7] = [
    "PILOTDECK_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "http_proxy",
    "HTTP_PROXY",
    "no_proxy",
    "NO_PROXY",
];

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
    proxy: Option<url::Url>,
    /// The joined value the baseline logs and matches `"*"` against.
    no_proxy: String,
    entries: Vec<NoProxyEntry>,
    /// The CONNECT `Proxy-Authorization` value, built at construction like
    /// `ProxyAgent`'s constructor. `decodeURIComponent` throws on malformed
    /// userinfo, which makes the TS install fail. Keep that error distinct
    /// from invalid URL structure so startup can retain direct transport.
    authorization: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProxyParseError {
    Configuration,
    Userinfo,
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
        let env_url = PROXY_ENV_KEYS
            .iter()
            // The five URL candidates; the `no_proxy` pair is read below.
            .take(5)
            .find_map(|&key| lookup(key).filter(|value| !value.is_empty()));
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
        let user_no_proxy = PROXY_ENV_KEYS
            .iter()
            .skip(5)
            .find_map(|&key| lookup(key).filter(|value| !value.is_empty()))
            .unwrap_or_default();
        let parsed = Self::new_with_extra(&proxy_url, &user_no_proxy, extra_no_proxy.as_deref());
        // TS first tries env, then config at startup. An invalid env install
        // must not prevent a valid config from being installed.
        let parsed = parsed.or_else(|error| {
            let Some(settings) = settings.filter(|settings| {
                settings.mode == ProxyInstallMode::Startup && !settings.url.is_empty()
            }) else {
                return Err(error);
            };
            Self::new_with_extra(&settings.url, &user_no_proxy, None).map_err(|fallback| {
                // A failed fallback must not hide a structural env error (or
                // vice versa). Only userinfo-only startup failures go direct.
                if error == ProxyParseError::Userinfo && fallback == ProxyParseError::Userinfo {
                    ProxyParseError::Userinfo
                } else {
                    ProxyParseError::Configuration
                }
            })
        });
        if matches!(parsed, Err(ProxyParseError::Userinfo))
            && settings.is_none_or(|settings| settings.mode == ProxyInstallMode::Startup)
        {
            return None;
        }
        Some(parsed.unwrap_or_else(|_| Self {
            proxy: None,
            no_proxy: String::new(),
            entries: Vec::new(),
            authorization: None,
        }))
    }

    #[cfg(test)]
    fn new(proxy_url: &str, user_no_proxy: &str) -> Option<Self> {
        Self::new_with_extra(proxy_url, user_no_proxy, None).ok()
    }

    fn new_with_extra(
        proxy_url: &str,
        user_no_proxy: &str,
        extra_no_proxy: Option<&str>,
    ) -> Result<Self, ProxyParseError> {
        let proxy = url::Url::parse(proxy_url).map_err(|_| ProxyParseError::Configuration)?;
        if proxy.host_str().is_none()
            || !matches!(proxy.scheme(), "http" | "https" | "socks5" | "socks")
        {
            return Err(ProxyParseError::Configuration);
        }
        if matches!(proxy.scheme(), "http" | "https")
            && (proxy.path() != "/"
                || proxy.query().is_some_and(|value| !value.is_empty())
                || proxy.fragment().is_some_and(|value| !value.is_empty()))
        {
            return Err(ProxyParseError::Configuration);
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
        for entry in no_proxy
            .split(|character: char| character == ',' || crate::metadata::js_space(character))
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
        let authorization = proxy_authorization(&proxy).map_err(|_| ProxyParseError::Userinfo)?;
        Ok(Self {
            proxy: Some(proxy),
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
        let Some(proxy) = &self.proxy else {
            return (String::new(), 0);
        };
        let host = match proxy.host() {
            Some(url::Host::Ipv6(address)) => address.to_string(),
            Some(url::Host::Ipv4(address)) => address.to_string(),
            Some(url::Host::Domain(name)) => name.to_owned(),
            None => String::new(),
        };
        let port = if matches!(proxy.scheme(), "socks" | "socks5") {
            proxy.port().filter(|port| *port != 0).unwrap_or(1080)
        } else {
            proxy.port_or_known_default().unwrap_or(80)
        };
        (host, port)
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
        tls: Arc<rustls::ClientConfig>,
    ) -> Result<(Box<dyn ProxyIo>, Vec<u8>), ProxyError> {
        let proxy = self.proxy.as_ref().ok_or_else(|| {
            ProxyError::new(
                "Invalid or unsupported proxy configuration; direct connection refused".into(),
            )
        })?;
        let (host, port) = self.host_port();
        let stream = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|error| self.connect_error(error))?;
        let mut stream: Box<dyn ProxyIo> = if proxy.scheme() == "https" {
            let name = rustls::pki_types::ServerName::try_from(host)
                .map_err(|_| ProxyError::new("Invalid HTTPS proxy hostname".into()))?;
            Box::new(
                tokio_rustls::TlsConnector::from(tls)
                    .connect(name, stream)
                    .await
                    .map_err(|_| ProxyError::new("HTTPS proxy TLS handshake failed".into()))?,
            )
        } else {
            Box::new(stream)
        };
        if matches!(proxy.scheme(), "socks5" | "socks") {
            super::proxy_socks::connect(&mut stream, proxy, target)
                .await
                .map_err(|error| ProxyError::new(error.to_string()))?;
            return Ok((stream, Vec::new()));
        }
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
/// the whole proxy install. The caller chooses startup direct fallback or
/// reload fail-closed; malformed credentials are never sent to a proxy.
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
pub(crate) trait ProxyIo:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> ProxyIo for T {}

#[derive(Clone)]
pub(crate) struct ProxyConnect {
    inner: HttpConnector,
    proxy: Option<Arc<ProxyConfig>>,
    tls: Arc<rustls::ClientConfig>,
}
impl tower_service::Service<hyper::Uri> for ProxyConnect {
    type Response = PrefixedStream<TokioIo<Box<dyn ProxyIo>>>;
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
            let tls = self.tls.clone();
            return Box::pin(async move {
                let (stream, prefix) =
                    tokio::time::timeout(Duration::from_secs(10), proxy.tunnel(&destination, tls))
                        .await
                        .map_err(|_| proxy.connect_error(std::io::ErrorKind::TimedOut.into()))?
                        .map_err(|error| {
                            Box::new(error) as Box<dyn std::error::Error + Send + Sync>
                        })?;
                Ok(PrefixedStream::new(TokioIo::new(stream), prefix))
            });
        }
        let direct = self.inner.call(destination);
        Box::pin(async move {
            direct
                .await
                .map(|stream| {
                    PrefixedStream::new(
                        TokioIo::new(Box::new(stream.into_inner()) as Box<dyn ProxyIo>),
                        Vec::new(),
                    )
                })
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
    let tls = Arc::new(config.clone());
    hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_or_http()
        .enable_http1()
        .wrap_connector(ProxyConnect {
            tls,
            inner: http,
            proxy: proxy.map(Arc::new),
        })
}

/// Shared default trust configuration, including explicit extra CA sources.
pub(crate) fn webpki_client_config() -> rustls::ClientConfig {
    tls::client_config()
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
        let method = request.method().clone();
        let follow_redirects = request
            .extensions()
            .get::<crate::fetch_redirect::FollowRedirects>()
            .is_some();
        TransportFuture::io(async move {
            let response = client
                .request(request)
                .await
                .map_err(|error| TransportFailure::fetch_failed(cause_code(&error)))?;
            encoding::decode_response(
                &method,
                response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()),
                follow_redirects,
            )
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
                rustls::Error::InvalidCertificate(
                    rustls::CertificateError::NotValidYet
                    | rustls::CertificateError::NotValidYetContext { .. },
                ) => "CERT_NOT_YET_VALID",
                rustls::Error::InvalidCertificate(
                    rustls::CertificateError::InvalidPurpose
                    | rustls::CertificateError::InvalidPurposeContext { .. },
                ) => "INVALID_PURPOSE",
                rustls::Error::InvalidCertificate(rustls::CertificateError::BadSignature) => {
                    "CERT_SIGNATURE_FAILURE"
                }
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
/// `DEFAULT_REQUEST_TIMEOUT_MS` (`streamModel.ts:714`): the non-streaming
/// fallback when the provider has no `timeoutMs`.
const DEFAULT_REQUEST_TIMEOUT_MS: f64 = 600_000.0;
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
        return (max_retries, js_math_min(after, max_delay));
    }
    let base = retry_number(provider, "baseDelayMs").unwrap_or(500.0);
    let jitter = retry_number(provider, "jitter").unwrap_or(0.75);
    let deterministic = base * (attempt + 1.0);
    (
        max_retries,
        js_math_min(deterministic + deterministic * jitter * random, max_delay),
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
/// `isRetryableStreamError`'s raw-`Error` branch (`streamModel.ts:587-618`).
/// This is the stream layer's list; `fallback::canonical_error` ports the
/// different router-layer `RouterRuntime.isNetworkTransient`
/// (`RouterRuntime.ts:1280-1304`), so dns/etimedout/abort belong there only.
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
    // WebIDL converts the entire record before validating/appending headers.
    let entries = headers
        .iter()
        .map(|(name, value)| {
            let units = crate::js_value::to_string_units(value).map_err(|_| failed())?;
            Ok((
                header_byte_string(name.units())?,
                header_byte_string(&units)?,
            ))
        })
        .collect::<Result<Vec<_>, NetworkFetchError>>()?;
    let mut map = hyper::HeaderMap::new();
    for (name, value) in entries {
        let name = hyper::header::HeaderName::from_bytes(&name).map_err(|_| {
            NetworkFetchError::classify(
                &format!(
                    "Headers.append: \"{}\" is an invalid header name.",
                    name.iter().map(|b| char::from(*b)).collect::<String>()
                ),
                None,
            )
        })?;
        let trim = |b: &u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n');
        let start = value.iter().position(|b| !trim(b)).unwrap_or(value.len());
        let end = value
            .iter()
            .rposition(|b| !trim(b))
            .map_or(start, |i| i + 1);
        let bytes = &value[start..end];
        if bytes.iter().any(|b| matches!(b, 0 | b'\r' | b'\n')) {
            return Err(NetworkFetchError::classify(
                &format!(
                    "Headers.append: \"{}\" is an invalid header value.",
                    bytes.iter().map(|b| char::from(*b)).collect::<String>()
                ),
                None,
            ));
        }
        let value = hyper::header::HeaderValue::from_bytes(bytes).map_err(|_| failed())?;
        if let Some(existing) = map.get(&name) {
            let mut joined = existing.as_bytes().to_vec();
            joined.extend_from_slice(b", ");
            joined.extend_from_slice(value.as_bytes());
            map.insert(
                name,
                hyper::header::HeaderValue::from_bytes(&joined).map_err(|_| failed())?,
            );
        } else {
            map.insert(name, value);
        }
    }
    Ok((parsed, map))
}

fn header_byte_string(units: &[u16]) -> Result<Vec<u8>, NetworkFetchError> {
    units.iter().enumerate().map(|(index, &unit)| {
        u8::try_from(unit).map_err(|_| NetworkFetchError::classify(&format!("Cannot convert argument to a ByteString because the character at index {index} has a value of {unit} which is greater than 255."), None))
    }).collect()
}

/// Preserve each redirect hop's completion source for both provider transports.
/// Errors retain whether their continuation precedes a later parent timer, so
/// Google's outer catch can use the same ordering as a successful response.
async fn provider_exchange(
    transport: &dyn ModelHttpTransport,
    request: Request<Full<Bytes>>,
    options: &StreamOptions,
    deadline: Option<tokio::time::Instant>,
    failure: impl FnOnce(TransportFailure) -> ModelStreamError,
    timeout: impl FnOnce() -> ModelStreamError,
) -> Result<Response<(ModelBody, bool)>, (ModelStreamError, bool)> {
    use crate::runtime::network::{Exchange, exchange_with_prearmed_parent};
    let send = transport.send(request);
    match exchange_with_prearmed_parent(
        send,
        wait_abort(&options.abort),
        || options.abort.as_ref().and_then(AbortFlag::abort_event_at),
        deadline,
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

/// Debug helper that writes the request body to a JSON file under the OS
/// temp directory when `PILOTDECK_DUMP_REQUEST=1`. Matches TS
/// `streamModel.ts:196-203,395-401` in trigger (streaming attempts only),
/// directory and log line. The
/// file name and creation mode intentionally diverge from the TS
/// `Date.now()` / `writeFileSync` pair: a dump carries the whole
/// conversation, so it is created exclusively (never following a
/// pre-existing symlink or truncating an existing file), with owner-only
/// permissions, under a name unique to this process
/// (specs/gateway-parity/diffs/A6.4-MODEL-DUMP-SECURE-001).
///
/// The dumped body is the outgoing wire body (after `extraBody` merging and,
/// for Google, after the SDK's `config` → `generationConfig` projection), not
/// the pre-SDK `buildModelRequest` object TS serializes; the projection
/// divergence is registered in the same entry. A failed write returns the
/// `writeFileSync`-shaped error so callers can surface it like TS.
fn dump_request_to_file(final_body: &JsValue, model: &JsValue) -> Result<(), DumpFailure> {
    // Model name for the log message. The Google path carries its model in
    // the URL, not in the outgoing JSON body, so callers pass it in.
    let model = model
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_else(|| "unknown".to_string());

    // Node ignores SIGXFSZ process-wide, so a dump write past RLIMIT_FSIZE
    // surfaces as EFBIG from `write` instead of killing the process; Rust
    // std ignores only SIGPIPE (probe: without a handler the writing
    // process dies with exit 153 before io::Error reaches user code). This
    // runtime has no process entry of its own, so the first dump registers
    // a tokio SIGXFSZ handler for its host process — the safe signal API,
    // no unsafe, once per process (OnceLock) — and the gateway binary
    // registers it at startup too (`main.rs`). Outside a tokio runtime the
    // registration degrades to a stderr note and the default disposition,
    // no worse than without this guard.
    #[cfg(unix)]
    crate::ensure_sigxfsz_ignored();

    // Format as pretty-printed JSON (TS uses `JSON.stringify(body, null, 2)`).
    let json = match crate::js_value::json_stringify(final_body) {
        Ok(Some(compact)) => {
            // Parse and re-stringify with indentation.
            match serde_json::from_str::<serde_json::Value>(&compact) {
                Ok(value) => serde_json::to_string_pretty(&value).unwrap_or(compact),
                Err(_) => compact,
            }
        }
        _ => return Ok(()), // Failed to serialize; skip the dump.
    };

    // `std::env::temp_dir()` reads TMPDIR alone and answers `""` when it is
    // set but empty, which would drop a dump (the whole conversation) into
    // the process's working directory, so the directory resolves the way its
    // TS counterpart does: `crate::node_io::node_tmpdir`.
    let path = write_dump_exclusively(
        &std::path::PathBuf::from(crate::node_io::node_tmpdir(|key| std::env::var_os(key))),
        &json,
        &mut || {
            next_dump_name(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or_default(),
            )
        },
        8,
    )?;
    println!(
        "[model-debug] Request dumped to {} (model={})",
        path.display(),
        model
    );
    Ok(())
}

/// One dump file name: the caller's clock reading, this process, and a
/// process-wide monotonic sequence keep concurrent dumps from sharing a name.
/// TS uses `Date.now()` alone (`streamModel.ts:199-200`), where
/// same-millisecond requests overwrite each other's dumps.
fn next_dump_name(now_ms: u128) -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(
        "pilotdeck_request_{now_ms}_{}_{}.json",
        std::process::id(),
        sequence
    )
}

/// What a failed dump write observes as, mirroring the `writeFileSync` throw
/// the TS paths surface (`streamModel.ts:196-203,395-401`): Node's `code`,
/// its lowercase errno description, the failing syscall, and the path for
/// the `open '<path>'` argument Node prints.
#[derive(Debug)]
struct DumpFailure {
    code: Option<&'static str>,
    description: Option<String>,
    syscall: &'static str,
    path: Option<std::path::PathBuf>,
    errno: Option<i32>,
    /// Text for the unmapped-errno fallback (candidate exhaustion has no TS
    /// counterpart: `writeFileSync` truncates instead of refusing).
    fallback: String,
}
impl DumpFailure {
    fn from_io(
        error: &std::io::Error,
        syscall: &'static str,
        path: Option<&std::path::Path>,
    ) -> Self {
        // `code` and its lowercase description come from the Node v22 libuv
        // table generated into `node_io` (oracle-tested against Node itself);
        // the platform's strerror text drifts from it (ELOOP, EDQUOT, ...).
        let (code, description) = crate::node_io::lookup(error)
            .map_or((None, None), |(code, description)| {
                (Some(code), Some(description.to_string()))
            });
        Self {
            code,
            description,
            syscall,
            path: path.map(std::path::Path::to_path_buf),
            // Node's fs errors carry libuv's signed errno (uv_errno_t is the
            // negated OS errno: ENOENT surfaces as -2, EACCES as -13).
            errno: error.raw_os_error().map(|raw| -raw),
            fallback: error.to_string(),
        }
    }

    /// Node's message shape: `ENOENT: no such file or directory, open '<path>'`
    /// (the path is an argument of `open` only; a write failure has none).
    fn message(&self) -> String {
        let Some(code) = self.code else {
            return format!(
                "Failed to {} dump file: {}",
                if self.syscall == "write" {
                    "write"
                } else {
                    "create"
                },
                self.fallback
            );
        };
        let description = self.description.as_deref().unwrap_or("unknown error");
        if self.syscall == "open"
            && let Some(path) = &self.path
        {
            format!("{code}: {description}, open '{}'", path.display())
        } else {
            format!("{code}: {description}, {}", self.syscall)
        }
    }
}

/// Writes `contents` under `dir` to the first candidate name from
/// `next_name` that does not already exist (up to `candidates` tries).
/// Creation is exclusive and owner-only: `O_CREAT|O_EXCL` refuses to follow
/// a pre-existing symlink or truncate an existing file, so a predictable
/// name cannot be used to overwrite other data, and owner-only permissions
/// keep the dump unreadable to other users even in a shared temp directory.
/// A partial file from a failed `write_all` is left in place, like TS's
/// `writeFileSync`.
fn write_dump_exclusively(
    dir: &std::path::Path,
    contents: &str,
    next_name: &mut dyn FnMut() -> String,
    candidates: usize,
) -> Result<std::path::PathBuf, DumpFailure> {
    use std::io::Write;
    for _ in 0..candidates {
        let path = dir.join(next_name());
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                return file
                    .write_all(contents.as_bytes())
                    .map(|()| path)
                    .map_err(|error| DumpFailure::from_io(&error, "write", None));
            }
            // A concurrent creator or pre-planted file with this name: move
            // to the next candidate instead of truncating it.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(DumpFailure::from_io(&error, "open", Some(&path))),
        }
    }
    Err(DumpFailure {
        code: None,
        description: None,
        syscall: "open",
        path: None,
        errno: None,
        fallback: "every name candidate already exists".into(),
    })
}

/// Replays the bytes already read for a completeness/shape check, optionally
/// ending with the body failure the caller's `response.text()` must observe. A
/// successful 2xx candidate must be handed back byte-for-byte, a failed read on
/// the last candidate still fails the way the tee'd undici body does, and a
/// drained over-limit body reports V8's plain `Error`.
struct ReplayBody {
    data: Option<Bytes>,
    tail: Option<BodyFailure>,
}

impl hyper::body::Body for ReplayBody {
    type Data = Bytes;
    type Error = BodyFailure;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, BodyFailure>>> {
        let this = self.as_mut().get_mut();
        if let Some(data) = this.data.take() {
            return Poll::Ready(Some(Ok(hyper::body::Frame::data(data))));
        }
        if let Some(tail) = this.tail.take() {
            return Poll::Ready(Some(Err(tail)));
        }
        Poll::Ready(None)
    }
}

fn replay_body(bytes: Bytes, tail: Option<BodyFailure>) -> ModelBody {
    ReplayBody {
        data: (!bytes.is_empty()).then_some(bytes),
        tail,
    }
    .boxed()
}

fn replay_response(
    parts: hyper::http::response::Parts,
    bytes: Bytes,
    tail: Option<BodyFailure>,
    before_abort: bool,
) -> Response<(ModelBody, bool)> {
    Response::from_parts(parts, (replay_body(bytes, tail), before_abort))
}

/// What a non-stream, multi-endpoint 2xx shape probe observed. Only stream
/// calls and single-endpoint calls skip this. The body read starts after the
/// response headers arrived; at that point `networkFetch` has returned and
/// detached its parent-abort listener, so neither `sendProviderRequest`'s
/// request timer nor a caller abort can stop it. Only undici's body idle
/// timeout / a socket failure does (r4b real-TS probe).
enum ShapeProbe {
    /// The whole body arrived. `parsed` is `None` for empty/invalid JSON, which
    /// `Response.json()` rejects and therefore treats as the wrong shape.
    Complete {
        bytes: Bytes,
        parsed: Option<JsValue>,
    },
    /// A body frame failed (undici `TypeError: terminated`) or the body idle
    /// timeout fired; the caller must keep the prefix for a possible last
    /// candidate.
    ReadError { bytes: Bytes },
    /// The body's received bytes passed V8's `MAX_STRING_LENGTH`. TS
    /// `copy.json()` reads the whole clone to EOF, then the byte→string
    /// conversion throws a plain `Error`; the candidate is rejected. The
    /// original body is drained and discarded, so the last candidate replays
    /// that error instead of retaining 512 MiB.
    StringTooLong,
}

/// Which outer event aborts the next endpoint fetch after a rejected shape.
/// `sendProviderRequest`'s catch rechecks `signal.aborted` and throws the
/// caller's abort reason even when the timeout fired first (`streamModel.ts:
/// 748-753`), so a parent abort that happened while the body was draining wins
/// regardless of deadline ordering. Only when the parent is still un-aborted is
/// an elapsed deadline reported as the timeout.
enum HeaderAbort {
    Timer,
    Parent,
}

fn shape_fallback_abort(
    options: &StreamOptions,
    deadline: Option<tokio::time::Instant>,
) -> Option<HeaderAbort> {
    if options.abort.as_ref().is_some_and(AbortFlag::is_aborted) {
        return Some(HeaderAbort::Parent);
    }
    deadline
        .is_some_and(|at| tokio::time::Instant::now() >= at)
        .then_some(HeaderAbort::Timer)
}

/// The byte→string conversion half shared by `read_shape_probe` and
/// `response_text`: both convert the body under V8's `MAX_STRING_LENGTH`,
/// which counts the received UTF-8 bytes — not the decoded UTF-16 units
/// (node v22.22 real-TS probe, `_tmp/20261025-issue-647-probe/RESULT.md`) —
/// and only throws after the body reached EOF, so an over-limit text keeps
/// draining while its accumulated value is dropped.
struct CappedText {
    decoder: sse::Utf8Decoder,
    text: String,
    bytes: usize,
    too_long: bool,
    #[cfg(test)]
    converted_bytes: usize,
}

impl CappedText {
    fn new() -> Self {
        Self {
            decoder: sse::Utf8Decoder::default(),
            text: String::new(),
            bytes: 0,
            too_long: false,
            #[cfg(test)]
            converted_bytes: 0,
        }
    }

    /// Feed one body frame. Returns `false` once the text can only be
    /// rejected: the conversion has not run yet, so the caller keeps
    /// draining to EOF — where a body failure still wins — but retains
    /// none of the over-limit prefix.
    fn push(&mut self, data: &[u8]) -> bool {
        if self.too_long {
            return false;
        }
        // The cap counts the frame's raw bytes, including the bytes of a
        // trailing incomplete UTF-8 sequence that only becomes U+FFFD at
        // EOF; exactly the limit is still allowed.
        self.bytes += data.len();
        if self.bytes > string_limit() {
            self.too_long = true;
            self.text = String::new();
            return false;
        }
        self.decode(data);
        true
    }

    fn decode(&mut self, data: &[u8]) {
        // Observe the actual text conversion entry, not merely the cap
        // decision: a late cap check must fail the allocation-order test.
        #[cfg(test)]
        {
            self.converted_bytes += data.len();
        }
        self.decoder.decode(data, &mut self.text);
    }

    /// EOF: every received byte was already counted frame by frame, so the
    /// cap verdict cannot change here; `finish` only completes a trailing
    /// incomplete UTF-8 sequence for the accepted text. `Err` is the
    /// over-limit outcome; each reader maps it to its own error shape.
    fn finish(mut self) -> Result<String, ()> {
        self.decoder.finish(&mut self.text);
        if self.too_long {
            return Err(());
        }
        Ok(self.text)
    }
}

async fn read_shape_probe(body: &mut ModelBody, options: &StreamOptions) -> ShapeProbe {
    use crate::runtime::network::{Exchange, body_frame};
    let mut capped = CappedText::new();
    let mut bytes = Vec::new();
    let mut continuation = false;
    loop {
        // A fresh undici `bodyTimeout` per frame, with no request deadline and
        // no caller abort: both were detached from this body when the headers
        // arrived, and TS still reads the clone to the end (probe r4b).
        let frame = body_frame(
            body,
            Some(duration(options.body_timeout_ms)),
            None,
            &mut continuation,
        )
        .await;
        match frame {
            Exchange::Response(Some(Ok(frame)), _) => {
                if let Some(data) = frame.data_ref() {
                    if capped.push(data) {
                        bytes.extend_from_slice(data);
                    } else {
                        // `copy.json()` reads the body to EOF before the
                        // byte→string conversion throws; do not fall back
                        // early, and retain none of the rejected candidate.
                        bytes = Vec::new();
                    }
                }
            }
            Exchange::Response(Some(Err(_)), _) | Exchange::Timeout { .. } => {
                return ShapeProbe::ReadError {
                    bytes: Bytes::from(bytes),
                };
            }
            Exchange::Response(None, _) => break,
            // `None` above means there is no abort source; keep the prefix
            // rather than inventing an abort result.
            Exchange::Aborted => {
                return ShapeProbe::ReadError {
                    bytes: Bytes::from(bytes),
                };
            }
        }
    }
    match capped.finish() {
        Err(()) => ShapeProbe::StringTooLong,
        Ok(text) => ShapeProbe::Complete {
            bytes: Bytes::from(bytes),
            parsed: parse_json(&text).ok(),
        },
    }
}

/// `sendProviderRequest` + `sendWithEndpointFallback` + `networkFetch`.
///
/// `stream` selects the baseline's timeout: streaming resolves
/// `resolveStreamIdleTimeout` and reports `Stream idle timeout…`; `complete()`
/// uses `provider.timeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS`, ignores
/// `streamTimeoutMs`, and reports `Model request timed out after <ms>ms.`
/// (`streamModel.ts:716-755`).
///
/// The timer bounds every endpoint's *headers* exchange. A 2xx shape body read
/// starts after the headers arrived, when `networkFetch` has already detached
/// its parent-abort listener; the remaining clone/`json()` read is therefore
/// not bounded by this timer (real-TS r4b probe). It still decides whether the
/// *next* endpoint may be tried: once the timer or caller abort fired, the
/// outer controller is aborted and the next `networkFetch` rejects before any
/// request is sent. `response.text()` runs after this function returns, as in
/// the baseline.
async fn send_provider_request(
    transport: &dyn ModelHttpTransport,
    provider: &JsValue,
    body: &JsValue,
    options: &StreamOptions,
    stream: bool,
) -> Result<Response<(ModelBody, bool)>, ModelStreamError> {
    let (timeout_ms, timeout_text) = if stream {
        let ms = idle_timeout_ms(provider, options);
        (
            ms,
            format!(
                "Stream idle timeout: no data received for {}ms",
                crate::js_value::number_string(ms)
            ),
        )
    } else {
        let ms = field(provider, "timeoutMs")
            .as_number()
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
        (
            ms,
            format!(
                "Model request timed out after {}ms.",
                crate::js_value::number_string(ms)
            ),
        )
    };
    let final_body = providers::final_body(body, provider);
    let headers = providers::provider_headers(provider, &final_body);
    let encoded = crate::js_value::json_stringify(&final_body)
        .map_err(|_| {
            ModelStreamError::thrown("RangeError", crate::js_value::StackOverflow::MESSAGE)
        })?
        .unwrap_or_default();

    // PILOTDECK_DUMP_REQUEST: debug aid that writes the provider request body
    // to a file under the OS temp directory. TS dumps only inside the stream
    // loop (`streamModel.ts:196-203`); `complete()` calls `sendProviderRequest`
    // directly without it (`:124`), so the non-streaming path stays cold.
    if stream && std::env::var("PILOTDECK_DUMP_REQUEST").as_deref() == Ok("1") {
        // TS throws the raw fs Error out of the generator here (the dump
        // sits outside the send try). Swallowing the failure and continuing
        // to send is this port's pre-existing behavior (already in main,
        // unchanged by this candidate's diff); registered in
        // A6.4-MODEL-DUMP-SECURE-001.
        if let Err(failure) = dump_request_to_file(&final_body, field(body, "model")) {
            eprintln!(
                "[model-debug] Failed to write request dump: {}",
                failure.message()
            );
        }
    }

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
    // TS `const timeout = effectiveTimeoutMs ? setTimeout(...) : undefined`
    // treats 0/NaN as "no timer"; a negative value schedules Node's 1 ms timer.
    let deadline = (!timeout_ms.is_nan() && timeout_ms != 0.0)
        .then(|| tokio::time::Instant::now() + duration(timeout_ms));
    let abort_error = |abort: HeaderAbort| match abort {
        HeaderAbort::Parent => ModelStreamError::abort(),
        HeaderAbort::Timer => to_provider(NetworkFetchError::new(
            "network_timeout",
            timeout_text.clone(),
            JsValue::Undefined,
        )),
    };
    let mut last = None;
    for (index, endpoint) in endpoints.iter().enumerate() {
        let last_candidate = index + 1 == count;
        let (url, header_map) = build_http_request(endpoint, &headers).map_err(to_provider)?;
        let response = crate::fetch_redirect::follow(
            url,
            hyper::Method::POST,
            header_map,
            encoded.clone(),
            || to_provider(NetworkFetchError::classify("fetch failed", None)),
            |request| async {
                let timeout_text = timeout_text.clone();
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
                            timeout_text,
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
        if ok && !stream && count > 1 {
            // TS `shouldUseEndpointResponse`: clone and parse the 2xx body
            // after headers. The read itself is detached from the request
            // timer, but a rejection moves to the next candidate only while
            // the outer controller is still un-aborted; the final candidate is
            // returned even when its shape is wrong or its read failed.
            let (parts, (mut body, before_abort)) = response.into_parts();
            match read_shape_probe(&mut body, options).await {
                ShapeProbe::Complete { bytes, parsed } => {
                    let accepted = parsed.as_ref().is_some_and(|value| {
                        providers::expected_complete_response_shape(
                            field(provider, "protocol"),
                            value,
                        )
                    });
                    let response = replay_response(parts, bytes, None, before_abort);
                    if accepted {
                        return Ok(response);
                    }
                    if !last_candidate && let Some(abort) = shape_fallback_abort(options, deadline)
                    {
                        return Err(abort_error(abort));
                    }
                    last = Some(response);
                    continue;
                }
                ShapeProbe::ReadError { bytes } => {
                    if !last_candidate && let Some(abort) = shape_fallback_abort(options, deadline)
                    {
                        return Err(abort_error(abort));
                    }
                    last = Some(replay_response(
                        parts,
                        bytes,
                        Some(BodyFailure::Terminated),
                        before_abort,
                    ));
                    continue;
                }
                ShapeProbe::StringTooLong => {
                    // The body was already drained to EOF above; only now may
                    // the rejected shape try the next candidate, and the last
                    // candidate must surface V8's plain `Error` from text().
                    if !last_candidate && let Some(abort) = shape_fallback_abort(options, deadline)
                    {
                        return Err(abort_error(abort));
                    }
                    last = Some(replay_response(
                        parts,
                        Bytes::new(),
                        Some(BodyFailure::StringTooLong),
                        before_abort,
                    ));
                    continue;
                }
            }
        }
        return Ok(response);
    }
    last.ok_or_else(|| to_provider(NetworkFetchError::classify("fetch failed", None)))
}

/// `await response.text()`: UTF-8 with BOM stripped.
///
/// The byte→string conversion runs only after the body reached EOF, exactly
/// like TS `Response.text()`: an over-`MAX_STRING_LENGTH` body is still read
/// to the end (real TS probes, `_tmp/20261025-issue-641/RESULT.md`), so a
/// mid-read body failure keeps priority over the string error. The cap
/// counts the received UTF-8 bytes, not the decoded UTF-16 units (node
/// v22.22 real-TS probe, `_tmp/20261025-issue-647-probe/RESULT.md`); a
/// trailing incomplete UTF-8 sequence becomes one U+FFFD in the returned
/// text, and its bytes were already counted when received. The conversion
/// throws V8's plain `Error`, never a `RangeError`.
async fn response_text(
    body: &mut ModelBody,
    options: &StreamOptions,
    mut continuation: bool,
) -> Result<String, ModelStreamError> {
    // TS reads this body after the request timer was cleared and the abort
    // listener detached; only undici's `bodyTimeout` bounds a stalled body
    // (node probe: `TypeError: terminated`, cause UND_ERR_BODY_TIMEOUT).
    // Google wraps this body read in its still-pending SDK promise below.
    let mut capped = CappedText::new();
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
            Exchange::Timeout { .. }
            | Exchange::Response(Some(Err(BodyFailure::Terminated)), _) => {
                return Err(ModelStreamError::thrown("TypeError", "terminated"));
            }
            // The drained over-limit shape body on the last candidate: real TS
            // `response.text()` throws V8's plain `Error`, not `RangeError`
            // (probe `probe-shape-string-limit-last.mts`).
            Exchange::Response(Some(Err(BodyFailure::StringTooLong)), _) => {
                return Err(ModelStreamError::thrown(
                    "Error",
                    crate::fetch_redirect::STRING_TOO_LONG,
                ));
            }
            Exchange::Response(None, _) => break,
            Exchange::Response(Some(Ok(frame)), _) => {
                if let Some(data) = frame.data_ref() {
                    // The string conversion has not run yet; `push` keeps
                    // draining past the cap so EOF / the body idle timeout
                    // still decide and a body failure keeps priority over
                    // the string error.
                    capped.push(data);
                }
            }
        }
    }
    capped
        .finish()
        .map_err(|()| ModelStreamError::thrown("Error", crate::fetch_redirect::STRING_TOO_LONG))
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
    // Equal durations favor the native idle watchdog. Undici's HTTP parser
    // uses a coarse fast timer for bodyTimeout (499 ms ticks, including short
    // nonzero durations). Its phase and event-loop lag are not recoverable
    // from these two durations: do not add a fixed 499/1000 ms correction.
    // #411 TCP probes: body=5000, idle=5100/5300 can still produce idle while
    // this duration comparison produces body. Parity scenarios use equality
    // or >=1000 ms separation; this is a fixture constraint, not a guarantee
    // under arbitrary scheduler stalls. Unaccepted window and raw evidence:
    // specs/gateway-parity/diffs/A6.4-PROVIDER-IDLE-001.md.
    let idle_deadline = tokio::time::Instant::now() + duration(idle_ms);
    loop {
        let remaining = idle_deadline.saturating_duration_since(tokio::time::Instant::now());
        let body_idle = duration(options.body_timeout_ms);
        let body_first = body_idle < remaining;
        let wait = body_idle.min(remaining);
        use crate::runtime::network::{Exchange, body_frame_with_prearmed_parent};
        match body_frame_with_prearmed_parent(
            body,
            Some(wait),
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
                if let Ok(data) = frame.into_data()
                    && !data.is_empty()
                {
                    return Ok(Some(data));
                }
            }
        }
    }
}

/// V8's string length limit (`MAX_STRING_LENGTH`), shared by every
/// cross-event string accumulator of the stream path and the non-stream
/// shape probe.
///
/// There is one function and one default tail expression for both builds. A
/// test only overrides it explicitly through [`LimitGuard`]; with no override
/// the test build runs the same `count_http::MAX_STRING_LENGTH` expression as
/// production. The production build does not compile the override branch.
pub(crate) fn string_limit() -> usize {
    #[cfg(test)]
    {
        if let Some(limit) = STRING_LIMIT.with(std::cell::Cell::get) {
            return limit;
        }
    }
    super::count_http::MAX_STRING_LENGTH
}
#[cfg(test)]
thread_local! {
    /// `None` (the default) means "use the shared V8 limit"; only
    /// [`LimitGuard`] sets a shrunken test limit, and nested guards restore the
    /// previous value on drop.
    pub(crate) static STRING_LIMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}
/// Restores a test's thread-local limit even if its assertion panics.
#[cfg(test)]
pub(crate) struct LimitGuard(Option<usize>);
#[cfg(test)]
impl LimitGuard {
    pub(crate) fn set(limit: usize) -> Self {
        Self(STRING_LIMIT.with(|cell| cell.replace(Some(limit))))
    }
}
#[cfg(test)]
impl Drop for LimitGuard {
    fn drop(&mut self) {
        STRING_LIMIT.with(|cell| cell.set(self.0));
    }
}
/// The SSE reader's own `buffer += ...` bound, distinct from the accumulator
/// [`string_limit`]: only its undelimited remainder accumulates here.
///
/// There is one function and one default tail expression for both builds. A
/// test only overrides it explicitly through [`SseLimitGuard`]; with no
/// override the test build runs the same `count_http::MAX_STRING_LENGTH`
/// expression as production. The production build does not compile the
/// override branch.
fn sse_limit() -> usize {
    #[cfg(test)]
    {
        if let Some(limit) = SSE_LIMIT.with(std::cell::Cell::get) {
            return limit;
        }
    }
    super::count_http::MAX_STRING_LENGTH
}
#[cfg(test)]
thread_local! {
    /// `None` (the default) means "use the shared V8 limit"; a separate knob
    /// so accumulator tests can keep the SSE buffer unbounded while
    /// [`LimitGuard`] shrinks `string_limit`. Only [`SseLimitGuard`] sets an
    /// override, and nested guards restore the previous value on drop.
    pub(crate) static SSE_LIMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}
/// Restores a test's SSE limit even if its assertion panics.
#[cfg(test)]
pub(crate) struct SseLimitGuard(Option<usize>);
#[cfg(test)]
impl SseLimitGuard {
    pub(crate) fn set(limit: usize) -> Self {
        Self(SSE_LIMIT.with(|cell| cell.replace(Some(limit))))
    }
}
#[cfg(test)]
impl Drop for SseLimitGuard {
    fn drop(&mut self) {
        SSE_LIMIT.with(|cell| cell.set(self.0));
    }
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

        let response = match send_provider_request(transport, &provider, &body, options, true).await
        {
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
#[path = "http/complete_tests.rs"]
mod complete_tests;

#[cfg(test)]
#[path = "http/google_tests.rs"]
mod google_tests;

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
        // A failed installation stays configured and fails closed (#676).
        let invalid = ProxyConfig::from_lookup(lookup(&[("HTTP_PROXY", "127.0.0.1:1")])).unwrap();
        assert!(invalid.proxy.is_none());
        assert!(invalid.should_proxy(&"http://127.0.0.1/".parse().unwrap()));
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

    #[tokio::test]
    async fn invalid_proxy_never_opens_even_a_no_proxy_target() {
        use tower_service::Service;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = format!("http://{}/", listener.local_addr().unwrap());
        for url in [
            "not a url",
            "ftp://127.0.0.1:9",
            "http://127.0.0.1:9/path",
            "https://127.0.0.1:9/?q",
            "http://127.0.0.1:9/#fragment",
        ] {
            for mode in [ProxyInstallMode::Startup, ProxyInstallMode::Reload] {
                let settings = ProxySettings {
                    url: url.into(),
                    mode,
                    no_proxy: Some("*".into()),
                };
                let proxy = ProxyConfig::from_settings_lookup(Some(&settings), |_| None);
                let mut connector = proxy_https_connector(webpki_client_config(), proxy);
                let result = connector.call(target.parse().unwrap()).await;
                let error = match result {
                    Ok(_) => panic!("invalid proxy opened target: {url}"),
                    Err(error) => error,
                };
                assert!(
                    error
                        .to_string()
                        .contains("Invalid or unsupported proxy configuration"),
                    "{url}: {error}"
                );
            }
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn proxy_userinfo_install_paths_replay_ts_matrix() {
        use tower_service::Service;
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp-proxy-schemes/userinfo-oracle.json"
        ))
        .unwrap();
        assert_eq!(
            oracle["sourceCommit"],
            "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
        );
        let cases = oracle["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 61);
        for case in cases {
            let id = case["id"].as_str().unwrap();
            let fill = |value: &str| value.replace("<proxy>", "127.0.0.1:9");
            let settings = case["config"].as_str().map(|url| ProxySettings {
                url: fill(url),
                no_proxy: None,
                mode: if case["mode"] == "reload" {
                    ProxyInstallMode::Reload
                } else {
                    ProxyInstallMode::Startup
                },
            });
            let proxy = ProxyConfig::from_settings_lookup(settings.as_ref(), |key| {
                (key == case["envKey"].as_str().unwrap_or("PILOTDECK_PROXY"))
                    .then(|| case["env"].as_str().map(fill))
                    .flatten()
            });
            // Explicit differences from the recorded TS behavior, documented
            // in A6.5-MCP-PROXY-FALLBACK-001-R1: structural failures and reload
            // must fail closed. Neither can hide behind a userinfo error.
            let fail_closed = matches!(
                id,
                "bad-env-structural-config"
                    | "structural-env-bad-config"
                    | "valid-env-bad-reload"
                    | "bad-userinfo-invalid-path"
                    | "bad-userinfo-unsupported"
            );
            if fail_closed {
                assert!(proxy.as_ref().is_some_and(|p| p.proxy.is_none()), "{id}");
            } else if case["ts"]["state"] == "proxy" {
                let expected =
                    url::Url::parse(&fill(case["ts"]["proxyUrl"].as_str().unwrap())).unwrap();
                assert_eq!(
                    proxy.as_ref().and_then(|p| p.proxy.as_ref()),
                    Some(&expected),
                    "{id}"
                );
            } else {
                assert!(proxy.is_none(), "{id}: TS kept direct transport");
            }

            // Exercise the production connector, not just the parsing seam.
            // A loopback destination still catches accidentally fail-closed
            // userinfo and accidentally direct structural/reload failures.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = format!("http://{}/", listener.local_addr().unwrap());
            let mut connector = proxy_https_connector(webpki_client_config(), proxy);
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                connector.call(target.parse().unwrap()),
            )
            .await
            .expect("local connector deadline");
            if fail_closed {
                let error = match result {
                    Ok(_) => panic!("{id}: rejected configuration opened target"),
                    Err(error) => error,
                };
                assert!(
                    error
                        .to_string()
                        .contains("Invalid or unsupported proxy configuration"),
                    "{id}: {error}"
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), listener.accept())
                        .await
                        .is_err(),
                    "{id}: target received connection"
                );
            } else {
                assert!(result.is_ok(), "{id}: TS direct connection must succeed");
                tokio::time::timeout(Duration::from_secs(2), listener.accept())
                    .await
                    .expect("target receipt deadline")
                    .unwrap();
            }
        }
    }

    #[tokio::test]
    async fn proxy_handshake_has_a_whole_connect_deadline() {
        use tower_service::Service;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut connector = proxy_https_connector(
            webpki_client_config(),
            ProxyConfig::new(&format!("http://{address}"), ""),
        );
        let request =
            tokio::spawn(async move { connector.call("http://mcp.test/".parse().unwrap()).await });
        let (_stalled, _) = listener.accept().await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(10)).await;
        let result = tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .expect("proxy handshake deadline missing")
            .unwrap();
        let error = match result {
            Ok(_) => panic!("stalled proxy connected"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("Connect Timeout Error"),
            "{error}"
        );
    }

    #[test]
    fn proxy_scheme_defaults_and_invalid_origins() {
        for (url, port) in [
            ("http://proxy", 80),
            ("http://proxy:0", 0),
            ("https://proxy", 443),
            ("https://proxy:0", 0),
            ("socks5://proxy", 1080),
            ("socks://proxy:0", 1080),
        ] {
            assert_eq!(
                ProxyConfig::new(url, "").unwrap().host_port().1,
                port,
                "{url}"
            );
        }
        for url in [
            "http://proxy/path",
            "https://proxy/?q",
            "http://proxy/#frag",
            "ftp://proxy",
            "not a url",
        ] {
            assert!(ProxyConfig::new(url, "").is_none(), "{url}");
        }
        for url in [
            "http://proxy/?#",
            "socks5://proxy/path",
            "socks://proxy/?q#frag",
        ] {
            assert!(ProxyConfig::new(url, "").is_some(), "{url}");
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

#[cfg(test)]
#[path = "http/encoding_tests.rs"]
mod encoding_tests;

#[cfg(test)]
#[path = "../../../tests/fixtures/provider-http-boundaries/proxy.rs"]
mod boundary_proxy_tests;
