//! The api-server listener: listener lifecycle plus the Node-parity HTTP
//! surface (`/health`, `/v1/models`, `/v1/chat/completions`, `OPTIONS *`).
//!
//! Transport is the #1250 Node-parity connection server, so request parsing,
//! keep-alive, graceful stop and zero-byte close on handler error are shared
//! with the rest of the gateway instead of re-implemented here.
use super::{
    MAX_REQUEST_BYTES,
    error_body::{self, ErrorBody},
    mapper::SessionMapper,
    render,
};
use crate::{http_proxy, metadata::object, runtime::memory::js_trim_units};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Frame, Incoming},
    header::{HeaderName, HeaderValue},
};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashSet,
    convert::Infallible,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{TcpListener, lookup_host},
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};
use url::Url;

const SESSION_HEADER: HeaderName = HeaderName::from_static("x-hermes-session-id");
const CORS_METHODS: &str = "GET, POST, OPTIONS, DELETE";
const CORS_HEADERS: &str = "Content-Type, Authorization, X-Hermes-Session-Id";
const CORS_EXPOSE: &str = "X-Hermes-Session-Id";

pub(crate) struct ServerShared {
    pub host: String,
    pub port: f64,
    pub api_key: String,
    pub model_name: String,
    pub cors_origins: Vec<String>,
    pub gateway: Option<Arc<dyn super::super::protocol::TurnGateway>>,
    pub mapper: Arc<SessionMapper>,
    pub active: Arc<Mutex<HashSet<String>>>,
    pub uuid: Arc<dyn Fn() -> String + Send + Sync>,
    pub timeout: Duration,
    pub log: Option<super::super::protocol::ChannelLog>,
}

impl ServerShared {
    fn log(&self, level: &str, message: &str) {
        if let Some(log) = self.log.clone() {
            log(level, message);
        }
    }
}

pub(crate) async fn accept_loop(
    listener: TcpListener,
    shared: Arc<ServerShared>,
    mut stop: watch::Receiver<bool>,
) {
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            joined = tasks.join_next(), if !tasks.is_empty() => { let _ = joined; }
            accepted = listener.accept() => match accepted {
                Ok((socket, _peer)) => {
                    let shared = shared.clone();
                    let stop = stop.clone();
                    tasks.spawn(async move {
                        let handler = move |request: Request<Incoming>, _lease| {
                            let shared = shared.clone();
                            async move { handle(shared, request).await }
                        };
                        let _ = http_proxy::serve_connection_native_with_shutdown(socket, handler, stop).await;
                    });
                }
                Err(_) => break,
            }
        }
    }
    drop(listener);
    while tasks.join_next().await.is_some() {}
}

pub(crate) async fn resolve_address(host: &str, port: f64) -> io::Result<std::net::SocketAddr> {
    if !port.is_finite() || port < 0.0 || port > 65535.0 || port.fract() != 0.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "options.port should be >= 0 and < 65536",
        ));
    }
    let host = if host.is_empty() { "0.0.0.0" } else { host };
    let mut addresses = lookup_host((host, port as u16)).await?;
    addresses
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "no address"))
}

pub(crate) fn format_port(port: f64) -> String {
    if port.is_nan() {
        "NaN".to_owned()
    } else if port.is_infinite() {
        if port.is_sign_negative() {
            "-Infinity".to_owned()
        } else {
            "Infinity".to_owned()
        }
    } else if port.fract() == 0.0 {
        format!("{}", port as i64)
    } else {
        port.to_string()
    }
}

/// JS `Number(value)` for the values `cfg.extra.port` can hold.
pub(crate) fn js_number(value: &JsValue) -> f64 {
    match value {
        JsValue::Undefined => f64::NAN,
        JsValue::Null => 0.0,
        JsValue::Bool(value) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        JsValue::Number(number) => *number,
        JsValue::String(text) => parse_number(&text.to_utf8().unwrap_or_default()),
        JsValue::Array(items) => {
            if items.is_empty() {
                0.0
            } else {
                parse_number(&items.iter().map(js_display).collect::<Vec<_>>().join(","))
            }
        }
        JsValue::Object(_) => f64::NAN,
    }
}

fn parse_number(text: &str) -> f64 {
    let text = crate::metadata::js_trim(text);
    if text.is_empty() {
        return 0.0;
    }
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, text.strip_prefix('+').unwrap_or(text)),
    };
    if rest == "Infinity" {
        return f64::INFINITY * sign;
    }
    let (radix, digits) =
        if let Some(digits) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            (16, digits)
        } else if let Some(digits) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B")) {
            (2, digits)
        } else if let Some(digits) = rest.strip_prefix("0o").or_else(|| rest.strip_prefix("0O")) {
            (8, digits)
        } else {
            (10, rest)
        };
    if radix != 10 {
        return match i128::from_str_radix(digits, radix) {
            Ok(value) => value as f64 * sign,
            Err(_) => f64::NAN,
        };
    }
    match rest.parse::<f64>() {
        Ok(value) => value * sign,
        Err(_) => f64::NAN,
    }
}

fn js_display(value: &JsValue) -> String {
    render::js_string_of(value)
        .to_utf8()
        .unwrap_or_else(|_| String::from_utf16_lossy(render::js_string_of(value).units()))
}

fn full(bytes: Vec<u8>) -> crate::Body {
    Full::new(Bytes::from(bytes))
        .map_err(|never: Infallible| match never {})
        .boxed()
}

fn json_text(value: &JsValue) -> String {
    stringify_json(value)
        .ok()
        .flatten()
        .unwrap_or_else(|| "null".to_owned())
}

fn json_response(status: StatusCode, value: &JsValue) -> Response<crate::Body> {
    let mut response = Response::new(full(json_text(value).into_bytes()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn empty_response(status: StatusCode) -> Response<crate::Body> {
    let mut response = Response::new(http_proxy::empty());
    *response.status_mut() = status;
    response
}

fn not_found() -> Response<crate::Body> {
    let mut response = Response::new(full(b"Not Found".to_vec()));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

/// Node joins repeated headers with `", "` (cookie is the only `"; "` case and
/// api-server never reads it). Header octets are latin1, as in Node.
fn header_string(headers: &hyper::HeaderMap, name: &str) -> String {
    let mut joined: Option<Vec<u8>> = None;
    for value in headers.get_all(name).iter() {
        let bytes = value.as_bytes();
        match joined.as_mut() {
            Some(joined) => {
                joined.extend_from_slice(b", ");
                joined.extend_from_slice(bytes);
            }
            None => joined = Some(bytes.to_vec()),
        }
    }
    match joined {
        Some(bytes) => bytes.iter().map(|byte| char::from(*byte)).collect(),
        None => String::new(),
    }
}

fn latin1_bytes(text: &str) -> Vec<u8> {
    text.chars()
        .map(|char| {
            if u32::from(char) <= 0xff {
                char as u8
            } else {
                b'?'
            }
        })
        .collect()
}

fn header_value(text: &str) -> HeaderValue {
    HeaderValue::from_bytes(&latin1_bytes(text)).unwrap_or(HeaderValue::from_static(""))
}

fn request_keep_alive(request: &Request<Incoming>) -> bool {
    let connection = header_string(request.headers(), "connection").to_ascii_lowercase();
    let has_close = connection.split(',').any(|token| token.trim() == "close");
    match request.version() {
        hyper::Version::HTTP_11 => !has_close,
        hyper::Version::HTTP_10 => connection
            .split(',')
            .any(|token| token.trim() == "keep-alive"),
        _ => false,
    }
}

/// Node adds `Date`, then (when it owns the connection header) `Connection`
/// and `Keep-Alive`. Explicit `Connection` values (SSE) are kept unless the
/// request closes the connection.
fn finish(mut response: Response<crate::Body>, keep_alive: bool) -> Response<crate::Body> {
    let headers = response.headers_mut();
    if !headers.contains_key(hyper::header::DATE) {
        headers.insert(
            hyper::header::DATE,
            header_value(&httpdate::fmt_http_date(SystemTime::now())),
        );
    }
    if keep_alive {
        if !headers.contains_key(hyper::header::CONNECTION) {
            headers.insert(
                hyper::header::CONNECTION,
                HeaderValue::from_static("keep-alive"),
            );
            headers.insert(
                HeaderName::from_static("keep-alive"),
                HeaderValue::from_static("timeout=5"),
            );
        }
    } else if !headers.contains_key(hyper::header::CONNECTION) {
        // A handler-set `Connection` wins: the SSE path explicitly asks for
        // `keep-alive` and Node kept it even when the request said `close`.
        headers.insert(hyper::header::CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

struct Cors {
    values: Vec<(&'static str, HeaderValue)>,
}

fn cors_headers(shared: &ServerShared, origin: &str) -> Option<Cors> {
    if origin.is_empty() || shared.cors_origins.is_empty() {
        return None;
    }
    let base = [
        ("access-control-allow-methods", CORS_METHODS),
        ("access-control-allow-headers", CORS_HEADERS),
        ("access-control-expose-headers", CORS_EXPOSE),
    ];
    if shared.cors_origins.iter().any(|entry| entry == "*") {
        let mut values = base
            .iter()
            .map(|(name, value)| (*name, HeaderValue::from_static(value)))
            .collect::<Vec<_>>();
        values.push(("access-control-allow-origin", HeaderValue::from_static("*")));
        return Some(Cors { values });
    }
    if shared.cors_origins.iter().any(|entry| entry == origin) {
        let mut values = base
            .iter()
            .map(|(name, value)| (*name, HeaderValue::from_static(value)))
            .collect::<Vec<_>>();
        values.push(("access-control-allow-origin", header_value(origin)));
        values.push(("vary", HeaderValue::from_static("Origin")));
        return Some(Cors { values });
    }
    None
}

fn apply_cors(response: &mut Response<crate::Body>, cors: &Option<Cors>) {
    if let Some(cors) = cors {
        for (name, value) in &cors.values {
            response
                .headers_mut()
                .insert(HeaderName::from_static(name), value.clone());
        }
    }
}

fn models_body(model: &str) -> JsValue {
    object([
        ("object", JsValue::from("list")),
        (
            "data",
            JsValue::Array(vec![object([
                ("id", JsValue::String(JsString::from(model))),
                ("object", JsValue::from("model")),
                ("created", JsValue::Number(created_seconds())),
                ("owned_by", JsValue::from("gateway")),
            ])]),
        ),
    ])
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn created_seconds() -> f64 {
    (now_millis() / 1000) as f64
}

pub(crate) async fn handle(
    shared: Arc<ServerShared>,
    request: Request<Incoming>,
) -> Result<Response<crate::Body>, io::Error> {
    let keep_alive = request_keep_alive(&request);
    let method = request.method().clone();
    let host = header_string(request.headers(), "host");
    let base_host = if host.is_empty() {
        format!("{}:{}", shared.host, format_port(shared.port))
    } else {
        host
    };
    let target = request.uri().to_string();
    let path = match Url::parse(&format!("http://{base_host}")) {
        Ok(base) => match Url::options().base_url(Some(&base)).parse(&target) {
            Ok(url) => url.path().to_owned(),
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid request target",
                ));
            }
        },
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid host header",
            ));
        }
    };
    let origin = header_string(request.headers(), "origin");
    let cors = cors_headers(&shared, &origin);

    let response = if method == Method::OPTIONS {
        empty_response(StatusCode::NO_CONTENT)
    } else if path == "/health" {
        json_response(
            StatusCode::OK,
            &object([
                ("status", JsValue::from("ok")),
                ("platform", JsValue::from("api-server")),
            ]),
        )
    } else if path == "/v1/models" {
        json_response(StatusCode::OK, &models_body(&shared.model_name))
    } else if path == "/v1/chat/completions" && method == Method::POST {
        chat_completions(&shared, request).await?
    } else {
        not_found()
    };
    let mut response = response;
    if method == Method::HEAD {
        // Node sends no Content-Length (and no body) for HEAD: the handler's
        // `res.end(body)` length is not advertised. Hyper would derive the
        // JSON length from the body's size hint, so drop the body here; the
        // HEAD framing rule then suppresses the implicit zero length too.
        *response.body_mut() = http_proxy::empty();
    }
    apply_cors(&mut response, &cors);
    Ok(finish(response, keep_alive))
}

enum ReadBodyError {
    TooLarge,
    Io(io::Error),
}

async fn read_body(mut body: Incoming) -> Result<Vec<u8>, ReadBodyError> {
    let mut out = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| {
            ReadBodyError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                error.to_string(),
            ))
        })?;
        if let Some(data) = frame.data_ref() {
            if out.len() + data.len() > MAX_REQUEST_BYTES {
                return Err(ReadBodyError::TooLarge);
            }
            out.extend_from_slice(data);
        }
    }
    Ok(out)
}

fn error_response(
    status: StatusCode,
    event: &str,
    message: &str,
    code: &str,
    kind: Option<&str>,
    user_hint: &str,
    scope: &str,
) -> Response<crate::Body> {
    json_response(
        status,
        &error_body::body(ErrorBody {
            event,
            message,
            code,
            status: status.as_u16(),
            kind,
            user_hint: Some(user_hint),
            scope,
        }),
    )
}

fn gateway_unavailable() -> Response<crate::Body> {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "gateway_unavailable",
        "Gateway not ready",
        "gateway_unavailable",
        Some("server_error"),
        "Start or reconnect the PilotDeck gateway, then retry.",
        "preflight",
    )
}

fn submit_failed(message: &str) -> Response<crate::Body> {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "channel_submit_failed",
        message,
        "channel_submit_failed",
        Some("server_error"),
        "PilotDeck failed before this API request could finish. Retry the request; if it repeats, check the API server and gateway logs.",
        "channel",
    )
}

struct ActiveChat {
    active: Arc<Mutex<HashSet<String>>>,
    chat_id: String,
}

impl Drop for ActiveChat {
    fn drop(&mut self) {
        self.active
            .lock()
            .expect("api_server active chats")
            .remove(&self.chat_id);
    }
}

fn js_truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => false,
        Some(JsValue::Bool(value)) => *value,
        Some(JsValue::Number(number)) => *number != 0.0 && !number.is_nan(),
        Some(JsValue::String(text)) => !text.is_empty(),
        Some(JsValue::Array(_)) | Some(JsValue::Object(_)) => true,
    }
}

/// `normalizeContent`: OpenAI content can be a string or a text-part array.
fn normalize_content(content: Option<&JsValue>) -> JsString {
    if !js_truthy(content) {
        return JsString::default();
    }
    match content.expect("checked truthy content") {
        JsValue::String(text) => text.clone(),
        JsValue::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                let value = match item {
                    JsValue::String(text) => Some(text.clone()),
                    JsValue::Object(part) => {
                        let kind = part
                            .get("type")
                            .and_then(JsValue::as_string)
                            .map(|value| value.equals_str("text") || value.equals_str("input_text"))
                            .unwrap_or(false);
                        if kind {
                            match part.get("text") {
                                Some(JsValue::Undefined) | Some(JsValue::Null) | None => {
                                    Some(JsString::default())
                                }
                                Some(text) => Some(render::js_string_of(text)),
                            }
                        } else {
                            Some(JsString::default())
                        }
                    }
                    _ => Some(JsString::default()),
                };
                if let Some(value) = value
                    && js_truthy(Some(&JsValue::String(value.clone())))
                {
                    parts.push(value);
                }
            }
            let mut units = Vec::new();
            for (index, part) in parts.iter().enumerate() {
                if index > 0 {
                    units.push(u16::from(b'\n'));
                }
                units.extend_from_slice(part.units());
            }
            JsString::from_utf16(units)
        }
        value => render::js_string_of(value),
    }
}

fn bearer_matches(header: &str, api_key: &str) -> bool {
    let Some(token) = header.strip_prefix("Bearer ") else {
        return false;
    };
    let token = crate::metadata::js_trim(token);
    let token = token.as_bytes();
    let key = api_key.as_bytes();
    if token.len() != key.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in token.iter().zip(key) {
        difference |= left ^ right;
    }
    difference == 0
}

async fn chat_completions(
    shared: &Arc<ServerShared>,
    request: Request<Incoming>,
) -> Result<Response<crate::Body>, io::Error> {
    let authorization = header_string(request.headers(), "authorization");
    let session_header =
        crate::metadata::js_trim(&header_string(request.headers(), "x-hermes-session-id"))
            .to_owned();
    if !shared.api_key.is_empty() && !bearer_matches(&authorization, &shared.api_key) {
        return Ok(error_response(
            StatusCode::UNAUTHORIZED,
            "api_auth_failed",
            "Invalid API key",
            "invalid_api_key",
            None,
            "Check the API key and retry.",
            "http",
        ));
    }

    let body = match read_body(request.into_body()).await {
        Ok(body) => body,
        Err(ReadBodyError::TooLarge) => {
            // TS calls `req.destroy()` right after rejecting; measured three
            // times, the client sees a zero-byte close and never a 413.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload too large",
            ));
        }
        Err(ReadBodyError::Io(error)) => {
            return Ok(error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "api_request_too_large",
                &format!("Request too large or unreadable: Error: {error}"),
                "request_too_large",
                None,
                "Reduce the request size and retry.",
                "http",
            ));
        }
    };
    let text = String::from_utf8_lossy(&body);
    let parsed = match parse_json(&text) {
        Ok(parsed) => parsed,
        Err(_) => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "api_invalid_json",
                "Invalid JSON",
                "invalid_json",
                None,
                "Send a valid JSON request body.",
                "http",
            ));
        }
    };
    let messages = match parsed.get("messages") {
        Some(JsValue::Array(items)) if !items.is_empty() => items,
        _ => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "api_messages_required",
                "messages is required",
                "messages_required",
                None,
                "Send at least one chat message.",
                "http",
            ));
        }
    };
    let last = messages.last().expect("checked non-empty messages");
    let content = if matches!(last, JsValue::Object(_)) {
        last.get("content")
    } else {
        None
    };
    let user_text = normalize_content(content);
    if user_text.is_empty() {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "api_empty_message",
            "Empty message",
            "empty_message",
            None,
            "Send a non-empty user message.",
            "http",
        ));
    }

    let chat_id = if session_header.is_empty() {
        format!("api-{}", (shared.uuid)())
    } else {
        session_header
    };
    let streaming = matches!(parsed.get("stream"), Some(JsValue::Bool(true)));

    if shared
        .active
        .lock()
        .expect("api_server active chats")
        .contains(&chat_id)
    {
        shared.log(
            "info",
            &format!("api-server: chat {chat_id} already active, rejecting"),
        );
        return Ok(error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "session_busy",
            "Session already processing a request",
            "session_busy",
            Some("rate_limit_error"),
            "Wait for the current request to finish or start a new session.",
            "session",
        ));
    }

    let user_text_utf8 = user_text
        .to_utf8()
        .unwrap_or_else(|_| String::from_utf16_lossy(user_text.units()));
    let mapped = shared.mapper.resolve(&chat_id, &user_text_utf8);
    if mapped.command_new && mapped.message.is_empty() {
        let ack = "已创建新会话。";
        if streaming {
            return Ok(ack_stream(shared, ack));
        }
        return Ok(json_response(
            StatusCode::OK,
            &completion_body(&shared.model_name, &JsString::from(ack)),
        ));
    }
    if mapped.message.is_empty() {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "api_empty_message",
            "Empty message",
            "empty_message",
            None,
            "Send a non-empty user message.",
            "http",
        ));
    }

    let Some(gateway) = shared.gateway.clone() else {
        return Ok(gateway_unavailable());
    };
    let session_key = JsString::from(mapped.session_key);
    let message = JsString::from(mapped.message);

    shared
        .active
        .lock()
        .expect("api_server active chats")
        .insert(chat_id.clone());
    let guard = ActiveChat {
        active: shared.active.clone(),
        chat_id: chat_id.clone(),
    };

    if streaming {
        let (sender, receiver) = mpsc::unbounded_channel();
        let driver_shared = shared.clone();
        let request = super::super::protocol::TurnRequest {
            session_key,
            channel_key: super::super::protocol::ChannelKey::new("api_server"),
            message,
        };
        let driver_chat = chat_id.clone();
        tokio::spawn(async move {
            stream_turn(driver_shared, gateway, request, driver_chat, sender, guard).await;
        });
        Ok(sse_response(&chat_id, receiver))
    } else {
        Ok(buffered_turn(shared, gateway, session_key, message, &chat_id, guard).await)
    }
}

fn completion_body(model: &str, content: &JsString) -> JsValue {
    object([
        (
            "id",
            JsValue::String(JsString::from(format!("chatcmpl-{}", now_millis()))),
        ),
        ("object", JsValue::from("chat.completion")),
        ("created", JsValue::Number(created_seconds())),
        ("model", JsValue::String(JsString::from(model))),
        (
            "choices",
            JsValue::Array(vec![object([
                ("index", JsValue::Number(0.0)),
                (
                    "message",
                    object([
                        ("role", JsValue::from("assistant")),
                        ("content", JsValue::String(content.clone())),
                    ]),
                ),
                ("finish_reason", JsValue::from("stop")),
            ])]),
        ),
        (
            "usage",
            object([
                ("prompt_tokens", JsValue::Number(0.0)),
                ("completion_tokens", JsValue::Number(0.0)),
                ("total_tokens", JsValue::Number(0.0)),
            ]),
        ),
    ])
}

fn chunk_body(model: &str, content: JsValue, finish: JsValue) -> JsValue {
    object([
        (
            "id",
            JsValue::String(JsString::from(format!("chatcmpl-{}", now_millis()))),
        ),
        ("object", JsValue::from("chat.completion.chunk")),
        ("created", JsValue::Number(created_seconds())),
        ("model", JsValue::String(JsString::from(model))),
        (
            "choices",
            JsValue::Array(vec![object([
                ("index", JsValue::Number(0.0)),
                ("delta", content),
                ("finish_reason", finish),
            ])]),
        ),
    ])
}

fn sse_chunk(model: &str, content: &JsString) -> Bytes {
    let body = chunk_body(
        model,
        object([("content", JsValue::String(content.clone()))]),
        JsValue::Null,
    );
    Bytes::from(format!("data: {}\n\n", json_text(&body)))
}

fn sse_done(model: &str) -> Bytes {
    let body = chunk_body(
        model,
        JsValue::Object(pilotdeck_protocol::JsObject::new()),
        JsValue::from("stop"),
    );
    Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", json_text(&body)))
}

fn sse_error(body: &JsValue) -> Bytes {
    Bytes::from(format!("data: {}\n\n", json_text(body)))
}

fn ack_stream(shared: &Arc<ServerShared>, ack: &str) -> Response<crate::Body> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let _ = sender.send(sse_chunk(&shared.model_name, &JsString::from(ack)));
    let _ = sender.send(sse_done(&shared.model_name));
    drop(sender);
    sse_response("", receiver)
}

fn sse_response(chat_id: &str, receiver: mpsc::UnboundedReceiver<Bytes>) -> Response<crate::Body> {
    let mut response = Response::new(
        ByteStream {
            receiver: Mutex::new(receiver),
        }
        .boxed(),
    );
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        hyper::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        hyper::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    headers.insert(
        hyper::header::CONNECTION,
        HeaderValue::from_static("keep-alive"),
    );
    if !chat_id.is_empty() {
        headers.insert(SESSION_HEADER, header_value(chat_id));
    }
    response
}

struct ByteStream {
    receiver: Mutex<mpsc::UnboundedReceiver<Bytes>>,
}

impl hyper::body::Body for ByteStream {
    type Data = Bytes;
    type Error = crate::ProxyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let mut receiver = self.receiver.lock().expect("api_server stream receiver");
        match receiver.poll_recv(context) {
            Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

async fn stream_turn(
    shared: Arc<ServerShared>,
    gateway: Arc<dyn super::super::protocol::TurnGateway>,
    request: super::super::protocol::TurnRequest,
    chat_id: String,
    sender: mpsc::UnboundedSender<Bytes>,
    _guard: ActiveChat,
) {
    let mut stream = match gateway.submit(request).await {
        Ok(stream) => stream,
        Err(error) => {
            shared.log(
                "error",
                &format!("api-server: stream submitTurn error: {error}"),
            );
            let body = error_body::body(ErrorBody {
                event: "channel_submit_failed",
                message: &error.to_string(),
                code: "channel_submit_failed",
                status: 500,
                kind: Some("server_error"),
                user_hint: Some(
                    "PilotDeck failed before this API request could finish. Retry the request; if it repeats, check the API server and gateway logs.",
                ),
                scope: "channel",
            });
            let _ = sender.send(sse_error(&body));
            return;
        }
    };
    let deadline = Instant::now() + shared.timeout;
    let mut visible_failure = false;
    loop {
        let next = tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                shared.log("warn", &format!("api-server: stream timeout for {chat_id}"));
                break;
            }
            next = stream.next() => next,
        };
        match next {
            Ok(Some(event)) => {
                if render::is_visible_failure_gateway_event(&event) {
                    visible_failure = true;
                }
                if render::event_type(&event).as_deref() == Some("error") && visible_failure {
                    continue;
                }
                if let Some(fragment) = render::render(&event)
                    && !fragment.is_empty()
                    && sender
                        .send(sse_chunk(&shared.model_name, &fragment))
                        .is_err()
                {
                    break;
                }
            }
            Ok(None) => {
                let _ = sender.send(sse_done(&shared.model_name));
                break;
            }
            Err(error) => {
                shared.log(
                    "error",
                    &format!("api-server: stream submitTurn error: {error}"),
                );
                let body = error_body::body(ErrorBody {
                    event: "channel_submit_failed",
                    message: &error.to_string(),
                    code: "channel_submit_failed",
                    status: 500,
                    kind: Some("server_error"),
                    user_hint: Some(
                        "PilotDeck failed before this API request could finish. Retry the request; if it repeats, check the API server and gateway logs.",
                    ),
                    scope: "channel",
                });
                let _ = sender.send(sse_error(&body));
                break;
            }
        }
    }
}

async fn buffered_turn(
    shared: &Arc<ServerShared>,
    gateway: Arc<dyn super::super::protocol::TurnGateway>,
    session_key: JsString,
    message: JsString,
    chat_id: &str,
    _guard: ActiveChat,
) -> Response<crate::Body> {
    let mut stream = match gateway
        .submit(super::super::protocol::TurnRequest {
            session_key,
            channel_key: super::super::protocol::ChannelKey::new("api_server"),
            message,
        })
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            shared.log("error", &format!("api-server: submitTurn error: {error}"));
            return submit_failed(&error.to_string());
        }
    };
    let warn_shared = shared.clone();
    let warn_chat = chat_id.to_owned();
    let timeout = shared.timeout;
    let timer = tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        warn_shared.log(
            "warn",
            &format!("api-server: buffered turn timeout for {warn_chat}"),
        );
    });
    let mut reply = Vec::new();
    let mut visible_failure = false;
    let mut error: Option<String> = None;
    loop {
        match stream.next().await {
            Ok(Some(event)) => {
                if render::is_visible_failure_gateway_event(&event) {
                    visible_failure = true;
                }
                if render::event_type(&event).as_deref() == Some("error") && visible_failure {
                    continue;
                }
                if let Some(fragment) = render::render(&event) {
                    reply.extend_from_slice(fragment.units());
                }
            }
            Ok(None) => break,
            Err(failure) => {
                error = Some(failure.to_string());
                break;
            }
        }
    }
    timer.abort();
    if let Some(error) = error {
        shared.log("error", &format!("api-server: submitTurn error: {error}"));
        return submit_failed(&error);
    }
    let content = JsString::from_utf16(js_trim_units(&reply).to_vec());
    let mut response = json_response(
        StatusCode::OK,
        &completion_body(&shared.model_name, &content),
    );
    response
        .headers_mut()
        .insert(SESSION_HEADER, header_value(chat_id));
    response
}
