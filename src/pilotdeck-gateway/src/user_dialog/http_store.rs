//! HTTP transport for the complete Gateway user-dialog store protocol.
//!
//! TS source: `resources/PilotDeck/src/gateway/user-dialog/HttpGatewayUserDialogStore.ts`
//! at `cc7c4e6` (`HttpGatewayUserDialogStore` :61, `createGatewayUserDialogStoreHttpHandler`
//! :186, `startGatewayUserDialogStoreHttpServer` :232, request/response validation and
//! `HttpStoreValidationError` :571).
//!
//! Values stay in the JS boundary model because every request and response body is
//! replayed as `JSON.stringify` bytes: property order, JS numbers and `undefined`
//! (dropped in objects, `null` in arrays) all have to match. The handler is
//! framework-neutral and synchronous like the file store it fronts; the client and
//! the Node-compatible server are async and live behind `HttpStoreTransport` so
//! tests can replay real TS recordings without a socket.

use super::{
    DialogAnswer, DialogError, FileGatewayUserDialogStore, LeaseClaim, OwnerClaim, Result,
    StoreKey, StoredAnswer, StoredDialog, UserDialogStore,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode,
    body::{Body, Frame, Incoming, SizeHint},
    header,
    server::conn::http1,
    service::service_fn,
};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo, TokioTimer},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    convert::Infallible,
    fmt,
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, watch},
    task::JoinSet,
};
use url::Url;

pub const HTTP_STORE_VERSION: f64 = 1.0;
pub const DEFAULT_TIMEOUT_MS: f64 = 10_000.0;
pub const STORE_PATH_PREFIX: &str = "/v1/gateway-user-dialog-store/";

/// Node `http.createServer` default `headersTimeout` (v22.22.0: 60000 ms).
const DEFAULT_HEADERS_TIMEOUT: Duration = Duration::from_secs(60);
/// Node `http.createServer` default `requestTimeout` (v22.22.0: 300000 ms).
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Node `http.createServer` default `keepAliveTimeout` (v22.22.0: 5000 ms).
const DEFAULT_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Node `server.keepAliveTimeoutBuffer` default (v22.22.0: 1000 ms). Node arms
/// the idle socket timeout with `keepAliveTimeout + keepAliveTimeoutBuffer`, so
/// an idle keep-alive connection actually closes after 6 s; the advertised
/// `Keep-Alive: timeout=` value is still `keepAliveTimeout`.
const DEFAULT_KEEP_ALIVE_TIMEOUT_BUFFER: Duration = Duration::from_secs(1);
/// Node `http.maxHeaderSize` default (v22.22.0: 16384 bytes). hyper's
/// `max_buf_size` is also its read-buffer cap and replays Node's 431.
const DEFAULT_MAX_HEADER_BYTES: usize = 16 * 1024;
/// The TS server has no body cap (`serveNodeRequest` collects every chunk), so
/// this is a deliberate resource bound rather than a parity value; it is
/// registered as an unapproved difference. 2 MiB clears the real TS fixture's
/// largest legitimate request (`huge-valid-json`, 1_048_709 bytes) with room to
/// spare while still bounding one connection's memory.
const DEFAULT_MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Node itself has no connection cap; this only bounds per-process resources.
const DEFAULT_MAX_CONNECTIONS: usize = 64;

const CONTENT_TYPE: &str = "application/json; charset=utf-8";
const TIMEOUT_REASON: &str = "gateway_user_dialog_store_timeout";
const INVALID_URL: &str = "Invalid URL";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Put,
    List,
    Remove,
    Clear,
    ListLive,
    ClaimLive,
    ReleaseLive,
    SubmitLiveAnswer,
    TakeLiveAnswer,
    ClaimLiveOwner,
    RenewLiveOwner,
    ReleaseLiveOwner,
}
impl Operation {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "put" => Self::Put,
            "list" => Self::List,
            "remove" => Self::Remove,
            "clear" => Self::Clear,
            "listLive" => Self::ListLive,
            "claimLive" => Self::ClaimLive,
            "releaseLive" => Self::ReleaseLive,
            "submitLiveAnswer" => Self::SubmitLiveAnswer,
            "takeLiveAnswer" => Self::TakeLiveAnswer,
            "claimLiveOwner" => Self::ClaimLiveOwner,
            "renewLiveOwner" => Self::RenewLiveOwner,
            "releaseLiveOwner" => Self::ReleaseLiveOwner,
            _ => return None,
        })
    }
    fn name(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::List => "list",
            Self::Remove => "remove",
            Self::Clear => "clear",
            Self::ListLive => "listLive",
            Self::ClaimLive => "claimLive",
            Self::ReleaseLive => "releaseLive",
            Self::SubmitLiveAnswer => "submitLiveAnswer",
            Self::TakeLiveAnswer => "takeLiveAnswer",
            Self::ClaimLiveOwner => "claimLiveOwner",
            Self::RenewLiveOwner => "renewLiveOwner",
            Self::ReleaseLiveOwner => "releaseLiveOwner",
        }
    }
}

// ---------------------------------------------------------------------------
// Backend boundary (the twelve methods of the TS `LiveGatewayUserDialogStore`)
// ---------------------------------------------------------------------------

/// Synchronous store boundary required by the HTTP service. TS checks that every
/// method exists when the handler is created; a Rust trait object always has all
/// twelve, so construction cannot fail on a partial implementation.
///
/// This is the same TS `GatewayUserDialogStore` interface as the channel's
/// [`UserDialogStore`], with every optional method required (`Required<...>`).
/// The two shared methods live on the supertrait instead of being forwarded
/// twice, so a `LiveGatewayUserDialogStore` can be handed to the channel
/// directly.
pub trait LiveGatewayUserDialogStore: UserDialogStore {
    fn list(&self, key: &StoreKey) -> Result<Vec<StoredDialog>>;
    fn clear(&self, key: &StoreKey) -> Result<()>;
    fn list_live(&self, key: &StoreKey) -> Result<Vec<StoredDialog>>;
    fn claim_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        ttl_ms: f64,
        lease_id: Option<&JsString>,
    ) -> Result<LeaseClaim>;
    fn release_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: &JsString,
    ) -> Result<bool>;
    fn submit_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: Option<&JsString>,
        result: &DialogAnswer,
    ) -> Result<bool>;
    fn take_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
    ) -> Result<Option<StoredAnswer>>;
    fn claim_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> Result<OwnerClaim>;
    fn renew_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> Result<bool>;
    fn release_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
    ) -> Result<bool>;
}

impl LiveGatewayUserDialogStore for FileGatewayUserDialogStore {
    fn list(&self, key: &StoreKey) -> Result<Vec<StoredDialog>> {
        FileGatewayUserDialogStore::list(self, key)
    }
    fn clear(&self, key: &StoreKey) -> Result<()> {
        FileGatewayUserDialogStore::clear(self, key)
    }
    fn list_live(&self, key: &StoreKey) -> Result<Vec<StoredDialog>> {
        FileGatewayUserDialogStore::list_live(self, key)
    }
    fn claim_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        ttl_ms: f64,
        lease_id: Option<&JsString>,
    ) -> Result<LeaseClaim> {
        FileGatewayUserDialogStore::claim_live(self, key, request_id, ttl_ms, lease_id)
    }
    fn release_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: &JsString,
    ) -> Result<bool> {
        FileGatewayUserDialogStore::release_live(self, key, request_id, lease_id)
    }
    fn submit_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: Option<&JsString>,
        result: &DialogAnswer,
    ) -> Result<bool> {
        FileGatewayUserDialogStore::submit_live_answer(self, key, request_id, lease_id, result)
    }
    fn take_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
    ) -> Result<Option<StoredAnswer>> {
        FileGatewayUserDialogStore::take_live_answer(self, key, request_id)
    }
    fn claim_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> Result<OwnerClaim> {
        FileGatewayUserDialogStore::claim_live_owner(self, key, request_id, owner_id, ttl_ms)
    }
    fn renew_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> Result<bool> {
        FileGatewayUserDialogStore::renew_live_owner(self, key, request_id, owner_id, ttl_ms)
    }
    fn release_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
    ) -> Result<bool> {
        FileGatewayUserDialogStore::release_live_owner(self, key, request_id, owner_id)
    }
}

// ---------------------------------------------------------------------------
// Validation (TS `HttpStoreValidationError` and the `required*` helpers)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpStoreValidationError {
    code: String,
    message: String,
}
impl HttpStoreValidationError {
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}
fn validation(code: &str, message: impl Into<String>) -> HttpStoreValidationError {
    HttpStoreValidationError {
        code: code.into(),
        message: message.into(),
    }
}
fn required_record<'a>(
    value: Option<&'a JsValue>,
    label: &str,
) -> std::result::Result<&'a JsObject, HttpStoreValidationError> {
    match value {
        Some(JsValue::Object(object)) => Ok(object),
        _ => Err(validation(
            "invalid_request",
            format!("{label} must be an object."),
        )),
    }
}
fn required_string<'a>(
    value: Option<&'a JsValue>,
    label: &str,
) -> std::result::Result<&'a JsString, HttpStoreValidationError> {
    match value {
        Some(JsValue::String(text))
            if !crate::runtime::memory::js_trim_units(text.units()).is_empty() =>
        {
            Ok(text)
        }
        _ => Err(validation(
            "invalid_request",
            format!("{label} must be a non-empty string."),
        )),
    }
}
fn optional_string(
    value: Option<&JsValue>,
    label: &str,
) -> std::result::Result<Option<JsString>, HttpStoreValidationError> {
    match value {
        None | Some(JsValue::Undefined) => Ok(None),
        value => Ok(Some(required_string(value, label)?.clone())),
    }
}
fn positive_integer(
    value: Option<&JsValue>,
    fallback: Option<f64>,
    label: &str,
) -> std::result::Result<f64, HttpStoreValidationError> {
    // `value ?? fallback`: only null/undefined fall back; any other non-number
    // still fails the `typeof` check below.
    let resolved = match value {
        None | Some(JsValue::Undefined | JsValue::Null) => fallback,
        Some(JsValue::Number(number)) => Some(*number),
        Some(_) => None,
    };
    match resolved {
        Some(number)
            if number.is_finite()
                && number.fract() == 0.0
                && number.abs() <= 9_007_199_254_740_991.0
                && number > 0.0 =>
        {
            Ok(number)
        }
        _ => Err(validation(
            "invalid_request",
            format!("{label} must be a positive safe integer."),
        )),
    }
}

fn required_dialog(
    value: Option<&JsValue>,
) -> std::result::Result<StoredDialog, HttpStoreValidationError> {
    let dialog = required_record(value, "dialog")?;
    let request = required_record(dialog.get("request"), "dialog.request")?;
    if request
        .get("type")
        .and_then(JsValue::as_string)
        .is_none_or(|kind| !kind.equals_str("user_dialog_request"))
    {
        return Err(validation(
            "invalid_dialog",
            "dialog.request must be a user_dialog_request.",
        ));
    }
    // Deliberately recompute the request value: the record keeps every extra
    // property of `request` (TS `structuredClone`), but nothing from `dialog`.
    let request_value = dialog
        .get("request")
        .cloned()
        .expect("required dialog.request");
    required_string(request.get("requestId"), "dialog.request.requestId")?;
    required_string(request.get("toolCallId"), "dialog.request.toolCallId")?;
    required_string(request.get("toolName"), "dialog.request.toolName")?;
    required_string(request.get("prompt"), "dialog.request.prompt")?;
    let kind = request
        .get("dialogKind")
        .and_then(JsValue::as_string)
        .and_then(|kind| kind.to_utf8().ok());
    if !matches!(
        kind.as_deref(),
        Some("input" | "select" | "confirm" | "form")
    ) {
        return Err(validation(
            "invalid_dialog",
            "dialog.request.dialogKind is invalid.",
        ));
    }
    Ok(StoredDialog {
        request: request_value,
        created_at: required_string(dialog.get("createdAt"), "dialog.createdAt")?.clone(),
    })
}

fn parse_store_key(
    value: Option<&JsValue>,
) -> std::result::Result<StoreKey, HttpStoreValidationError> {
    let key = required_record(value, "key")?;
    Ok(StoreKey {
        project_root: required_string(key.get("projectRoot"), "key.projectRoot")?.clone(),
        pilot_home: required_string(key.get("pilotHome"), "key.pilotHome")?.clone(),
        session_id: required_string(key.get("sessionId"), "key.sessionId")?.clone(),
    })
}

struct ParsedRequest<'a> {
    key: StoreKey,
    dialog: Option<StoredDialog>,
    request_id: Option<&'a JsString>,
    input: Option<&'a JsObject>,
}

fn parse_request(
    value: &JsValue,
) -> std::result::Result<ParsedRequest<'_>, HttpStoreValidationError> {
    let object = match value {
        JsValue::Object(object) => object,
        _ => return Err(version_error()),
    };
    if object.get("version").and_then(JsValue::as_number) != Some(HTTP_STORE_VERSION) {
        return Err(version_error());
    }
    let key = parse_store_key(object.get("key"))?;
    let dialog = match object.get("dialog") {
        None | Some(JsValue::Undefined) => None,
        value => Some(required_dialog(value)?),
    };
    let request_id = match object.get("requestId") {
        None | Some(JsValue::Undefined) => None,
        value => Some(required_string(value, "requestId")?),
    };
    let input = match object.get("input") {
        None | Some(JsValue::Undefined) => None,
        value => Some(required_record(value, "input")?),
    };
    Ok(ParsedRequest {
        key,
        dialog,
        request_id,
        input,
    })
}
fn version_error() -> HttpStoreValidationError {
    validation(
        "invalid_request",
        "Request must use gateway user-dialog store schema version 1.",
    )
}

fn parse_claim_input(
    input: Option<&JsObject>,
) -> std::result::Result<(JsString, f64, Option<JsString>), HttpStoreValidationError> {
    let input = require_input(input)?;
    Ok((
        required_string(input.get("requestId"), "input.requestId")?.clone(),
        positive_integer(input.get("ttlMs"), None, "input.ttlMs")?,
        optional_string(input.get("leaseId"), "input.leaseId")?,
    ))
}
fn parse_release_input(
    input: Option<&JsObject>,
) -> std::result::Result<(JsString, JsString), HttpStoreValidationError> {
    let input = require_input(input)?;
    Ok((
        required_string(input.get("requestId"), "input.requestId")?.clone(),
        required_string(input.get("leaseId"), "input.leaseId")?.clone(),
    ))
}
fn parse_owner_renew_input(
    input: Option<&JsObject>,
) -> std::result::Result<(JsString, JsString, f64), HttpStoreValidationError> {
    let input = require_input(input)?;
    Ok((
        required_string(input.get("requestId"), "input.requestId")?.clone(),
        required_string(input.get("ownerId"), "input.ownerId")?.clone(),
        positive_integer(input.get("ttlMs"), None, "input.ttlMs")?,
    ))
}
fn parse_owner_release_input(
    input: Option<&JsObject>,
) -> std::result::Result<(JsString, JsString), HttpStoreValidationError> {
    let input = require_input(input)?;
    Ok((
        required_string(input.get("requestId"), "input.requestId")?.clone(),
        required_string(input.get("ownerId"), "input.ownerId")?.clone(),
    ))
}
fn require_input(
    input: Option<&JsObject>,
) -> std::result::Result<&JsObject, HttpStoreValidationError> {
    input.ok_or_else(|| validation("invalid_request", "input must be an object."))
}
struct ParsedAnswer {
    request_id: JsString,
    lease_id: Option<JsString>,
    result: DialogAnswer,
}
fn parse_answer_input(
    input: Option<&JsObject>,
) -> std::result::Result<ParsedAnswer, HttpStoreValidationError> {
    let input = require_input(input)?;
    parse_answer_result(
        input.get("result"),
        input.get("requestId"),
        input.get("leaseId"),
    )
}
/// Mirrors `parseAnswerInput({requestId, leaseId, result})`: the result record
/// and its `behavior` are checked before either identifier.
fn parse_answer_result(
    result: Option<&JsValue>,
    request_id: Option<&JsValue>,
    lease_id: Option<&JsValue>,
) -> std::result::Result<ParsedAnswer, HttpStoreValidationError> {
    let result = required_record(result, "input.result")?;
    let behavior = result
        .get("behavior")
        .and_then(JsValue::as_string)
        .and_then(|behavior| behavior.to_utf8().ok());
    let answered = match behavior.as_deref() {
        Some("answered") => true,
        Some("cancelled") => false,
        _ => {
            return Err(validation(
                "invalid_dialog_result",
                "input.result must be answered or cancelled.",
            ));
        }
    };
    let request_id = required_string(request_id, "input.requestId")?.clone();
    let lease_id = optional_string(lease_id, "input.leaseId")?;
    let result = if answered {
        DialogAnswer::Answered(result.get("value").cloned().unwrap_or(JsValue::Undefined))
    } else {
        DialogAnswer::Cancelled(optional_string(
            result.get("reason"),
            "input.result.reason",
        )?)
    };
    Ok(ParsedAnswer {
        request_id,
        lease_id,
        result,
    })
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct HttpGatewayUserDialogStoreOptions {
    pub url: String,
    pub authorization_token: String,
    /// `timeoutMs`; `None` keeps the TS default of 10_000. Non-integral,
    /// non-positive, non-finite or unsafe values fail like TS.
    pub timeout_ms: Option<f64>,
}

/// One outgoing fetch-shaped exchange. `authorization` already includes the
/// `Bearer ` prefix; `content_type` is the fixed JSON media type. `timeout` is
/// the TS `AbortSignal` deadline for the whole exchange: headers plus body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpStoreTransportRequest {
    pub url: String,
    pub authorization: String,
    pub content_type: String,
    pub body: Vec<u8>,
    pub timeout: Duration,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpStoreTransportResponse {
    pub status: u16,
    pub body: Vec<u8>,
}
/// Raw fetch failure text: `fetch failed` for network errors before the
/// response head, or the abort reason `gateway_user_dialog_store_timeout`.
/// A body failure after the head is not an error here: TS
/// `response.json().catch(() => undefined)` turns it into an undefined body and
/// `call` then decides by status code, exactly like any other undefined body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpStoreTransportError {
    pub message: String,
}
impl HttpStoreTransportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The injectable `fetch` boundary. The returned future owns no detached work:
/// dropping it (the client's timeout) cancels the exchange like `AbortSignal`.
pub trait HttpStoreTransport: Send + Sync {
    fn post<'a>(
        &'a self,
        request: HttpStoreTransportRequest,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = std::result::Result<
                        HttpStoreTransportResponse,
                        HttpStoreTransportError,
                    >,
                > + Send
                + 'a,
        >,
    >;
}

/// Production transport: undici-compatible redirect/port rules via
/// `fetch_redirect::follow`, HTTP(S) through the A6.4 provider connector.
#[derive(Clone)]
pub struct HyperHttpStoreTransport {
    client: Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>,
    max_body_bytes: usize,
}
impl Default for HyperHttpStoreTransport {
    fn default() -> Self {
        let config = crate::runtime::model::http::webpki_client_config();
        Self {
            client: Client::builder(TokioExecutor::new())
                .build(crate::runtime::model::http::https_connector(config)),
            max_body_bytes: crate::fetch_redirect::MAX_STRING_LENGTH,
        }
    }
}
impl HyperHttpStoreTransport {
    /// The production decoder keeps Node's `MAX_STRING_LENGTH` ceiling. Tests
    /// substitute a small bound so the over-limit branch is reachable without
    /// allocating half a gigabyte.
    #[cfg(test)]
    fn with_max_body_bytes(max_body_bytes: usize) -> Self {
        Self {
            max_body_bytes,
            ..Self::default()
        }
    }
}
impl fmt::Debug for HyperHttpStoreTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HyperHttpStoreTransport")
    }
}
impl HttpStoreTransport for HyperHttpStoreTransport {
    fn post<'a>(
        &'a self,
        request: HttpStoreTransportRequest,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = std::result::Result<
                        HttpStoreTransportResponse,
                        HttpStoreTransportError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let url = Url::parse(&request.url).map_err(|_| failed())?;
            let mut headers = HeaderMap::new();
            headers.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_bytes(request.authorization.as_bytes())
                    .map_err(|_| failed())?,
            );
            headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_bytes(request.content_type.as_bytes())
                    .map_err(|_| failed())?,
            );
            let client = self.client.clone();
            // `setTimeout(..., timeoutMs)` starts before the fetch and covers
            // redirects, the response head and the body read.
            let deadline = tokio::time::Instant::now() + request.timeout;
            let response = match tokio::time::timeout_at(
                deadline,
                crate::fetch_redirect::follow(
                    url,
                    Method::POST,
                    headers,
                    Bytes::from(request.body),
                    failed,
                    move |request| {
                        let client = client.clone();
                        async move { client.request(request).await.map_err(|_| failed()) }
                    },
                ),
            )
            .await
            {
                Ok(response) => response?,
                Err(_) => return Err(HttpStoreTransportError::new(TIMEOUT_REASON)),
            };
            let status = response.status().as_u16();
            // After a response head exists, TS does
            // `response.json().catch(() => undefined)`: a body timeout,
            // termination or the `MAX_STRING_LENGTH` ceiling is not a transport
            // error, it leaves the body undefined. `call` then decides by the
            // status code (2xx -> invalid response, otherwise `HTTP n`).
            // Read frame by frame so an endless/oversized body cannot grow
            // memory without bound.
            let body = match tokio::time::timeout_at(
                deadline,
                crate::fetch_redirect::read_body(response.into_body(), self.max_body_bytes, None),
            )
            .await
            {
                Ok(Ok(body)) => body,
                Ok(Err(_)) | Err(_) => Vec::new(),
            };
            Ok(HttpStoreTransportResponse { status, body })
        })
    }
}
fn failed() -> HttpStoreTransportError {
    HttpStoreTransportError::new("fetch failed")
}

pub struct HttpGatewayUserDialogStore {
    endpoint: Url,
    authorization_token: String,
    /// The validated `timeoutMs` property, unchanged by the Node `setTimeout`
    /// overflow clamp that only affects the effective timer (`timeout`).
    timeout_ms: f64,
    timeout: Duration,
    transport: Arc<dyn HttpStoreTransport>,
}
impl fmt::Debug for HttpGatewayUserDialogStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpGatewayUserDialogStore")
            .field("endpoint", &self.endpoint.as_str())
            .field("timeoutMs", &self.timeout_ms)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}
impl HttpGatewayUserDialogStore {
    pub fn new(options: HttpGatewayUserDialogStoreOptions) -> Result<Self> {
        Self::with_transport(options, Arc::new(HyperHttpStoreTransport::default()))
    }

    pub fn with_transport(
        options: HttpGatewayUserDialogStoreOptions,
        transport: Arc<dyn HttpStoreTransport>,
    ) -> Result<Self> {
        let endpoint = normalize_endpoint(&options.url)?;
        if crate::metadata::js_trim(&options.authorization_token).is_empty() {
            return Err(DialogError::Invalid(
                "HttpGatewayUserDialogStore authorizationToken is required.".into(),
            ));
        }
        let timeout_ms = positive_integer(
            options.timeout_ms.map(JsValue::Number).as_ref(),
            Some(DEFAULT_TIMEOUT_MS),
            "timeoutMs",
        )
        .map_err(|error| DialogError::Invalid(error.message))?;
        // Node clamps a `setTimeout` delay above 2^31-1 to 1 ms
        // (`TimeoutOverflowWarning`); keep that observable timeout behavior
        // while the `timeoutMs` property itself stays unclamped like TS.
        let effective_ms = if timeout_ms > 2_147_483_647.0 {
            1.0
        } else {
            timeout_ms
        };
        Ok(Self {
            endpoint,
            authorization_token: options.authorization_token,
            timeout_ms,
            // `positiveInteger` guarantees an integral safe value, so this
            // conversion is exact and cannot panic.
            timeout: Duration::from_millis(effective_ms as u64),
            transport,
        })
    }

    pub async fn put(&self, key: &StoreKey, dialog: &StoredDialog) -> Result<()> {
        self.call(
            Operation::Put,
            vec![("key", store_key_value(key)), ("dialog", dialog.to_value())],
        )
        .await?;
        Ok(())
    }
    pub async fn list(&self, key: &StoreKey) -> Result<Vec<StoredDialog>> {
        let value = self
            .call(Operation::List, vec![("key", store_key_value(key))])
            .await?;
        assert_dialogs(&value)
    }
    pub async fn remove(&self, key: &StoreKey, request_id: &JsString) -> Result<()> {
        self.call(
            Operation::Remove,
            vec![
                ("key", store_key_value(key)),
                ("requestId", JsValue::String(request_id.clone())),
            ],
        )
        .await?;
        Ok(())
    }
    pub async fn clear(&self, key: &StoreKey) -> Result<()> {
        self.call(Operation::Clear, vec![("key", store_key_value(key))])
            .await?;
        Ok(())
    }
    pub async fn list_live(&self, key: &StoreKey) -> Result<Vec<StoredDialog>> {
        let value = self
            .call(Operation::ListLive, vec![("key", store_key_value(key))])
            .await?;
        assert_dialogs(&value)
    }
    pub async fn claim_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        ttl_ms: f64,
        lease_id: Option<&JsString>,
    ) -> Result<HttpStoreLeaseClaim> {
        let mut input = JsObject::new();
        input.insert("requestId".into(), JsValue::String(request_id.clone()));
        input.insert("ttlMs".into(), JsValue::Number(ttl_ms));
        if let Some(lease_id) = lease_id {
            input.insert("leaseId".into(), JsValue::String(lease_id.clone()));
        }
        let value = self
            .call(
                Operation::ClaimLive,
                vec![
                    ("key", store_key_value(key)),
                    ("input", JsValue::Object(input)),
                ],
            )
            .await?;
        assert_lease_claim(&value)
    }
    pub async fn release_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: &JsString,
    ) -> Result<bool> {
        let value = self
            .call(
                Operation::ReleaseLive,
                vec![
                    ("key", store_key_value(key)),
                    (
                        "input",
                        input_object(&[
                            ("requestId", JsValue::String(request_id.clone())),
                            ("leaseId", JsValue::String(lease_id.clone())),
                        ]),
                    ),
                ],
            )
            .await?;
        assert_boolean(&value)
    }
    pub async fn submit_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: Option<&JsString>,
        result: &DialogAnswer,
    ) -> Result<bool> {
        let mut input = JsObject::new();
        input.insert("requestId".into(), JsValue::String(request_id.clone()));
        if let Some(lease_id) = lease_id {
            input.insert("leaseId".into(), JsValue::String(lease_id.clone()));
        }
        input.insert("result".into(), result.to_value());
        let value = self
            .call(
                Operation::SubmitLiveAnswer,
                vec![
                    ("key", store_key_value(key)),
                    ("input", JsValue::Object(input)),
                ],
            )
            .await?;
        assert_boolean(&value)
    }
    pub async fn take_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
    ) -> Result<Option<HttpStoreAnswer>> {
        let value = self
            .call(
                Operation::TakeLiveAnswer,
                vec![
                    ("key", store_key_value(key)),
                    ("requestId", JsValue::String(request_id.clone())),
                ],
            )
            .await?;
        assert_answer(&value)
    }
    pub async fn claim_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> Result<HttpStoreOwnerClaim> {
        let value = self
            .call(
                Operation::ClaimLiveOwner,
                vec![
                    ("key", store_key_value(key)),
                    (
                        "input",
                        input_object(&[
                            ("requestId", JsValue::String(request_id.clone())),
                            ("ownerId", JsValue::String(owner_id.clone())),
                            ("ttlMs", JsValue::Number(ttl_ms)),
                        ]),
                    ),
                ],
            )
            .await?;
        assert_owner_claim(&value)
    }
    pub async fn renew_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> Result<bool> {
        let value = self
            .call(
                Operation::RenewLiveOwner,
                vec![
                    ("key", store_key_value(key)),
                    (
                        "input",
                        input_object(&[
                            ("requestId", JsValue::String(request_id.clone())),
                            ("ownerId", JsValue::String(owner_id.clone())),
                            ("ttlMs", JsValue::Number(ttl_ms)),
                        ]),
                    ),
                ],
            )
            .await?;
        assert_boolean(&value)
    }
    pub async fn release_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
    ) -> Result<bool> {
        let value = self
            .call(
                Operation::ReleaseLiveOwner,
                vec![
                    ("key", store_key_value(key)),
                    (
                        "input",
                        input_object(&[
                            ("requestId", JsValue::String(request_id.clone())),
                            ("ownerId", JsValue::String(owner_id.clone())),
                        ]),
                    ),
                ],
            )
            .await?;
        assert_boolean(&value)
    }

    async fn call(
        &self,
        operation: Operation,
        payload: Vec<(&'static str, JsValue)>,
    ) -> Result<JsValue> {
        let mut body = JsObject::new();
        body.insert("version".into(), JsValue::Number(HTTP_STORE_VERSION));
        for (name, value) in payload {
            body.insert(name.into(), value);
        }
        let text = stringify_json(&JsValue::Object(body))
            .map_err(|error| DialogError::Invalid(error.to_string()))?
            .ok_or_else(|| DialogError::Invalid("Unable to serialize the request body.".into()))?;
        let url = self
            .endpoint
            .join(operation.name())
            .map_err(|_| DialogError::Invalid(INVALID_URL.into()))?;
        let request = HttpStoreTransportRequest {
            url: url.to_string(),
            authorization: format!("Bearer {}", self.authorization_token),
            content_type: "application/json".into(),
            body: text.into_bytes(),
            timeout: self.timeout,
        };
        // `HyperHttpStoreTransport` enforces the same deadline itself so a body
        // that stalls after the head becomes an undefined body (TS
        // `json().catch`), not a timeout message. This outer timer only aborts
        // transports that never honour the request deadline; the extra grace
        // keeps it from racing the body-phase deadline.
        let outcome = tokio::time::timeout(
            self.timeout + Duration::from_secs(1),
            self.transport.post(request),
        )
        .await;
        let response = match outcome {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => return Err(DialogError::Invalid(error.message)),
            Err(_) => return Err(DialogError::Invalid(TIMEOUT_REASON.into())),
        };
        let text = String::from_utf8_lossy(&response.body);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        // `response.json().catch(() => undefined)`: malformed JSON is not a
        // transport error, it just leaves the body undefined.
        let parsed = parse_json(text).ok();
        if !(200..=299).contains(&response.status) {
            let message = match parsed.as_ref().and_then(JsValue::as_object) {
                Some(object) => object
                    .get("message")
                    .and_then(JsValue::as_string)
                    .map(|message| String::from_utf16_lossy(message.units())),
                None => None,
            }
            .unwrap_or_else(|| format!("HTTP {}", response.status));
            return Err(DialogError::Invalid(format!(
                "Gateway user-dialog store {} failed: {message}",
                operation.name()
            )));
        }
        let Some(object) = parsed.as_ref().and_then(JsValue::as_object) else {
            return Err(DialogError::Invalid(format!(
                "Gateway user-dialog store {} returned an invalid response.",
                operation.name()
            )));
        };
        if object.get("version").and_then(JsValue::as_number) != Some(HTTP_STORE_VERSION) {
            return Err(DialogError::Invalid(format!(
                "Gateway user-dialog store {} returned an invalid response.",
                operation.name()
            )));
        }
        match object.get("result") {
            Some(result) => Ok(result.clone()),
            None => Err(DialogError::Invalid(format!(
                "Gateway user-dialog store {} returned an invalid response.",
                operation.name()
            ))),
        }
    }
}

/// TS `assertAnswer` reconstructs `{requestId, submittedAt, result}` (note the
/// order differs from the stored `{requestId, result, submittedAt}`).
#[derive(Clone, Debug, PartialEq)]
pub struct HttpStoreAnswer {
    pub request_id: JsString,
    pub submitted_at: JsString,
    pub result: DialogAnswer,
}
impl HttpStoreAnswer {
    pub fn to_value(&self) -> JsValue {
        object([
            ("requestId", JsValue::String(self.request_id.clone())),
            ("submittedAt", JsValue::String(self.submitted_at.clone())),
            ("result", self.result.to_value()),
        ])
    }
}

/// TS `GatewayStoredUserDialogLeaseClaim`. The optional `expiresAt` on the
/// negative branch is not representable by the store's `LeaseClaim`, which only
/// describes records the file store actually writes.
#[derive(Clone, Debug, PartialEq)]
pub enum HttpStoreLeaseClaim {
    Claimed {
        lease_id: JsString,
        expires_at: JsString,
    },
    NotClaimed {
        reason: JsString,
        expires_at: Option<JsString>,
    },
}
impl HttpStoreLeaseClaim {
    pub fn to_value(&self) -> JsValue {
        match self {
            Self::Claimed {
                lease_id,
                expires_at,
            } => object([
                ("claimed", JsValue::Bool(true)),
                ("leaseId", JsValue::String(lease_id.clone())),
                ("expiresAt", JsValue::String(expires_at.clone())),
            ]),
            Self::NotClaimed { reason, expires_at } => {
                let mut claim = JsObject::new();
                claim.insert("claimed".into(), JsValue::Bool(false));
                claim.insert("reason".into(), JsValue::String(reason.clone()));
                if let Some(expires_at) = expires_at {
                    claim.insert("expiresAt".into(), JsValue::String(expires_at.clone()));
                }
                JsValue::Object(claim)
            }
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum HttpStoreOwnerClaim {
    Owned {
        owner_id: JsString,
        expires_at: JsString,
    },
    NotOwned {
        reason: JsString,
        expires_at: Option<JsString>,
    },
}
impl HttpStoreOwnerClaim {
    pub fn to_value(&self) -> JsValue {
        match self {
            Self::Owned {
                owner_id,
                expires_at,
            } => object([
                ("owned", JsValue::Bool(true)),
                ("ownerId", JsValue::String(owner_id.clone())),
                ("expiresAt", JsValue::String(expires_at.clone())),
            ]),
            Self::NotOwned { reason, expires_at } => {
                let mut claim = JsObject::new();
                claim.insert("owned".into(), JsValue::Bool(false));
                claim.insert("reason".into(), JsValue::String(reason.clone()));
                if let Some(expires_at) = expires_at {
                    claim.insert("expiresAt".into(), JsValue::String(expires_at.clone()));
                }
                JsValue::Object(claim)
            }
        }
    }
}

fn normalize_endpoint(value: &str) -> Result<Url> {
    let mut endpoint = Url::parse(value).map_err(|_| {
        DialogError::Invalid("HttpGatewayUserDialogStore url must be an absolute URL.".into())
    })?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err(DialogError::Invalid(
            "HttpGatewayUserDialogStore url must use HTTP(S).".into(),
        ));
    }
    if !endpoint.path().ends_with('/') {
        endpoint.set_path(&format!("{}/", endpoint.path()));
    }
    Ok(endpoint)
}

fn assert_dialogs(value: &JsValue) -> Result<Vec<StoredDialog>> {
    let JsValue::Array(dialogs) = value else {
        return Err(DialogError::Invalid(
            "Gateway user-dialog store returned invalid dialog records.".into(),
        ));
    };
    dialogs
        .iter()
        .map(|dialog| {
            required_dialog(Some(dialog)).map_err(|error| DialogError::Invalid(error.message))
        })
        .collect()
}
fn assert_lease_claim(value: &JsValue) -> Result<HttpStoreLeaseClaim> {
    let claim = required_record(Some(value), "response.claim")
        .map_err(|error| DialogError::Invalid(error.message))?;
    match claim.get("claimed").and_then(JsValue::as_bool) {
        Some(true) => Ok(HttpStoreLeaseClaim::Claimed {
            lease_id: required_string(claim.get("leaseId"), "response.claim.leaseId")
                .map_err(|error| DialogError::Invalid(error.message))?
                .clone(),
            expires_at: required_string(claim.get("expiresAt"), "response.claim.expiresAt")
                .map_err(|error| DialogError::Invalid(error.message))?
                .clone(),
        }),
        Some(false) => {
            let reason = required_claim_reason(
                claim,
                "claimed",
                "Gateway user-dialog store returned an invalid lease claim.",
            )?;
            Ok(HttpStoreLeaseClaim::NotClaimed {
                reason,
                expires_at: optional_string(claim.get("expiresAt"), "response.claim.expiresAt")
                    .map_err(|error| DialogError::Invalid(error.message))?,
            })
        }
        _ => Err(DialogError::Invalid(
            "Gateway user-dialog store returned an invalid lease claim.".into(),
        )),
    }
}
fn assert_owner_claim(value: &JsValue) -> Result<HttpStoreOwnerClaim> {
    let claim = required_record(Some(value), "response.ownerClaim")
        .map_err(|error| DialogError::Invalid(error.message))?;
    match claim.get("owned").and_then(JsValue::as_bool) {
        Some(true) => Ok(HttpStoreOwnerClaim::Owned {
            owner_id: required_string(claim.get("ownerId"), "response.ownerClaim.ownerId")
                .map_err(|error| DialogError::Invalid(error.message))?
                .clone(),
            expires_at: required_string(claim.get("expiresAt"), "response.ownerClaim.expiresAt")
                .map_err(|error| DialogError::Invalid(error.message))?
                .clone(),
        }),
        Some(false) => {
            let reason = required_claim_reason(
                claim,
                "owned",
                "Gateway user-dialog store returned an invalid owner claim.",
            )?;
            Ok(HttpStoreOwnerClaim::NotOwned {
                reason,
                expires_at: optional_string(
                    claim.get("expiresAt"),
                    "response.ownerClaim.expiresAt",
                )
                .map_err(|error| DialogError::Invalid(error.message))?,
            })
        }
        _ => Err(DialogError::Invalid(
            "Gateway user-dialog store returned an invalid owner claim.".into(),
        )),
    }
}
/// TS compares `reason` against the two literals (no `requiredString` label), so
/// a non-string or an unknown literal is the generic "invalid claim" error.
fn required_claim_reason(
    claim: &JsObject,
    pending: &str,
    invalid_message: &str,
) -> Result<JsString> {
    match claim.get("reason").and_then(JsValue::as_string) {
        Some(reason) if reason.equals_str(pending) || reason.equals_str("not_pending") => {
            Ok(reason.clone())
        }
        _ => Err(DialogError::Invalid(invalid_message.to_string())),
    }
}
fn assert_answer(value: &JsValue) -> Result<Option<HttpStoreAnswer>> {
    if matches!(value, JsValue::Null) {
        return Ok(None);
    }
    let answer = required_record(Some(value), "response.answer")
        .map_err(|error| DialogError::Invalid(error.message))?;
    let request_id = required_string(answer.get("requestId"), "response.answer.requestId")
        .map_err(|error| DialogError::Invalid(error.message))?
        .clone();
    let submitted_at = required_string(answer.get("submittedAt"), "response.answer.submittedAt")
        .map_err(|error| DialogError::Invalid(error.message))?
        .clone();
    let parsed = parse_answer_result(answer.get("result"), answer.get("requestId"), None)
        .map_err(|error| DialogError::Invalid(error.message))?;
    Ok(Some(HttpStoreAnswer {
        request_id,
        submitted_at,
        result: parsed.result,
    }))
}
fn assert_boolean(value: &JsValue) -> Result<bool> {
    match value {
        JsValue::Bool(value) => Ok(*value),
        _ => Err(DialogError::Invalid(
            "Gateway user-dialog store returned an invalid boolean result.".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Handler (framework-neutral, mirrors `createGatewayUserDialogStoreHttpHandler`)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct HttpStoreHttpRequest {
    pub method: String,
    pub url: String,
    /// Header pairs in arrival order. Lookups are case-insensitive and duplicate
    /// values combine with `", "` like the Fetch `Headers` object.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpStoreHttpResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct GatewayUserDialogStoreHttpHandler {
    store: Arc<dyn LiveGatewayUserDialogStore>,
    authorization_token: String,
}
impl fmt::Debug for GatewayUserDialogStoreHttpHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GatewayUserDialogStoreHttpHandler")
    }
}

pub fn create_gateway_user_dialog_store_http_handler(
    store: Arc<dyn LiveGatewayUserDialogStore>,
    authorization_token: &str,
) -> Result<GatewayUserDialogStoreHttpHandler> {
    if crate::metadata::js_trim(authorization_token).is_empty() {
        return Err(DialogError::Invalid(
            "Gateway user-dialog store HTTP authorizationToken is required.".into(),
        ));
    }
    Ok(GatewayUserDialogStoreHttpHandler {
        store,
        authorization_token: authorization_token.to_string(),
    })
}

#[derive(Debug)]
enum ServiceError {
    Validation(HttpStoreValidationError),
    Failure(String),
}

impl GatewayUserDialogStoreHttpHandler {
    pub fn handle(&self, request: HttpStoreHttpRequest) -> HttpStoreHttpResponse {
        match self.precheck(&request.method, &request.url, &request.headers) {
            Ok(operation) => self.dispatch_request(operation, request),
            Err(response) => response,
        }
    }

    /// The checks Node performs before it consumes the request body. The server
    /// runs them first so an unauthenticated or oversized request never makes it
    /// read (and buffer) a body it is going to reject.
    fn precheck(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
    ) -> std::result::Result<Operation, HttpStoreHttpResponse> {
        // TS checks the method before it even looks at the URL or the token.
        if method != "POST" {
            return Err(error_response(
                405,
                "method_not_allowed",
                "Only POST is supported.",
            ));
        }
        // `operationFromRequest` runs outside the handler's try/catch; an
        // unparseable Request URL becomes a 500 via `serveNodeRequest`.
        let operation = match operation_from_request(url) {
            Ok(Some(operation)) => operation,
            Ok(None) => {
                return Err(error_response(
                    404,
                    "not_found",
                    "Unknown Gateway user-dialog store operation.",
                ));
            }
            Err(message) => return Err(error_response(500, "server_failure", &message)),
        };
        if !tokens_match(
            header(headers, "authorization").as_deref(),
            &self.authorization_token,
        ) {
            return Err(error_response(
                401,
                "unauthorized",
                "Gateway user-dialog store authorization failed.",
            ));
        }
        Ok(operation)
    }

    fn dispatch_request(
        &self,
        operation: Operation,
        request: HttpStoreHttpRequest,
    ) -> HttpStoreHttpResponse {
        let text = decode_body(&request.body);
        let Ok(value) = parse_json(&text) else {
            return error_response(400, "invalid_json", "Request body must be JSON.");
        };
        match self.dispatch(operation, &value) {
            Ok(result) => {
                let body = object([
                    ("version", JsValue::Number(HTTP_STORE_VERSION)),
                    ("result", result),
                ]);
                match stringify_json(&body) {
                    Ok(Some(text)) => HttpStoreHttpResponse {
                        status: 200,
                        content_type: CONTENT_TYPE.into(),
                        body: text.into_bytes(),
                    },
                    Ok(None) => {
                        error_response(500, "store_failure", "Unable to serialize the result.")
                    }
                    Err(error) => error_response(500, "store_failure", &error.to_string()),
                }
            }
            Err(ServiceError::Validation(error)) => {
                error_response(400, error.code(), error.message())
            }
            Err(ServiceError::Failure(message)) => error_response(500, "store_failure", &message),
        }
    }

    fn dispatch(
        &self,
        operation: Operation,
        value: &JsValue,
    ) -> std::result::Result<JsValue, ServiceError> {
        let request = parse_request(value).map_err(ServiceError::Validation)?;
        let store = |error: DialogError| ServiceError::Failure(error.to_string());
        Ok(match operation {
            Operation::Put => {
                // `parse_request` only validates a `dialog` that is present;
                // dispatch requires it just like `requiredDialog(request.dialog)`.
                let dialog = request.dialog.clone().ok_or_else(missing_dialog)?;
                self.store.put(&request.key, &dialog).map_err(store)?;
                JsValue::Null
            }
            Operation::List => JsValue::Array(
                self.store
                    .list(&request.key)
                    .map_err(store)?
                    .iter()
                    .map(StoredDialog::to_value)
                    .collect(),
            ),
            Operation::Remove => {
                let request_id = request.request_id.cloned().ok_or_else(missing_request_id)?;
                self.store
                    .remove(&request.key, &request_id)
                    .map_err(store)?;
                JsValue::Null
            }
            Operation::Clear => {
                self.store.clear(&request.key).map_err(store)?;
                JsValue::Null
            }
            Operation::ListLive => JsValue::Array(
                self.store
                    .list_live(&request.key)
                    .map_err(store)?
                    .iter()
                    .map(StoredDialog::to_value)
                    .collect(),
            ),
            Operation::ClaimLive => {
                let (request_id, ttl_ms, lease_id) =
                    parse_claim_input(request.input).map_err(ServiceError::Validation)?;
                lease_claim_value(
                    &self
                        .store
                        .claim_live(&request.key, &request_id, ttl_ms, lease_id.as_ref())
                        .map_err(store)?,
                )
            }
            Operation::ReleaseLive => {
                let (request_id, lease_id) =
                    parse_release_input(request.input).map_err(ServiceError::Validation)?;
                JsValue::Bool(
                    self.store
                        .release_live(&request.key, &request_id, &lease_id)
                        .map_err(store)?,
                )
            }
            Operation::SubmitLiveAnswer => {
                let parsed = parse_answer_input(request.input).map_err(ServiceError::Validation)?;
                JsValue::Bool(
                    self.store
                        .submit_live_answer(
                            &request.key,
                            &parsed.request_id,
                            parsed.lease_id.as_ref(),
                            &parsed.result,
                        )
                        .map_err(store)?,
                )
            }
            Operation::TakeLiveAnswer => {
                let request_id = request.request_id.cloned().ok_or_else(missing_request_id)?;
                match self
                    .store
                    .take_live_answer(&request.key, &request_id)
                    .map_err(store)?
                {
                    Some(answer) => answer.to_value(),
                    None => JsValue::Null,
                }
            }
            Operation::ClaimLiveOwner => {
                let (request_id, owner_id, ttl_ms) =
                    parse_owner_renew_input(request.input).map_err(ServiceError::Validation)?;
                owner_claim_value(
                    &self
                        .store
                        .claim_live_owner(&request.key, &request_id, &owner_id, ttl_ms)
                        .map_err(store)?,
                )
            }
            Operation::RenewLiveOwner => {
                let (request_id, owner_id, ttl_ms) =
                    parse_owner_renew_input(request.input).map_err(ServiceError::Validation)?;
                JsValue::Bool(
                    self.store
                        .renew_live_owner(&request.key, &request_id, &owner_id, ttl_ms)
                        .map_err(store)?,
                )
            }
            Operation::ReleaseLiveOwner => {
                let (request_id, owner_id) =
                    parse_owner_release_input(request.input).map_err(ServiceError::Validation)?;
                JsValue::Bool(
                    self.store
                        .release_live_owner(&request.key, &request_id, &owner_id)
                        .map_err(store)?,
                )
            }
        })
    }
}

fn missing_request_id() -> ServiceError {
    ServiceError::Validation(validation(
        "invalid_request",
        "requestId must be a non-empty string.",
    ))
}

fn missing_dialog() -> ServiceError {
    ServiceError::Validation(validation("invalid_request", "dialog must be an object."))
}

fn lease_claim_value(claim: &LeaseClaim) -> JsValue {
    match claim {
        LeaseClaim::Claimed(lease) => object([
            ("claimed", JsValue::Bool(true)),
            ("leaseId", JsValue::String(lease.lease_id.clone())),
            ("expiresAt", JsValue::String(lease.expires_at.clone())),
        ]),
        LeaseClaim::Busy { expires_at } => object([
            ("claimed", JsValue::Bool(false)),
            ("reason", JsValue::from("claimed")),
            ("expiresAt", JsValue::String(expires_at.clone())),
        ]),
        LeaseClaim::NotPending => object([
            ("claimed", JsValue::Bool(false)),
            ("reason", JsValue::from("not_pending")),
        ]),
    }
}
fn owner_claim_value(claim: &OwnerClaim) -> JsValue {
    match claim {
        OwnerClaim::Owned {
            owner_id,
            expires_at,
        } => object([
            ("owned", JsValue::Bool(true)),
            ("ownerId", JsValue::String(owner_id.clone())),
            ("expiresAt", JsValue::String(expires_at.clone())),
        ]),
        OwnerClaim::Busy { expires_at } => object([
            ("owned", JsValue::Bool(false)),
            ("reason", JsValue::from("owned")),
            ("expiresAt", JsValue::String(expires_at.clone())),
        ]),
        OwnerClaim::NotPending => object([
            ("owned", JsValue::Bool(false)),
            ("reason", JsValue::from("not_pending")),
        ]),
    }
}

fn operation_from_request(url: &str) -> std::result::Result<Option<Operation>, String> {
    let parsed = Url::parse(url).map_err(|_| INVALID_URL.to_string())?;
    let path = parsed.path();
    let Some(operation) = path.strip_prefix(STORE_PATH_PREFIX) else {
        return Ok(None);
    };
    Ok(Operation::parse(operation))
}

fn tokens_match(authorization: Option<&str>, expected: &str) -> bool {
    let Some(value) = authorization else {
        return false;
    };
    let Some(supplied) = value.strip_prefix("Bearer ") else {
        return false;
    };
    let supplied = supplied.as_bytes();
    let expected = expected.as_bytes();
    if supplied.len() != expected.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in supplied.iter().zip(expected) {
        difference |= left ^ right;
    }
    difference == 0
}

/// `Headers.get` combines duplicate values with `", "`.
fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    let values: Vec<&str> = headers
        .iter()
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values.join(", "))
    }
}

/// Node's UTF-8 decode (fatal=false) drops one leading BOM, then `JSON.parse`.
fn decode_body(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    text.strip_prefix('\u{feff}').unwrap_or(&text).to_string()
}

fn json_response(status: u16, value: &JsValue) -> HttpStoreHttpResponse {
    match stringify_json(value) {
        Ok(Some(text)) => HttpStoreHttpResponse {
            status,
            content_type: CONTENT_TYPE.into(),
            body: text.into_bytes(),
        },
        Ok(None) => error_response(500, "server_failure", "Unable to serialize the response."),
        Err(error) => error_response(500, "server_failure", &error.to_string()),
    }
}
fn error_response(status: u16, code: &str, message: &str) -> HttpStoreHttpResponse {
    json_response(
        status,
        &object([
            ("version", JsValue::Number(HTTP_STORE_VERSION)),
            ("error", JsValue::from(code)),
            ("message", JsValue::from(message)),
        ]),
    )
}

fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (name, value) in fields {
        object.insert(name.into(), value);
    }
    JsValue::Object(object)
}
fn input_object(fields: &[(&str, JsValue)]) -> JsValue {
    let mut object = JsObject::new();
    for (name, value) in fields {
        object.insert((*name).into(), value.clone());
    }
    JsValue::Object(object)
}
fn store_key_value(key: &StoreKey) -> JsValue {
    object([
        ("projectRoot", JsValue::String(key.project_root.clone())),
        ("pilotHome", JsValue::String(key.pilot_home.clone())),
        ("sessionId", JsValue::String(key.session_id.clone())),
    ])
}

// ---------------------------------------------------------------------------
// Node-compatible loopback server
// ---------------------------------------------------------------------------

pub struct GatewayUserDialogStoreHttpServer {
    url: String,
    shutdown: Option<watch::Sender<bool>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl fmt::Debug for GatewayUserDialogStoreHttpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GatewayUserDialogStoreHttpServer")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}
impl GatewayUserDialogStoreHttpServer {
    pub fn url(&self) -> &str {
        &self.url
    }
    /// Stops accepting new connections, closes idle keep-alive connections and
    /// waits for in-flight requests to finish, like Node v22 `server.close()`
    /// (which runs `closeIdleConnections()` before `net.Server.close()` and
    /// invokes its callback once every connection has ended).
    pub async fn close(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(true);
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

/// Node-compatible read limits. Defaults mirror `http.createServer` on Node
/// v22.22.0; tests substitute short timers and small caps so the branches are
/// reachable deterministically.
#[derive(Clone, Copy, Debug)]
struct ServeLimits {
    headers_timeout: Duration,
    request_timeout: Duration,
    keep_alive_timeout: Duration,
    keep_alive_timeout_buffer: Duration,
    max_header_bytes: usize,
    max_body_bytes: usize,
    max_connections: usize,
}
impl Default for ServeLimits {
    fn default() -> Self {
        Self {
            headers_timeout: DEFAULT_HEADERS_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            keep_alive_timeout: DEFAULT_KEEP_ALIVE_TIMEOUT,
            keep_alive_timeout_buffer: DEFAULT_KEEP_ALIVE_TIMEOUT_BUFFER,
            max_header_bytes: DEFAULT_MAX_HEADER_BYTES,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_connections: DEFAULT_MAX_CONNECTIONS,
        }
    }
}

/// Where one connection is in Node's timeout model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeoutPhase {
    /// Before the first complete request head: only `headersTimeout` applies.
    InitialHeaders,
    /// After a response was fully written: `keepAliveTimeout + buffer`, reset by
    /// socket activity.
    Idle,
    /// Bytes of a pipelined/follow-up request arrived: the idle inactivity timer
    /// and a fresh `headersTimeout` both run until the head completes.
    NextHeaders,
    /// A complete request head was parsed; `requestTimeout` (inside `serve`)
    /// owns the body and no connection-level timer may interrupt it.
    Request,
}

#[derive(Debug)]
struct TimeoutState {
    phase: TimeoutPhase,
    header_deadline: Option<tokio::time::Instant>,
    idle_deadline: Option<tokio::time::Instant>,
}
impl TimeoutState {
    fn nearest_deadline(&self) -> Option<tokio::time::Instant> {
        match (self.header_deadline, self.idle_deadline) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        }
    }
}

/// Node v22.22.0 connection timers, reproduced on top of hyper:
///
/// * the first request head is bounded by `headersTimeout` (60 s);
/// * after a response, `socket.setTimeout(keepAliveTimeout + buffer)` starts a
///   6 s inactivity timer (5 s + 1 s default);
/// * socket activity resets that inactivity timer, but does not postpone a
///   second request's `headersTimeout`, which starts at its first byte;
/// * a parsed request head clears the connection timers; the body is bounded by
///   `requestTimeout` in [`serve`].
///
/// The timeout closes the connection silently (`poll_read` returns EOF), like
/// Node's `socket.destroy()` after an idle/header timeout.
#[derive(Debug)]
struct KeepAliveTimer {
    headers_timeout: Duration,
    keep_alive_timeout: Duration,
    state: Mutex<TimeoutState>,
    /// Connection read waker. A state change (arm/disarm/reset) must wake a
    /// `poll_read` that is parked on an older deadline.
    waker: Mutex<Option<Waker>>,
}
impl KeepAliveTimer {
    fn new(headers_timeout: Duration, keep_alive_timeout: Duration) -> Self {
        let now = tokio::time::Instant::now();
        Self {
            headers_timeout,
            keep_alive_timeout,
            state: Mutex::new(TimeoutState {
                phase: TimeoutPhase::InitialHeaders,
                header_deadline: Some(now + headers_timeout),
                idle_deadline: None,
            }),
            waker: Mutex::new(None),
        }
    }

    fn store_waker(&self, cx: &Context<'_>) {
        *self.waker.lock().unwrap() = Some(cx.waker().clone());
    }

    fn wake(&self) {
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }

    /// A complete request head reached the handler; stop the connection timers
    /// so a slow request body is governed only by `requestTimeout`.
    fn request_started(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.phase = TimeoutPhase::Request;
            state.header_deadline = None;
            state.idle_deadline = None;
        }
        self.wake();
    }

    /// The response body ended. Node arms the keep-alive inactivity timer from
    /// the response `finish` event; direct arming here is at most one flush
    /// early, and the next write resets the deadline (see [`Self::write_activity`]).
    fn response_finished(&self) {
        let now = tokio::time::Instant::now();
        {
            let mut state = self.state.lock().unwrap();
            state.phase = TimeoutPhase::Idle;
            state.header_deadline = None;
            state.idle_deadline = Some(now + self.keep_alive_timeout);
        }
        self.wake();
    }

    /// Any successfully read byte resets the keep-alive inactivity timer. The
    /// first byte of a follow-up request additionally starts `headersTimeout`.
    fn read_activity(&self) {
        let now = tokio::time::Instant::now();
        {
            let mut state = self.state.lock().unwrap();
            match state.phase {
                TimeoutPhase::InitialHeaders => {}
                TimeoutPhase::Idle => {
                    state.phase = TimeoutPhase::NextHeaders;
                    state.header_deadline = Some(now + self.headers_timeout);
                    state.idle_deadline = Some(now + self.keep_alive_timeout);
                }
                TimeoutPhase::NextHeaders => {
                    state.idle_deadline = Some(now + self.keep_alive_timeout);
                }
                TimeoutPhase::Request => {}
            }
        }
        self.wake();
    }

    /// Writes count as socket activity for Node's `socket.setTimeout`, so a slow
    /// response flush must not consume the idle budget.
    fn write_activity(&self) {
        let now = tokio::time::Instant::now();
        {
            let mut state = self.state.lock().unwrap();
            if matches!(state.phase, TimeoutPhase::Idle | TimeoutPhase::NextHeaders) {
                state.idle_deadline = Some(now + self.keep_alive_timeout);
            }
        }
        self.wake();
    }

    fn nearest_deadline(&self) -> Option<tokio::time::Instant> {
        self.state.lock().unwrap().nearest_deadline()
    }
}

/// Transport wrapper that turns the shared deadline into a silent EOF. Keeping
/// the timer in `poll_read` means hyper is always the task that observes it, so
/// no background task can race the connection state.
struct TimeoutStream<S> {
    inner: S,
    timer: Arc<KeepAliveTimer>,
    sleep: Pin<Box<tokio::time::Sleep>>,
    armed_deadline: Option<tokio::time::Instant>,
}
impl<S> TimeoutStream<S> {
    fn new(inner: S, timer: Arc<KeepAliveTimer>) -> Self {
        let armed_deadline = timer.nearest_deadline();
        let sleep = Box::pin(tokio::time::sleep_until(armed_deadline.unwrap_or_else(
            || tokio::time::Instant::now() + Duration::from_secs(86_400),
        )));
        Self {
            inner,
            timer,
            sleep,
            armed_deadline,
        }
    }

    fn refresh_deadline(&mut self) -> Option<tokio::time::Instant> {
        let deadline = self.timer.nearest_deadline();
        if deadline != self.armed_deadline {
            self.armed_deadline = deadline;
            if let Some(deadline) = deadline {
                self.sleep.as_mut().reset(deadline);
            }
        }
        deadline
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for TimeoutStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let deadline = this.refresh_deadline();
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Poll::Ready(Ok(()));
        }
        match Pin::new(&mut this.inner).poll_read(cx, buffer) {
            Poll::Ready(Ok(())) => {
                if !buffer.filled().is_empty() {
                    this.timer.read_activity();
                }
                Poll::Ready(Ok(()))
            }
            Poll::Pending => {
                this.timer.store_waker(cx);
                if deadline.is_some() && this.sleep.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Ok(()))
                } else {
                    Poll::Pending
                }
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
        }
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for TimeoutStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_write(cx, buffer) {
            Poll::Ready(Ok(written)) => {
                if written > 0 {
                    this.timer.write_activity();
                }
                Poll::Ready(Ok(written))
            }
            poll => poll,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Response body that tells the connection timer when the response is done.
struct KeepAliveBody {
    inner: Full<Bytes>,
    timer: Arc<KeepAliveTimer>,
    finished: bool,
}
impl KeepAliveBody {
    fn new(inner: Full<Bytes>, timer: Arc<KeepAliveTimer>) -> Self {
        Self {
            inner,
            timer,
            finished: false,
        }
    }
}
impl Body for KeepAliveBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let frame = Pin::new(&mut this.inner).poll_frame(cx);
        if !this.finished
            && match &frame {
                Poll::Ready(None) => true,
                Poll::Ready(Some(Ok(frame))) => frame.is_data() && this.inner.is_end_stream(),
                _ => false,
            }
        {
            this.finished = true;
            this.timer.response_finished();
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

pub async fn start_gateway_user_dialog_store_http_server(
    store: Arc<dyn LiveGatewayUserDialogStore>,
    authorization_token: &str,
    host: Option<&str>,
    port: Option<u16>,
) -> Result<GatewayUserDialogStoreHttpServer> {
    start_gateway_user_dialog_store_http_server_with_limits(
        store,
        authorization_token,
        host,
        port,
        ServeLimits::default(),
    )
    .await
}

async fn start_gateway_user_dialog_store_http_server_with_limits(
    store: Arc<dyn LiveGatewayUserDialogStore>,
    authorization_token: &str,
    host: Option<&str>,
    port: Option<u16>,
    limits: ServeLimits,
) -> Result<GatewayUserDialogStoreHttpServer> {
    let handler = create_gateway_user_dialog_store_http_handler(store, authorization_token)?;
    let host = host.unwrap_or("127.0.0.1").to_string();
    let listener = TcpListener::bind((host.as_str(), port.unwrap_or(0)))
        .await
        .map_err(DialogError::Io)?;
    let address = listener.local_addr().map_err(DialogError::Io)?;
    let handler = Arc::new(handler);
    // hyper asserts `max_buf_size >= 8192`; keep that h1 minimum.
    let max_header_bytes = limits.max_header_bytes.max(8 * 1024);
    // Node arms `socket.setTimeout(keepAliveTimeout + keepAliveTimeoutBuffer)`.
    let keep_alive_timeout = limits.keep_alive_timeout + limits.keep_alive_timeout_buffer;
    let connections = Arc::new(Semaphore::new(limits.max_connections));
    let (shutdown, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { continue };
                    // Node has no connection cap; this semaphore only bounds the
                    // resources one process can pin down. Accept must never wait
                    // on a permit: a full server answers the overflow
                    // connection with an immediate 503 and keeps accepting, so
                    // one peer cannot park the accept loop for headersTimeout.
                    let Ok(permit) = connections.clone().try_acquire_owned() else {
                        // Answer without holding the accept loop; the writer
                        // runs as a short task so a peer that stops reading
                        // cannot park accept either.
                        tasks.spawn(reject_over_capacity(stream));
                        continue;
                    };
                    let handler = handler.clone();
                    let mut connection_shutdown = shutdown_rx.clone();
                    tasks.spawn(async move {
                        let _permit = permit;
                        let timer = Arc::new(KeepAliveTimer::new(
                            limits.headers_timeout,
                            keep_alive_timeout,
                        ));
                        let service_timer = timer.clone();
                        let service = service_fn(move |request: Request<Incoming>| {
                            let handler = handler.clone();
                            let timer = service_timer.clone();
                            async move {
                                // The complete request head is in; only
                                // `requestTimeout` may bound the body now.
                                timer.request_started();
                                let response = serve(handler, request, limits).await;
                                Ok::<_, Infallible>(
                                    response.map(|body| KeepAliveBody::new(body, timer)),
                                )
                            }
                        });
                        let mut builder = http1::Builder::new();
                        builder
                            .timer(TokioTimer::new())
                            // Node's `headersTimeout` is enforced by
                            // `TimeoutStream` from the first byte of each
                            // request. hyper's timer starts while the
                            // connection is still idle after a response, which
                            // would make a late partial head inherit a shorter
                            // deadline than Node's fresh 60 s.
                            .header_read_timeout(None::<Duration>)
                            // Node's `maxHeaderSize` (16 KiB) rejects with 431.
                            // hyper's own default caps at 100 fields; Node
                            // v22.22.0's runtime `maxHeadersCount` default is
                            // `null` (unlimited; the 2000 in the docs is not
                            // applied), so this is only a backstop above what
                            // the 16 KiB buffer can hold.
                            .max_headers(4096)
                            .max_buf_size(max_header_bytes);
                        let connection = builder.serve_connection(
                            TokioIo::new(TimeoutStream::new(stream, timer)),
                            service,
                        );
                        tokio::pin!(connection);
                        tokio::select! {
                            _ = wait_for_shutdown(&mut connection_shutdown) => {
                                // Node `server.close()` closes idle
                                // connections and lets an in-flight request
                                // finish; hyper's graceful shutdown has the
                                // same shape.
                                connection.as_mut().graceful_shutdown();
                                let _ = connection.await;
                            }
                            _ = &mut connection => {}
                        }
                    });
                }
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            }
        }
        drop(listener);
        // Node's close callback waits until every connection has ended. The
        // per-connection request/header timeouts already bound an abandoned
        // connection; the extra second only keeps a wedged task from hanging
        // shutdown forever.
        let deadline =
            tokio::time::Instant::now() + limits.request_timeout + Duration::from_secs(1);
        loop {
            match tokio::time::timeout_at(deadline, tasks.join_next()).await {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    break;
                }
            }
        }
    });
    Ok(GatewayUserDialogStoreHttpServer {
        url: format!("http://{host}:{}{STORE_PATH_PREFIX}", address.port()),
        shutdown: Some(shutdown),
        task: Some(task),
    })
}

const SERVICE_UNAVAILABLE: &[u8] =
    b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";

/// Answer an over-capacity connection without holding the accept loop. The
/// request bytes already queued by the peer are drained for a short bounded
/// window before the socket closes; closing with unread data would send an RST
/// that discards the 503 before the client can read it.
async fn reject_over_capacity(mut stream: TcpStream) {
    if stream.write_all(SERVICE_UNAVAILABLE).await.is_err() {
        return;
    }
    let _ = stream.shutdown().await;
    let mut buffer = [0u8; 1024];
    let _ = tokio::time::timeout(Duration::from_millis(100), async {
        while matches!(stream.read(&mut buffer).await, Ok(read) if read > 0) {}
    })
    .await;
}

/// Resolves when `close` starts (or the server handle is dropped).
async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    let _ = shutdown.changed().await;
}

#[derive(Debug)]
enum RequestBodyError {
    TooLarge,
    Failed(String),
}

async fn read_request_body(
    body: Incoming,
    limit: usize,
) -> std::result::Result<Vec<u8>, RequestBodyError> {
    match Limited::new(body, limit).collect().await {
        Ok(collected) => Ok(collected.to_bytes().to_vec()),
        Err(error) => {
            if error
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some()
            {
                Err(RequestBodyError::TooLarge)
            } else {
                Err(RequestBodyError::Failed(error.to_string()))
            }
        }
    }
}

fn payload_too_large() -> HttpStoreHttpResponse {
    error_response(413, "payload_too_large", "Request body is too large.")
}

async fn serve(
    handler: Arc<GatewayUserDialogStoreHttpHandler>,
    request: Request<Incoming>,
    limits: ServeLimits,
) -> Response<Full<Bytes>> {
    let method = request.method().as_str().to_string();
    let path = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/")
        .to_string();
    // `serveNodeRequest` rebuilds the absolute URL from the Host header and
    // assumes plain HTTP for the loopback service.
    let host = request
        .headers()
        .get(header::HOST)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .unwrap_or_else(|| "localhost".into());
    // Node's `maxHeaderSize` counts header field name + value bytes (probed on
    // v22.22.0: 2,500 fields of `aaaaa: x` pass, 2,800 fail; a 16,350-byte
    // value passes, 16,380 fails). This raw-byte sum also counts separators and
    // the request line, so it can reject a many-small-header request earlier
    // than Node; that stricter direction is registered as an unapproved
    // difference (see the A7.4-HTTPSTORE candidate note). hyper's
    // `max_buf_size` already rejects anything past its 16 KiB read buffer.
    let header_bytes: usize = request
        .headers()
        .iter()
        .map(|(name, value)| name.as_str().len() + 2 + value.as_bytes().len() + 2)
        .sum();
    if header_bytes > limits.max_header_bytes {
        // Node's parser answers 431 with an empty body and `Connection: close`.
        return empty_response(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    }
    // Node's `createServer` defaults to `joinDuplicateHeaders: false`: the
    // `IncomingMessage.headers` object keeps only the first value of a repeated
    // header (verified for `authorization`; a 1 MB body is not affected). The
    // framework-neutral handler still joins duplicates from a Fetch `Headers`,
    // so this collapse belongs to the Node-compatible server wrapper only.
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in request.headers().iter() {
        let name = name.as_str().to_string();
        if headers
            .iter()
            .any(|(existing, _)| existing.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        headers.push((name, String::from_utf8_lossy(value.as_bytes()).into_owned()));
    }
    let url = format!("http://{host}{path}");
    // Method -> operation -> token, exactly the handler's order, before a byte
    // of the body is read. 405/404/401 must not wait for (or buffer) the body.
    let operation = match handler.precheck(&method, &url, &headers) {
        Ok(operation) => operation,
        Err(response) => return build(response),
    };
    // An honest Content-Length lets us refuse before reading; a lying or absent
    // one is still caught by `Limited`.
    if let Some(length) = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        && length > limits.max_body_bytes as u64
    {
        return build(payload_too_large());
    }
    let body = match tokio::time::timeout(
        limits.request_timeout,
        read_request_body(request.into_body(), limits.max_body_bytes),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(RequestBodyError::TooLarge)) => return build(payload_too_large()),
        Ok(Err(RequestBodyError::Failed(error))) => {
            return build(error_response(500, "server_failure", &error));
        }
        Err(_) => {
            // `requestTimeout` in Node also answers with an empty body and
            // closes the connection.
            return empty_response(StatusCode::REQUEST_TIMEOUT);
        }
    };
    let request = HttpStoreHttpRequest {
        method,
        url,
        headers,
        body,
    };
    // `FileGatewayUserDialogStore` is synchronous by contract and may sleep on
    // its 5 s lock retry; keep it off the async executor's worker threads.
    let handler = Arc::clone(&handler);
    match tokio::task::spawn_blocking(move || handler.dispatch_request(operation, request)).await {
        Ok(response) => build(response),
        Err(error) => build(error_response(500, "server_failure", &error.to_string())),
    }
}

fn build(response: HttpStoreHttpResponse) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header(header::CONTENT_TYPE, response.content_type)
        .body(Full::new(Bytes::from(response.body)))
        .expect("static response parts are valid")
}

/// Node's parser-level 431/408 responses carry no body and close the socket.
fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(header::CONNECTION, "close")
        .body(Full::new(Bytes::new()))
        .expect("static response parts are valid")
}

#[cfg(test)]
#[path = "http_store_tests.rs"]
mod http_store_tests;
