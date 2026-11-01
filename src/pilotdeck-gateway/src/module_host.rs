//! Module-host HTTP transport: the `/api/module-host/describe` and
//! `/api/module-host/call` endpoints sidecars and modules use to call back
//! into the host (TS `ui/server/pilotdeck-host-remote-transport.mjs:5-30` is
//! the wire client). This is the #1349 component: it replicates the TS
//! `publicHostHttpTransport.ts` (baseline cc7c4e6) transport cell by cell but
//! answers from the Rust host itself, so no TS process participates. It is
//! not wired into `native_process` dispatch yet (#1305 owns the wiring); a
//! caller composes it in front of its own routing and treats `None` as "not
//! handled" — TS `handlePublicHostHttpRequest` returning `false`.
//!
//! Cells deliberately not modeled: the `resolve` callback throwing or
//! rejecting (TS injects a function; the Rust provider is an injected `Arc`,
//! so the channel does not exist), the `req`-aborted/`res`-close
//! AbortController wiring (it is bound to the real socket lifecycle the
//! native listener owns — the #1250 lane — and the 499
//! `PUBLIC_HOST_CANCELLED` provider response is covered as a plain
//! response), and `res` errors after headers were flushed, which the TS
//! catch answers by destroying the socket; here the same zero-terminator
//! close is the hyper body error the streaming cells replay.
//!
//! Transport-level byte divergences of the hyper codec are the ones the
//! A8.4-NATIVE-HTTP-001 ledger entry already registers for this listener
//! (header name case, chunk-size case, the HTTP/1.0 status line); the
//! module-host replay normalizes them the same way, and anything new is
//! pinned in A8.4-MODULE-HOST-001. One new residual: a request that closes
//! before its declared body length is complete draws Node's own parser-level
//! `400 Connection: close` (measured, oracle cell `premature-close`), which
//! hyper renders as the handler-error zero-byte close — the transport itself
//! writes nothing in either stack.
// The component is not wired into `native_process` dispatch yet (#1305 owns
// the wiring), so outside the test build nothing constructs it.
#![cfg_attr(not(test), allow(dead_code))]
use crate::{Body, metadata, model_catalog::trim};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::mpsc;

/// `PUBLIC_HOST_WIRE_OPERATIONS` (publicHostWire.ts), the transport's
/// operation allowlist. A provider operation outside this table is never
/// advertised, exactly the TS filter.
pub(crate) const WIRE_OPERATIONS: [&str; 22] = [
    "list_tools",
    "create_tool",
    "update_tool",
    "test_tool",
    "probe_unsaved_tool",
    "remove_tool",
    "list_general_skills",
    "import_general_skill",
    "publish_general_skill",
    "archive_general_skill",
    "test_general_skill",
    "list_model_catalog",
    "extract_sop_text",
    "model_prepare",
    "model_stream",
    "file_parse",
    "task_start",
    "task_status",
    "task_result",
    "task_cancel",
    "task_events",
    "read_sop_authority",
];

/// `PUBLIC_HOST_WIRE_HEADERS`: the only response headers the transport
/// copies from a provider response.
const WIRE_HEADERS: [&str; 6] = [
    "content-type",
    "etag",
    "retry-after",
    "x-request-id",
    "x-pilotdeck-model-id",
    "x-pilotdeck-provider-id",
];

/// The request-body budget `publicHostHttpTransport.ts` enforces per chunk
/// sum: strictly more than 32 MiB is the 413, exactly 32 MiB is served.
const INPUT_BUDGET: usize = 32 * 1024 * 1024;

/// The top-level input keys a module may not smuggle through its call input:
/// `PUBLIC_SCOPE_OVERRIDE`'s own-property list.
const SCOPE_OVERRIDE_KEYS: [&str; 7] = [
    "tenantId",
    "tenant_id",
    "actorUserId",
    "actor_user_id",
    "credentialId",
    "agentId",
    "agent_id",
];

/// The pause before a destruction error: one `Pending` poll makes hyper
/// flush whatever it has already queued (the TS `flushHeaders` whose head is
/// on the wire before `res.destroy()` runs), and the timer is what wakes the
/// connection for the error that then closes it.
const FLUSH_PAUSE: Duration = Duration::from_millis(1);

/// `PublicHostPrincipal`: the four non-empty trimmed strings the transport
/// demands before a request may reach a provider. The transport validates
/// and passes the whole principal through to the resolver and the provider
/// call; the ports installed in this crate key only on `agent_id` (the
/// scripted resolver's scenario) or nothing, and consuming the remaining
/// fields — the TS runtime adapter threads `options.principal` into its
/// runtime calls — belongs to the #1305 wiring lane.
pub(crate) struct PublicHostPrincipal {
    // Validated and forwarded, but nothing in this crate reads them yet:
    // the TS runtime adapter threads `options.principal` into its runtime
    // calls, which is the #1305 wiring lane's port surface.
    #[allow(dead_code)]
    pub pilot_deck_user_id: JsString,
    #[allow(dead_code)]
    pub tenant_id: JsString,
    #[allow(dead_code)]
    pub actor_user_id: JsString,
    pub agent_id: JsString,
}

/// One event of a streaming host response. `Value` is any other JS value; the
/// encoder accepts an object only under the declared NDJSON content type,
/// exactly `encodeHostCapabilityStream`.
pub(crate) enum HostEvent {
    Text(String),
    Bytes(Bytes),
    Value(JsValue),
}

/// The provider response body, one variant per TS branch of the response
/// serialization: `Value` is the `JSON.stringify` cell (including `null` and
/// `undefined`), `Bytes` is `body instanceof Uint8Array`, and `Stream` is the
/// `isHostCapabilityStreamBody` cell. The `rawBody` escape hatch is the
/// separate [`HostResponse::raw_body`] field, checked first like TS.
pub(crate) enum HostBody {
    Value(JsValue),
    Bytes(Bytes),
    Stream(mpsc::Receiver<Result<HostEvent, String>>),
}

pub(crate) struct HostResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: HostBody,
    /// `rawBody`: when present the transport ends the response with these
    /// bytes verbatim, before the Uint8Array branch, like TS.
    pub raw_body: Option<String>,
}

/// The provider `throw Object.assign(new Error(message), { status, code })`
/// channel the outer TS catch maps to `status ?? 502` +
/// `code ?? PUBLIC_HOST_TRANSPORT_FAILED`. `message: None` is a throw whose
/// error carries no message property, which the TS JSON body omits.
#[derive(Debug)]
pub(crate) struct HostFailure {
    pub status: Option<u16>,
    pub code: Option<String>,
    pub message: Option<String>,
}

/// A host capability provider: `operations` is the host's actual capability
/// list (describe is derived from it, so an advertised operation is always
/// callable — the fail-closed contract `ModuleRouter` follows too), and
/// `call` takes the already-validated `input` object.
pub(crate) trait PublicHostProvider: Send + Sync {
    fn operations(&self) -> Vec<String>;
    fn call(
        &self,
        operation: &str,
        input: &JsValue,
        principal: &PublicHostPrincipal,
    ) -> Pin<Box<dyn Future<Output = Result<HostResponse, HostFailure>> + Send + '_>>;
}

/// The TS `resolve(principal)` seam: the provider is chosen per request from
/// the validated principal. An absent resolver (a `Config` without
/// `capabilities`) is the transport's 501.
pub(crate) trait PublicHostResolver: Send + Sync {
    fn resolve(&self, principal: &PublicHostPrincipal) -> Arc<dyn PublicHostProvider>;
}

/// The transport's routing inputs: the bearer token and the optional
/// capability resolver. `None` mirrors TS's absent `options.resolve`.
#[derive(Clone, Default)]
pub(crate) struct Config {
    pub token: String,
    pub capabilities: Option<Arc<dyn PublicHostResolver>>,
}

/// Route one request through the module-host transport. `None` means the
/// request is not for this surface and the caller keeps routing, like TS
/// `handlePublicHostHttpRequest` returning `false`. An `Err` is a
/// zero-response connection close (a body read error, the transport's
/// equivalent of TS's aborted request destroying the response).
pub(crate) async fn serve(
    request: &mut hyper::Request<hyper::body::Incoming>,
    config: &Config,
) -> Option<Result<hyper::Response<Body>, io::Error>> {
    let version = request.version();
    let path = whatwg_path(
        request
            .uri()
            .path_and_query()
            .map_or("", |value| value.as_str()),
    );
    if path != "/api/module-host/describe" && path != "/api/module-host/call" {
        return None;
    }
    let keep_alive = !crate::mutants::on("module-host-connection-close-off")
        && version == hyper::Version::HTTP_11
        && !request_wants_close(request);
    // TS checks the bearer token before the method: a wrong-authorization GET
    // is a 401, not a 405.
    let authorized = crate::mutants::on("module-host-auth-off")
        || request
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .is_some_and(|value| value.as_bytes() == format!("Bearer {}", config.token).as_bytes());
    if !authorized {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::UNAUTHORIZED,
                "{\"code\":\"PUBLIC_HOST_TRANSPORT_UNAUTHORIZED\"}",
                keep_alive,
            ),
            version,
        )));
    }
    if !crate::mutants::on("module-host-method-off") && request.method() != hyper::Method::POST {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::METHOD_NOT_ALLOWED,
                "{\"code\":\"PUBLIC_HOST_METHOD_NOT_ALLOWED\"}",
                keep_alive,
            ),
            version,
        )));
    }
    let Some(resolver) = config.capabilities.clone() else {
        if crate::mutants::on("module-host-provider-off") {
            // The mutant answers with a provider that advertises nothing,
            // which diverges on the unconfigured 501 cell.
            return Some(Ok(finalize(
                send_json(hyper::StatusCode::OK, "{\"operations\":[]}", keep_alive),
                version,
            )));
        }
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::NOT_IMPLEMENTED,
                "{\"code\":\"PUBLIC_HOST_PORT_UNAVAILABLE\"}",
                keep_alive,
            ),
            version,
        )));
    };
    let input = match read_body(request.body_mut()).await {
        Ok(bytes) => bytes,
        Err(ReadError::OverBudget) => {
            return Some(Ok(finalize(
                send_json(
                    hyper::StatusCode::PAYLOAD_TOO_LARGE,
                    "{\"code\":\"PUBLIC_HOST_INPUT_TOO_LARGE\"}",
                    keep_alive,
                ),
                version,
            )));
        }
        // A body that errors before its declared end is a premature close;
        // TS's aborted request destroys the response with zero bytes.
        Err(ReadError::PrematureClose) => {
            return Some(Err(io::Error::other(
                "module-host request body ended prematurely",
            )));
        }
    };
    let Ok(parsed) = pilotdeck_protocol::parse_json(&String::from_utf8_lossy(&input)) else {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::BAD_REQUEST,
                "{\"code\":\"PUBLIC_HOST_INPUT_INVALID\"}",
                keep_alive,
            ),
            version,
        )));
    };
    let Some(principal) = principal_of(&parsed) else {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::BAD_REQUEST,
                "{\"code\":\"PUBLIC_HOST_PRINCIPAL_REQUIRED\"}",
                keep_alive,
            ),
            version,
        )));
    };
    let provider = resolver.resolve(&principal);
    let operations = if crate::mutants::on("module-host-describe-filter-off") {
        provider.operations()
    } else {
        provider
            .operations()
            .into_iter()
            .filter(|operation| WIRE_OPERATIONS.contains(&operation.as_str()))
            .collect::<Vec<_>>()
    };
    if path == "/api/module-host/describe" {
        let listed = format!(
            "{{\"operations\":{}}}",
            string_array(&operations.iter().map(String::as_str).collect::<Vec<_>>())
        );
        return Some(Ok(finalize(
            send_json(hyper::StatusCode::OK, &listed, keep_alive),
            version,
        )));
    }
    let operation = parsed
        .get("operation")
        .and_then(JsValue::as_string)
        .and_then(|text| text.to_utf8().ok());
    let listed = operation
        .as_ref()
        .is_some_and(|name| operations.iter().any(|available| available == name));
    if !crate::mutants::on("module-host-operation-gate-off") && !listed {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::CONFLICT,
                "{\"code\":\"PILOTDECK_HOST_CAPABILITY_UNAVAILABLE\"}",
                keep_alive,
            ),
            version,
        )));
    }
    let operation = operation.unwrap_or_default();
    let call_input = parsed.get("input").cloned().unwrap_or(JsValue::Undefined);
    let shape_ok = !crate::mutants::on("module-host-input-shape-off")
        && (!js_truthy(&call_input) || !matches!(&call_input, JsValue::Object(_)));
    if shape_ok {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::BAD_REQUEST,
                "{\"code\":\"PUBLIC_HOST_INPUT_INVALID\"}",
                keep_alive,
            ),
            version,
        )));
    }
    if !crate::mutants::on("module-host-scope-override-off")
        && let JsValue::Object(input) = &call_input
        && SCOPE_OVERRIDE_KEYS
            .iter()
            .any(|key| input.contains_key(key))
    {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::BAD_REQUEST,
                "{\"code\":\"PUBLIC_SCOPE_OVERRIDE\"}",
                keep_alive,
            ),
            version,
        )));
    }
    let response = match provider.call(&operation, &call_input, &principal).await {
        Ok(response) => response,
        Err(failure) => {
            let code = failure.code.unwrap_or_else(|| {
                if crate::mutants::on("module-host-failure-default-off") {
                    String::new()
                } else {
                    "PUBLIC_HOST_TRANSPORT_FAILED".to_owned()
                }
            });
            let body = match &failure.message {
                Some(message) => format!(
                    "{{\"code\":{},\"message\":{}}}",
                    json_text(&code),
                    json_text(message)
                ),
                None => format!("{{\"code\":{}}}", json_text(&code)),
            };
            return Some(Ok(finalize(
                send_json(
                    hyper::StatusCode::from_u16(failure.status.unwrap_or(502))
                        .unwrap_or(hyper::StatusCode::BAD_GATEWAY),
                    &body,
                    keep_alive,
                ),
                version,
            )));
        }
    };
    Some(Ok(finalize(
        provider_response(response, keep_alive),
        version,
    )))
}

/// Node answers every HTTP/1.0 request with a `Connection: close` response
/// on a closed socket, even one asking to keep alive; an explicit 1.0
/// response version keeps hyper's `fix_keep_alive` in its HTTP/1.0 branch —
/// the same #1209 trick (whose hyper `HTTP/1.0` status line against Node's
/// `HTTP/1.1` is the registered A8.4-NATIVE-HTTP-001 diff).
fn finalize(mut response: hyper::Response<Body>, version: hyper::Version) -> hyper::Response<Body> {
    if version == hyper::Version::HTTP_10 {
        *response.version_mut() = hyper::Version::HTTP_10;
    }
    response
}

enum ReadError {
    OverBudget,
    PrematureClose,
}

/// Read the request body under the TS byte budget, which is a per-chunk sum:
/// strictly more than 32 MiB total is the 413, exactly 32 MiB is served.
async fn read_body(body: &mut hyper::body::Incoming) -> Result<Vec<u8>, ReadError> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return Err(ReadError::PrematureClose);
        };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        let over = if crate::mutants::on("module-host-input-budget-boundary") {
            bytes.len() + data.len() >= INPUT_BUDGET
        } else {
            bytes.len() + data.len() > INPUT_BUDGET
        };
        if !crate::mutants::on("module-host-input-budget-off") && over {
            return Err(ReadError::OverBudget);
        }
        bytes.extend_from_slice(&data);
    }
    Ok(bytes)
}

/// A TS `writeHead` + `end` JSON response: unknown-length body (chunked,
/// because `writeHead` commits the headers before `end` knows the size), the
/// literal content-type, Node's Date, and the keep-alive pair, in Node's
/// header order.
fn send_json(status: hyper::StatusCode, body: &str, keep_alive: bool) -> hyper::Response<Body> {
    let mut builder = node_reason(hyper::Response::builder().status(status), status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .header(
            hyper::header::DATE,
            httpdate::fmt_http_date(SystemTime::now()),
        );
    builder = connection_headers(builder, keep_alive);
    builder
        .body(streamed(Some(Bytes::from(body.to_owned()))))
        .expect("#1349 valid transport response")
}

/// The provider-success response, one builder per TS `setHeader` + `end`
/// cell: the wire-header allowlist copy, the JSON content-type default, and
/// the body serialization branch. The response body of a known-length value
/// is Content-Length framed, like `end(payload)` with headers not yet sent.
fn provider_response(response: HostResponse, keep_alive: bool) -> hyper::Response<Body> {
    if !(100..600).contains(&response.status) {
        // The TS `Invalid host status` throw reaches the client as the
        // generic transport failure; the u16 re-validation keeps the same
        // cell reachable.
        return send_json(
            hyper::StatusCode::BAD_GATEWAY,
            "{\"code\":\"PUBLIC_HOST_TRANSPORT_FAILED\",\"message\":\"Invalid host status\"}",
            keep_alive,
        );
    }
    if let Some(message) = header_construction_error(&response.headers) {
        // `new Headers(response.headers)` throws before any header is
        // copied, so the TS catch answers the generic transport failure
        // with the undici message (Node v22.22.0, measured).
        let body = format!(
            "{{\"code\":\"PUBLIC_HOST_TRANSPORT_FAILED\",\"message\":{}}}",
            json_text(&message)
        );
        return send_json(hyper::StatusCode::BAD_GATEWAY, &body, keep_alive);
    }
    let content_type = header_value(&response.headers, "content-type");
    let streaming = matches!(response.body, HostBody::Stream(_));
    let stream_content_type = is_stream_content_type(content_type.as_deref());
    if streaming && !crate::mutants::on("module-host-stream-ct-off") && !stream_content_type {
        return send_json(
            hyper::StatusCode::BAD_GATEWAY,
            "{\"code\":\"PILOTDECK_HOST_RESPONSE_INVALID\",\"message\":\"Host stream content-type is required.\"}",
            keep_alive,
        );
    }
    if stream_content_type {
        return stream_content_type_response(response, keep_alive);
    }
    let provider_had_content_type = content_type.is_some();
    let (body, content_type) = if let Some(raw) = &response.raw_body {
        (Bytes::from(raw.clone()), content_type)
    } else {
        match &response.body {
            HostBody::Bytes(bytes) => (bytes.clone(), content_type),
            HostBody::Value(JsValue::String(text)) => {
                if content_type
                    .as_deref()
                    .is_some_and(|value| !value.contains("json"))
                {
                    (utf8_bytes(text), content_type)
                } else {
                    let text = stringify_json(&JsValue::String(text.clone()))
                        .expect("#1349 a string value is serializable")
                        .unwrap_or_default();
                    (Bytes::from(text), content_type)
                }
            }
            HostBody::Value(value) => (
                Bytes::from(
                    stringify_json(value)
                        .expect("#1349 a JSON value is serializable")
                        .unwrap_or_default(),
                ),
                content_type.or_else(|| {
                    if crate::mutants::on("module-host-json-ct-off") {
                        None
                    } else {
                        Some("application/json".to_owned())
                    }
                }),
            ),
            // Unreachable: a stream body without a stream content type is
            // answered above.
            HostBody::Stream(_) => (Bytes::new(), content_type),
        }
    };
    let status =
        hyper::StatusCode::from_u16(response.status).expect("#1349 validated provider status");
    let mut builder = node_reason(hyper::Response::builder().status(status), status);
    builder = copy_wire_headers(builder, &response.headers);
    // Node adds the default content-type only when the provider set none:
    // `setHeader` replaces, never duplicates, so the wire copy already holds
    // a provider content-type and the default lands after it.
    if let (false, Some(content_type)) = (provider_had_content_type, content_type) {
        builder = builder.header(hyper::header::CONTENT_TYPE, content_type);
    }
    builder = builder.header(
        hyper::header::DATE,
        httpdate::fmt_http_date(SystemTime::now()),
    );
    builder = connection_headers(builder, keep_alive);
    builder
        .body(
            Full::new(body)
                .map_err(|never: std::convert::Infallible| match never {})
                .boxed(),
        )
        .expect("#1349 valid provider response")
}

/// The stream-content-type branch, TS's `flushHeaders` + `pipeline` over
/// `encodeHostCapabilityStream`: headers go out first with chunked framing,
/// then the encoded events. A falsy body is the `Missing host stream` 502; a
/// truthy non-stream body iterates instead — a string one UTF-16 code unit
/// at a time, an array element by element, an empty Uint8Array zero events —
/// and anything else is not iterable and the response is destroyed after
/// its headers, which the TS pipeline error and the hyper body error both
/// render as the same header bytes with no terminator and a closed socket.
fn stream_content_type_response(response: HostResponse, keep_alive: bool) -> hyper::Response<Body> {
    if !matches!(response.body, HostBody::Stream(_))
        && !crate::mutants::on("module-host-missing-stream-off")
        && !body_truthy(&response.body)
    {
        return send_json(
            hyper::StatusCode::BAD_GATEWAY,
            "{\"code\":\"PUBLIC_HOST_TRANSPORT_FAILED\",\"message\":\"Missing host stream\"}",
            keep_alive,
        );
    }
    let ndjson = is_ndjson(&response.headers);
    let body: Body = match response.body {
        HostBody::Stream(receiver) => EncodedStreamBody {
            receiver,
            ndjson,
            destroying: None,
        }
        .boxed(),
        other => match host_events(&other) {
            Some(events) => QueuedBody {
                events: events.into(),
                ndjson,
                destroying: None,
            }
            .boxed(),
            // A truthy body that does not iterate (an object, a number, a
            // boolean) or iterates values the encoder rejects (a non-empty
            // Uint8Array's numbers): headers, no terminator, closed socket.
            None => DestroyedBody {
                pause: Box::pin(tokio::time::sleep(FLUSH_PAUSE)),
            }
            .boxed(),
        },
    };
    let status =
        hyper::StatusCode::from_u16(response.status).expect("#1349 validated provider status");
    let mut builder = node_reason(hyper::Response::builder().status(status), status);
    builder = copy_wire_headers(builder, &response.headers);
    builder = builder.header(
        hyper::header::DATE,
        httpdate::fmt_http_date(SystemTime::now()),
    );
    builder = connection_headers(builder, keep_alive);
    builder.body(body).expect("#1349 valid stream response")
}

/// Node writes two reason phrases this transport can answer differently from
/// hyper's table: `418 I'm a Teapot` (its capital `T`, where hyper's canonical
/// phrase is the lowercase `I'm a teapot` — oracle cell `fail-nomessage`, live
/// probe on Node v22.22.0) and `unknown` for a status without a standard
/// reason (measured `writeHead(499)`: `HTTP/1.1 499 unknown`; hyper's own
/// filler is `<none>`). The extension carries the Node phrase byte-exactly.
fn node_reason(
    mut builder: hyper::http::response::Builder,
    status: hyper::StatusCode,
) -> hyper::http::response::Builder {
    let phrase = if status == hyper::StatusCode::IM_A_TEAPOT {
        Some(hyper::ext::ReasonPhrase::from_static(b"I'm a Teapot"))
    } else if status.canonical_reason().is_none() {
        Some(hyper::ext::ReasonPhrase::from_static(b"unknown"))
    } else {
        None
    };
    if let Some(phrase) = phrase {
        builder = builder.extension(phrase);
    }
    builder
}

/// The TS iteration of a truthy non-stream body under a stream content type:
/// a string one UTF-16 code unit at a time, an array element by element
/// (a string element passes through, every other element reaches the
/// encoder), an empty Uint8Array zero events; anything else is not
/// iterable and destroys the response after its headers.
fn host_events(body: &HostBody) -> Option<Vec<HostEvent>> {
    match body {
        HostBody::Value(JsValue::String(text)) => Some(
            text.units()
                .to_vec()
                .into_iter()
                .map(|unit| HostEvent::Text(String::from_utf16_lossy(&[unit])))
                .collect(),
        ),
        HostBody::Value(JsValue::Array(items)) => Some(
            items
                .iter()
                .map(|item| match item {
                    JsValue::String(text) => {
                        HostEvent::Text(String::from_utf16_lossy(text.units()))
                    }
                    other => HostEvent::Value(other.clone()),
                })
                .collect(),
        ),
        HostBody::Bytes(bytes) if bytes.is_empty() => Some(Vec::new()),
        _ => None,
    }
}

/// A fixed event list as a hyper body: the queued `for await` events of a
/// non-stream body under a stream content type, encoded one frame at a
/// time so a rejected event destroys the response mid-stream like TS — the
/// frames already queued are flushed first ([`FLUSH_PAUSE`]), then the
/// destruction error closes the connection without a terminator.
struct QueuedBody {
    events: std::collections::VecDeque<HostEvent>,
    ndjson: bool,
    /// The armed destroy gate: the rejection message and its flush pause.
    destroying: Option<(String, Pin<Box<tokio::time::Sleep>>)>,
}
impl hyper::body::Body for QueuedBody {
    type Data = Bytes;
    type Error = crate::ProxyError;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, crate::ProxyError>>> {
        let this = self.get_mut();
        if let Some((message, pause)) = this.destroying.as_mut() {
            if pause.as_mut().poll(context).is_pending() {
                return std::task::Poll::Pending;
            }
            return std::task::Poll::Ready(Some(Err(rejected_event(message))));
        }
        while let Some(event) = this.events.pop_front() {
            match encode_event(event, this.ndjson) {
                Ok(bytes) if bytes.is_empty() => continue,
                Ok(bytes) => {
                    return std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes))));
                }
                Err(message) => {
                    let error = rejected_event(&message);
                    let mut pause = Box::pin(tokio::time::sleep(FLUSH_PAUSE));
                    let pending = pause.as_mut().poll(context).is_pending();
                    this.destroying = Some((message, pause));
                    if pending {
                        return std::task::Poll::Pending;
                    }
                    return std::task::Poll::Ready(Some(Err(error)));
                }
            }
        }
        std::task::Poll::Ready(None)
    }
}

/// The destroyed-response body: the first `Pending` poll flushes the queued
/// head ([`FLUSH_PAUSE`], TS `flushHeaders`), and the poll after the pause is
/// the destruction error that makes hyper close the connection with nothing
/// after the head — the byte shape a TS `res.destroy()` after `flushHeaders`
/// produces.
struct DestroyedBody {
    pause: Pin<Box<tokio::time::Sleep>>,
}
impl hyper::body::Body for DestroyedBody {
    type Data = Bytes;
    type Error = crate::ProxyError;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, crate::ProxyError>>> {
        if self.get_mut().pause.as_mut().poll(context).is_pending() {
            return std::task::Poll::Pending;
        }
        std::task::Poll::Ready(Some(Err(Box::new(io::Error::other(
            "module-host host stream body is not iterable",
        )))))
    }
}

/// The rejected-event destruction error both queued and provider streams
/// destroy the connection with.
fn rejected_event(message: &str) -> crate::ProxyError {
    Box::new(io::Error::other(format!(
        "module-host stream event rejected: {message}"
    )))
}

/// The stream-content-type branch's `!body`, TS truthiness: undefined, null,
/// `false`, `0`, and the empty string all miss the stream; a Uint8Array is
/// an object and never falsy.
fn body_truthy(body: &HostBody) -> bool {
    match body {
        HostBody::Stream(_) | HostBody::Bytes(_) => true,
        HostBody::Value(value) => js_truthy(value),
    }
}

/// `for (const header of PUBLIC_HOST_WIRE_HEADERS) if (headers.has(header))`:
/// only the allowlisted headers, in table order (the case-insensitive
/// `Headers` lookup over the provider's header list). A value Node's
/// `setHeader` rejects is the TS ERR_INVALID_CHAR catch cell.
fn copy_wire_headers(
    builder: hyper::http::response::Builder,
    headers: &[(String, String)],
) -> hyper::http::response::Builder {
    let mut builder = builder;
    if crate::mutants::on("module-host-wire-headers-off") {
        for (name, value) in headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        return builder;
    }
    for header in WIRE_HEADERS {
        if let Some(value) = header_value(headers, header) {
            builder = builder.header(header, value);
        }
    }
    builder
}

fn connection_headers(
    builder: hyper::http::response::Builder,
    keep_alive: bool,
) -> hyper::http::response::Builder {
    if keep_alive {
        builder
            .header(hyper::header::CONNECTION, "keep-alive")
            .header("keep-alive", "timeout=5")
    } else {
        builder.header(hyper::header::CONNECTION, "close")
    }
}

/// The `new Headers(response.headers)` validation, which undici performs
/// eagerly for every pair (a non-wire header included): a byte Node rejects
/// in a header value throws before any header is copied.
fn header_construction_error(headers: &[(String, String)]) -> Option<String> {
    for (name, value) in headers {
        if hyper::http::HeaderName::from_bytes(name.as_bytes()).is_err() {
            return Some(format!(
                "Headers.append: \"{name}\" is an invalid header name."
            ));
        }
        if hyper::http::HeaderValue::from_str(value).is_err() {
            return Some(format!(
                "Headers.append: \"{value}\" is an invalid header value."
            ));
        }
    }
    None
}

/// `new Headers(headers).get(name)`: case-insensitive, first value wins.
fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn is_stream_content_type(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|value| {
        value.starts_with("application/x-ndjson") || value.starts_with("text/event-stream")
    })
}

fn is_ndjson(headers: &[(String, String)]) -> bool {
    header_value(headers, "content-type")
        .as_deref()
        .is_some_and(|value| value.starts_with("application/x-ndjson"))
}

/// `input?.principal` with the TS shape check: truthy, and all four fields
/// (agentId included) non-empty strings after the JavaScript `trim` (the
/// model_catalog trim keeps the exact ECMAScript whitespace set).
fn principal_of(input: &JsValue) -> Option<PublicHostPrincipal> {
    let principal = input.get("principal")?;
    if !js_truthy(principal) {
        return None;
    }
    let field = |name: &str| -> Option<JsString> {
        let text = principal.get(name)?.as_string()?.clone();
        if !crate::mutants::on("module-host-principal-trim-off") && trim(&text).len_utf16() == 0 {
            return None;
        }
        Some(text)
    };
    Some(PublicHostPrincipal {
        pilot_deck_user_id: field("pilotDeckUserId")?,
        tenant_id: field("tenantId")?,
        actor_user_id: field("actorUserId")?,
        agent_id: field("agentId")?,
    })
}

/// JavaScript truthiness for a JSON value: every object and array, a non-empty
/// string, a non-zero non-NaN number, and `true`.
fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(bool) => *bool,
        JsValue::Number(number) => *number != 0.0 && !number.is_nan(),
        JsValue::String(text) => text.len_utf16() > 0,
        _ => true,
    }
}

/// The WHATWG path of the request target, `new URL(request.url,
/// "http://127.0.0.1").pathname` in TS: dot segments collapse, `%2e` is a
/// dot, fragments and queries split off. The same parse `native_http` uses
/// for its static-layer traversal defense; kept here so the component does
/// not depend on the #1209 routing module's private helper.
fn whatwg_path(target: &str) -> String {
    static BASE: std::sync::OnceLock<url::Url> = std::sync::OnceLock::new();
    let base = BASE.get_or_init(|| url::Url::parse("http://127.0.0.1").expect("valid base URL"));
    url::Url::options()
        .base_url(Some(base))
        .parse(target)
        .map_or_else(|_| "/".to_owned(), |parsed| parsed.path().to_owned())
}

/// Node's `Connection: close` request detection, copied from the #1209
/// router: every `Connection` header value is split on commas and a `close`
/// token (case-insensitive, trimmed) asks for a close.
fn request_wants_close(request: &hyper::Request<hyper::body::Incoming>) -> bool {
    request
        .headers()
        .get_all(hyper::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("close"))
}

/// `JSON.stringify` of a bare string: quoting and escapes for one value.
fn json_text(value: &str) -> String {
    pilotdeck_protocol::quote_json_string(&JsString::from(value))
}

/// `JSON.stringify` of a string array, `["a","b"]` or `[]`.
fn string_array(values: &[&str]) -> String {
    let mut out = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&json_text(value));
    }
    out.push(']');
    out
}

/// A JS string written with `res.end(string)`: UTF-8 with lone surrogates
/// replaced, Node's `Buffer.from(string)` behavior.
fn utf8_bytes(text: &JsString) -> Bytes {
    Bytes::from(String::from_utf16_lossy(text.units()))
}

/// Unknown-length single-frame body: the chunked framing a `writeHead` +
/// `end` response gets, like the #1209 `StreamedBody`.
struct StreamedBody(Option<Bytes>);
impl hyper::body::Body for StreamedBody {
    type Data = Bytes;
    type Error = crate::ProxyError;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, crate::ProxyError>>> {
        std::task::Poll::Ready(
            self.0
                .take()
                .map(|bytes| Ok(hyper::body::Frame::data(bytes))),
        )
    }
}

fn streamed(body: Option<Bytes>) -> Body {
    StreamedBody(body).boxed()
}

/// `encodeHostCapabilityStream` as a hyper body: strings and bytes pass
/// through, an object under the declared NDJSON content type is framed as
/// `JSON.stringify(chunk)\n`, and anything else — an object under SSE, an
/// array, a bare primitive — is the `PILOTDECK_HOST_RESPONSE_INVALID` event,
/// which after `flushHeaders` is a destroyed connection: the events already
/// queued are flushed ([`FLUSH_PAUSE`]), then the body error makes hyper close
/// with the same header bytes and abrupt close TS `res.destroy()` produces. A
/// provider stream error destroys the same way.
struct EncodedStreamBody {
    receiver: mpsc::Receiver<Result<HostEvent, String>>,
    ndjson: bool,
    /// The armed destroy gate: the failure message and its flush pause.
    destroying: Option<(String, Pin<Box<tokio::time::Sleep>>)>,
}
impl hyper::body::Body for EncodedStreamBody {
    type Data = Bytes;
    type Error = crate::ProxyError;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, crate::ProxyError>>> {
        if let Some((message, pause)) = self.destroying.as_mut() {
            if pause.as_mut().poll(context).is_pending() {
                return std::task::Poll::Pending;
            }
            return std::task::Poll::Ready(Some(Err(Box::new(io::Error::other(format!(
                "module-host stream failed: {message}"
            ))))));
        }
        loop {
            match self.receiver.poll_recv(context) {
                std::task::Poll::Pending => return std::task::Poll::Pending,
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                std::task::Poll::Ready(Some(Err(message))) => {
                    let error = Box::new(io::Error::other(format!(
                        "module-host stream failed: {message}"
                    )));
                    let mut pause = Box::pin(tokio::time::sleep(FLUSH_PAUSE));
                    let pending = pause.as_mut().poll(context).is_pending();
                    self.destroying = Some((message, pause));
                    if pending {
                        return std::task::Poll::Pending;
                    }
                    return std::task::Poll::Ready(Some(Err(error)));
                }
                std::task::Poll::Ready(Some(Ok(event))) => match encode_event(event, self.ndjson) {
                    Err(message) => {
                        let error = rejected_event(&message);
                        let mut pause = Box::pin(tokio::time::sleep(FLUSH_PAUSE));
                        let pending = pause.as_mut().poll(context).is_pending();
                        self.destroying = Some((message, pause));
                        if pending {
                            return std::task::Poll::Pending;
                        }
                        return std::task::Poll::Ready(Some(Err(error)));
                    }
                    Ok(bytes) => {
                        if !bytes.is_empty() {
                            return std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(
                                bytes,
                            ))));
                        }
                    }
                },
            }
        }
    }
}

/// The one-event encoder. The NDJSON newline is the mutant's own switch: a
/// JSON event without it diverges on every framed NDJSON cell.
fn encode_event(event: HostEvent, ndjson: bool) -> Result<Bytes, String> {
    match event {
        HostEvent::Text(text) => Ok(Bytes::from(text)),
        HostEvent::Bytes(bytes) => Ok(bytes),
        HostEvent::Value(value) => {
            if ndjson
                && !crate::mutants::on("module-host-ndjson-newline-off")
                && matches!(&value, JsValue::Object(_))
            {
                let Ok(Some(json)) = stringify_json(&value) else {
                    return Err("Host event must be JSON serializable.".to_owned());
                };
                let mut framed = json.into_bytes();
                framed.push(b'\n');
                Ok(Bytes::from(framed))
            } else {
                Err("Host stream requires bytes, strings, or declared NDJSON events.".to_owned())
            }
        }
    }
}

/// The native host provider: the operations the Rust host actually backs,
/// derived from the installed ports exactly like the TS native provider's
/// supported list is derived from the runtime it wraps. An uninstalled port
/// removes its operations from describe (fail-closed), and a direct call of
/// a gap operation gets the TS `unsupported` 501 shape.
pub(crate) struct ModuleHostPorts {
    pub skills: Option<SkillsPort>,
    pub catalog: Option<CatalogPort>,
}

/// The skills port the public host surface calls through, with the runtime
/// project key the TS `NativePublicHostRuntime.projectKey` carries: public
/// inputs without their own `projectKey` resolve against it.
pub(crate) struct SkillsPort {
    pub manager: Arc<crate::skills::SkillManager>,
    pub project_key: Option<JsString>,
}

/// The model-catalog port: the provider/model enumeration and the default
/// selection the public catalog cell is derived from, sourced from the same
/// config store the WS modelCatalog RPC reads.
pub(crate) struct CatalogPort {
    pub metadata: metadata::Metadata,
    pub env: crate::config::Env,
}

/// The resolver adapter the wiring lane (#1305) passes as
/// `Config::capabilities`: the ports behind one `Arc`, the same
/// installed-port provider for every principal.
pub(crate) struct ModuleHostResolver(pub(crate) Arc<ModuleHostPorts>);

impl PublicHostResolver for ModuleHostResolver {
    fn resolve(&self, _principal: &PublicHostPrincipal) -> Arc<dyn PublicHostProvider> {
        let ports: Arc<ModuleHostPorts> = Arc::clone(&self.0);
        ports
    }
}

impl PublicHostProvider for ModuleHostPorts {
    fn operations(&self) -> Vec<String> {
        let mut operations = Vec::new();
        if self.skills.is_some() {
            operations.push("list_general_skills");
            operations.push("import_general_skill");
        }
        if self.catalog.is_some() {
            operations.push("list_model_catalog");
        }
        operations.into_iter().map(String::from).collect()
    }

    fn call(
        &self,
        operation: &str,
        input: &JsValue,
        _principal: &PublicHostPrincipal,
    ) -> Pin<Box<dyn Future<Output = Result<HostResponse, HostFailure>> + Send + '_>> {
        let operation = operation.to_owned();
        let input = input.clone();
        Box::pin(async move {
            let provider_error = |message: String| HostResponse {
                status: 502,
                headers: Vec::new(),
                body: HostBody::Value(object_value(&[
                    ("code", string_value("PUBLIC_HOST_PROVIDER_ERROR")),
                    ("operation", string_value(&operation)),
                    ("message", string_value(&message)),
                ])),
                raw_body: None,
            };
            if !matches!(
                operation.as_str(),
                "list_general_skills" | "import_general_skill" | "list_model_catalog"
            ) {
                return Ok(unsupported(&operation));
            }
            let JsValue::Object(fields) = &input else {
                return Ok(HostResponse {
                    status: 400,
                    headers: Vec::new(),
                    body: HostBody::Value(object_value(&[
                        ("code", string_value("PUBLIC_HOST_INPUT_INVALID")),
                        ("message", string_value("input must be an object.")),
                    ])),
                    raw_body: None,
                });
            };
            match operation.as_str() {
                "list_general_skills" => {
                    let Some(port) = &self.skills else {
                        return Ok(unsupported(&operation));
                    };
                    let list_input = public_skills_list_input(fields, &port.project_key);
                    match port.manager.list(&list_input) {
                        Ok(result) => Ok(HostResponse {
                            status: 200,
                            headers: Vec::new(),
                            body: HostBody::Value(public_skills_list_result(&result)),
                            raw_body: None,
                        }),
                        Err(failure) => Ok(provider_error(String::from_utf16_lossy(
                            failure.message.units(),
                        ))),
                    }
                }
                "import_general_skill" => {
                    let Some(port) = &self.skills else {
                        return Ok(unsupported(&operation));
                    };
                    let payload = match fields.get("body") {
                        Some(value @ JsValue::Object(_)) => value.clone(),
                        _ => input.clone(),
                    };
                    let create_input = public_skills_create_input(&payload, &port.project_key);
                    match port.manager.create(&create_input) {
                        Ok(result) => Ok(HostResponse {
                            status: 200,
                            headers: Vec::new(),
                            body: HostBody::Value(result),
                            raw_body: None,
                        }),
                        Err(failure) => Ok(provider_error(String::from_utf16_lossy(
                            failure.message.units(),
                        ))),
                    }
                }
                _ => {
                    let Some(port) = &self.catalog else {
                        return Ok(unsupported(&operation));
                    };
                    match crate::config::load(&port.metadata, &port.env) {
                        Ok(config) => Ok(HostResponse {
                            status: 200,
                            headers: Vec::new(),
                            body: HostBody::Value(public_catalog_result(&config)),
                            raw_body: None,
                        }),
                        Err(failure) => Ok(provider_error(String::from_utf16_lossy(
                            failure.message.units(),
                        ))),
                    }
                }
            }
        })
    }
}

/// The TS `unsupported` response: a typed 501 naming the operation. It is
/// unreachable through the transport for the native provider (the operation
/// is not advertised, so the transport 409s first); it keeps a direct
/// call of a gap operation typed instead of void.
fn unsupported(operation: &str) -> HostResponse {
    HostResponse {
        status: 501,
        headers: Vec::new(),
        body: HostBody::Value(object_value(&[
            ("code", string_value("PUBLIC_HOST_CAPABILITY_UNAVAILABLE")),
            ("operation", string_value(operation)),
            (
                "message",
                string_value("No equivalent configured PilotDeck runtime capability is available."),
            ),
        ])),
        raw_body: None,
    }
}

/// The public list input, the TS `skills.list` mapping: the public
/// `scope`/`query`/`cursor`/`limit` fields and the runtime `projectKey`
/// (TS passes the runtime key unconditionally, not a per-request one).
fn public_skills_list_input(input: &JsObject, project_key: &Option<JsString>) -> JsValue {
    let mut mapped = JsObject::new();
    for key in ["scope", "query", "cursor", "limit"] {
        mapped.insert(
            key.into(),
            input.get(key).cloned().unwrap_or(JsValue::Undefined),
        );
    }
    mapped.insert(
        "projectKey".into(),
        project_key
            .clone()
            .map(JsValue::String)
            .unwrap_or(JsValue::Null),
    );
    JsValue::Object(mapped)
}

/// The public list result: `{data, next_cursor, builtin, user, project,
/// projectPath}` from the SkillManager shape (`next_cursor` is `null` when
/// absent, exactly the TS `result.nextCursor ?? null`).
fn public_skills_list_result(result: &JsValue) -> JsValue {
    let mut public = JsObject::new();
    public.insert(
        "data".into(),
        result
            .get("items")
            .cloned()
            .unwrap_or(JsValue::Array(vec![])),
    );
    public.insert(
        "next_cursor".into(),
        result.get("nextCursor").cloned().unwrap_or(JsValue::Null),
    );
    for key in ["builtin", "user", "project", "projectPath"] {
        public.insert(
            key.into(),
            result.get(key).cloned().unwrap_or(JsValue::Null),
        );
    }
    JsValue::Object(public)
}

/// The public import payload, the TS `skillInput` mapping: the public body
/// fields with `projectKey ?? runtime projectKey`.
fn public_skills_create_input(payload: &JsValue, project_key: &Option<JsString>) -> JsValue {
    let mut mapped = JsObject::new();
    for key in ["scope", "slug", "name", "description", "body", "content"] {
        mapped.insert(
            key.into(),
            payload.get(key).cloned().unwrap_or(JsValue::Undefined),
        );
    }
    let effective = match payload.get("projectKey") {
        Some(value @ JsValue::String(_)) => value.clone(),
        _ => project_key
            .clone()
            .map(JsValue::String)
            .unwrap_or(JsValue::Null),
    };
    mapped.insert("projectKey".into(), effective);
    JsValue::Object(mapped)
}

/// The public catalog result: `data` of `{id, name, model, provider,
/// enabled, is_default}` per provider/model pair, derived exactly like the
/// TS native provider derives it from its runtime modelConfig (every
/// provider and model, insertion order, always enabled).
fn public_catalog_result(config: &crate::config::Config) -> JsValue {
    let mut entries = Vec::new();
    for provider in &config.providers {
        for model in &provider.models {
            let mut entry = JsObject::new();
            entry.insert(
                "id".into(),
                string_value(&format!(
                    "{}/{}",
                    String::from_utf16_lossy(provider.id.units()),
                    String::from_utf16_lossy(model.id.units())
                )),
            );
            entry.insert("name".into(), JsValue::String(model.display_name.clone()));
            entry.insert("model".into(), JsValue::String(model.id.clone()));
            entry.insert("provider".into(), JsValue::String(provider.id.clone()));
            entry.insert("enabled".into(), JsValue::Bool(true));
            entry.insert(
                "is_default".into(),
                JsValue::Bool(config.selection.0 == provider.id && config.selection.1 == model.id),
            );
            entries.push(JsValue::Object(entry));
        }
    }
    let mut result = JsObject::new();
    result.insert("data".into(), JsValue::Array(entries));
    JsValue::Object(result)
}

fn object_value(fields: &[(&str, JsValue)]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert((*key).into(), value.clone());
    }
    JsValue::Object(object)
}

fn string_value(value: &str) -> JsValue {
    JsValue::String(JsString::from(value))
}

#[cfg(test)]
mod tests;
