//! exp008 / plan 003 Issue B1: minimal Rust reverse proxy in front of the TS
//! `ui/server`. HTTP (incl. streaming SSE/NDJSON) and whole WebSocket
//! connections are relayed byte-for-byte. This is throwaway probe code: it
//! lives under `scripts/`, never under `src/`, and is not product code.
//!
//! Behaviour mirrors the A1 transparent proxy where it matters:
//! * hop-by-hop headers only, no synthetic `X-Forwarded-*`;
//! * request/response bodies are streamed, never buffered;
//! * a whole WS connection owns exactly one upstream connection; frames are
//!   not parsed or rewritten; close is propagated in both directions;
//! * every relayed WS frame is teed to a JSONL log (direction + monotonic
//!   sequence + raw payload).
//!
//! All proxied traffic is tagged `X-PilotDeck-Handler: <label>` (B1: `ts`) so a
//! recording layer can later prove which side answered.
use std::{
    convert::Infallible,
    error::Error,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::{
    body::Incoming, header, server::conn::http1, service::service_fn, Request, Response, StatusCode,
};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::{TokioExecutor, TokioIo},
};
use tokio::{io::AsyncWriteExt, net::TcpListener};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        handshake::derive_accept_key,
        protocol::{
            frame::{
                coding::{Data, OpCode},
                Frame,
            },
            Role, WebSocketConfig,
        },
        ClientRequestBuilder, Message,
    },
    WebSocketStream,
};

type ProxyError = Box<dyn Error + Send + Sync>;
type Body = BoxBody<Bytes, ProxyError>;
type HttpClient = Client<HttpConnector, Body>;

#[path = "../../../ui-server-parity/recording.rs"]
mod recording;
use recording::{RecordedBody, Recorder, WireTap};

// Match the A1 bounds: buffer at most a complete (possibly fragmented) message.
const MAX_WS_MESSAGE: usize = 16 * 1024 * 1024;
const MAX_WS_FRAME: usize = 4 * 1024 * 1024;
struct FrameLog {
    recorder: Arc<Recorder>,
    connection: u64,
}
impl FrameLog {
    fn open(path: &str) -> std::io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            recorder: Recorder::open(path)?,
            connection: 0,
        }))
    }
    async fn tee(
        &self,
        dir: &str,
        payload: &[u8],
        opcode: &str,
        close: Option<(u16, &str)>,
    ) -> Result<(), ProxyError> {
        let mut entry = serde_json::json!({"kind":"ws_message", "connection":self.connection,
            "dir":dir, "opcode":opcode, "bytes":payload.len(), "payload_b64":STANDARD.encode(payload),
            "raw": if opcode == "text" { String::from_utf8_lossy(payload).into_owned() } else { STANDARD.encode(payload) }});
        if let Some((code, reason)) = close {
            entry["close"] = serde_json::json!({"code":code,"reason":reason});
        }
        self.recorder.write(entry)
    }
}

struct Proxy {
    upstream: String,
    upstream_ws_authority: String,
    client: HttpClient,
    handler: String,
    frames: Option<Arc<FrameLog>>,
    recorder: Option<Arc<Recorder>>,
    next_id: AtomicU64,
}

#[tokio::main]
async fn main() -> Result<(), ProxyError> {
    let listen: SocketAddr = std::env::var("EXP008_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:13001".into())
        .parse()?;
    // Loopback is enforced by binding, not by path or token.
    if listen.ip() != Ipv4Addr::LOCALHOST {
        return Err("proxy must listen on 127.0.0.1".into());
    }
    let upstream =
        std::env::var("EXP008_UPSTREAM").unwrap_or_else(|_| "http://127.0.0.1:13002".into());
    let uri: hyper::Uri = upstream.parse()?;
    if uri.scheme_str() != Some("http") || uri.authority().is_none() || uri.path() != "/" {
        return Err("upstream must be an http origin (no path)".into());
    }
    let authority = uri.authority().expect("validated").as_str().to_owned();
    let handler = std::env::var("EXP008_HANDLER").unwrap_or_else(|_| "ts".into());
    let frames = match std::env::var("EXP008_WS_LOG") {
        Ok(path) if !path.is_empty() => Some(FrameLog::open(&path)?),
        _ => None,
    };
    let recorder = std::env::var("EXP010_RECORD_LOG")
        .ok()
        .filter(|p| !p.is_empty())
        .map(|p| Recorder::open(&p))
        .transpose()?;
    let proxy = Arc::new(Proxy {
        upstream: upstream.trim_end_matches('/').to_owned(),
        upstream_ws_authority: authority,
        client: Client::builder(TokioExecutor::new()).build_http(),
        handler,
        frames,
        recorder,
        next_id: AtomicU64::new(1),
    });
    if let Some(log) = &proxy.recorder {
        log.write(serde_json::json!({"kind":"run_start", "schema":1, "listen":listen.to_string(), "upstream":proxy.upstream, "clock":"monotonic nanoseconds since recorder start", "body_encoding":"base64"}))?;
    }
    let listener = TcpListener::bind(listen).await?;
    eprintln!("exp008 proxy listening {listen} -> {}", proxy.upstream);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut health_tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = match accepted {
                    Ok(connection) => connection,
                    Err(error) => {
                        eprintln!("downstream accept: {error}");
                        if fatal_accept_error(&error) {
                            return Err(error.into());
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let proxy = proxy.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        let proxy = proxy.clone();
                        async move { Ok::<_, Infallible>(proxy.serve(request).await) }
                    });
                    if let Err(error) = http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .with_upgrades()
                        .await
                    {
                        eprintln!("downstream HTTP: {error}");
                    }
                });
            }
            _ = health_tick.tick() => {
                if proxy.recorder.as_ref().is_some_and(|r| r.failed.load(Ordering::SeqCst)) || proxy.frames.as_ref().is_some_and(|r| r.recorder.failed.load(Ordering::SeqCst)) {
                    return Err("recording writer failed; evidence incomplete".into());
                }
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
        }
    }
    if let Some(log) = &proxy.recorder {
        log.write(serde_json::json!({"kind":"run_end"}))?;
    }
    Ok(())
}

impl Proxy {
    fn local_response(&self, id: u64, status: StatusCode, message: &'static str) -> Response<Body> {
        if let Some(log) = &self.recorder {
            let _ = log.write(serde_json::json!({"kind":"recorder_error", "id":id,
                "status":status.as_u16(), "headers":[], "body_b64":STANDARD.encode(message.as_bytes())}));
        }
        plain(status, message)
    }
    async fn serve(&self, mut request: Request<Incoming>) -> Response<Body> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        if let Some(log) = &self.recorder {
            if log.write(serde_json::json!({"kind":"http_request", "id":id, "method":request.method().as_str(),
                "path":request.uri().path(), "query":recording::query(request.uri().query()),
                "case":request.headers().get("x-pdrs-parity-case").and_then(|v| v.to_str().ok()),
                "headers":recording::headers(request.headers(), true)})).is_err() {
                return self.local_response(id, StatusCode::INTERNAL_SERVER_ERROR, "recording failed");
            }
        }
        // Validate before either HTTP or WS forwarding, including non-ASCII values.
        if connection_names(request.headers()).is_err() {
            return self.local_response(id, StatusCode::BAD_REQUEST, "invalid Connection header");
        }
        let is_upgrade = request
            .headers()
            .get(header::UPGRADE)
            .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"));
        if is_upgrade {
            let response = if self.recorder.is_some() {
                self.recorded_websocket(&mut request, id).await
            } else {
                self.websocket(&mut request, id).await
            };
            return match response {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("WS pairing: {error}");
                    self.local_response(
                        id,
                        StatusCode::BAD_GATEWAY,
                        "upstream websocket unavailable",
                    )
                }
            };
        }
        let path = request
            .uri()
            .path_and_query()
            .map_or("/", |value| value.as_str());
        let uri: hyper::Uri = match format!("{}{path}", self.upstream).parse() {
            Ok(uri) => uri,
            Err(_) => {
                return self.local_response(id, StatusCode::BAD_REQUEST, "invalid request URI")
            }
        };
        let host = uri
            .authority()
            .expect("validated upstream")
            .as_str()
            .to_owned();
        *request.uri_mut() = uri;
        strip_hop_headers(request.headers_mut()).expect("validated Connection");
        request
            .headers_mut()
            .insert(header::HOST, host.parse().expect("URI authority"));
        let request = request.map(|body| -> Body {
            match &self.recorder {
                Some(log) => RecordedBody::new(body, log.clone(), id, "request").boxed(),
                None => body
                    .map_err(|error| -> ProxyError { Box::new(error) })
                    .boxed(),
            }
        });
        match self.client.request(request).await {
            Ok(mut response) => {
                if let Some(log) = &self.recorder {
                    if log.write(serde_json::json!({"kind":"http_response", "id":id, "status":response.status().as_u16(),
                        "headers":recording::headers(response.headers(), false), "handler":response.headers().get("x-pilotdeck-handler").and_then(|v| v.to_str().ok())})).is_err() {
                        return self.local_response(id, StatusCode::INTERNAL_SERVER_ERROR, "recording failed");
                    }
                }
                if strip_hop_headers(response.headers_mut()).is_err() {
                    return self.local_response(
                        id,
                        StatusCode::BAD_GATEWAY,
                        "invalid upstream Connection header",
                    );
                }
                if self.recorder.is_none() {
                    if let Ok(value) = self.handler.parse() {
                        response.headers_mut().insert("x-pilotdeck-handler", value);
                    }
                }
                response.map(|body| {
                    if let Some(log) = &self.recorder {
                        return RecordedBody::new(body, log.clone(), id, "response").boxed();
                    }
                    body.map_err(|error| -> ProxyError { Box::new(error) })
                        .boxed()
                })
            }
            Err(error) => {
                eprintln!("HTTP upstream: {error}");
                self.local_response(id, StatusCode::BAD_GATEWAY, "upstream HTTP unavailable")
            }
        }
    }

    // The recorder must not generate its own Pong/Close or reassemble fragments.
    // Hyper retains handshake-tail bytes in both Upgrade objects. After the two
    // handshakes, the client-masked / server-unmasked frames already have the
    // correct polarity for the other leg, so raw duplex forwarding is sufficient.
    async fn recorded_websocket(
        &self,
        request: &mut Request<Incoming>,
        id: u64,
    ) -> Result<Response<Body>, ProxyError> {
        let Some(accept) = websocket_accept(request.method(), request.headers()) else {
            return Ok(self.local_response(
                id,
                StatusCode::BAD_REQUEST,
                "invalid websocket upgrade",
            ));
        };
        let log = self.recorder.as_ref().expect("recording mode").clone();
        log.write(
            serde_json::json!({"kind":"http_end", "id":id,"direction":"request", "offset":0}),
        )?;
        let path = request.uri().path_and_query().map_or("/", |v| v.as_str());
        let uri: hyper::Uri = format!("{}{path}", self.upstream).parse()?;
        let mut forwarded = Request::new(empty());
        *forwarded.uri_mut() = uri;
        *forwarded.headers_mut() = request.headers().clone();
        strip_hop_headers(forwarded.headers_mut())?;
        forwarded
            .headers_mut()
            .remove(header::SEC_WEBSOCKET_EXTENSIONS);
        forwarded
            .headers_mut()
            .insert(header::HOST, self.upstream_ws_authority.parse()?);
        forwarded.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("Upgrade"),
        );
        forwarded.headers_mut().insert(
            header::UPGRADE,
            header::HeaderValue::from_static("websocket"),
        );
        let mut response =
            tokio::time::timeout(Duration::from_secs(10), self.client.request(forwarded)).await??;
        log.write(serde_json::json!({"kind":"http_response","id":id,"connection":id,"status":response.status().as_u16(),
            "headers":recording::headers(response.headers(), false),
            "handler":response.headers().get("x-pilotdeck-handler").and_then(|v| v.to_str().ok())}))?;
        connection_names(response.headers())?;
        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
            strip_hop_headers(response.headers_mut())?;
            return Ok(response.map(|body| RecordedBody::new(body, log, id, "response").boxed()));
        }
        if response
            .headers()
            .get(header::SEC_WEBSOCKET_ACCEPT)
            .is_none_or(|v| v.as_bytes() != accept.as_bytes())
            || response
                .headers()
                .contains_key(header::SEC_WEBSOCKET_EXTENSIONS)
        {
            return Err("invalid/unsupported upstream websocket handshake".into());
        }
        let upstream = hyper::upgrade::on(&mut response);
        let downstream = hyper::upgrade::on(request);
        let response = websocket_response(response.into_parts().0.headers, &accept)?;
        tokio::spawn(async move {
            let result = async {
                let (upstream, downstream) = tokio::try_join!(upstream, downstream)?;
                let mut upstream =
                    WireTap::new(TokioIo::new(upstream), Some(log.clone()), id, "upstream");
                let mut downstream = WireTap::new(
                    TokioIo::new(downstream),
                    Some(log.clone()),
                    id,
                    "downstream",
                );
                tokio::io::copy_bidirectional(&mut upstream, &mut downstream).await?;
                Ok::<(), ProxyError>(())
            }
            .await;
            let _ = log.write(serde_json::json!({"kind":"ws_end", "connection":id, "error":result.err().map(|e| e.to_string())}));
        });
        Ok(response)
    }

    async fn websocket(
        &self,
        request: &mut Request<Incoming>,
        id: u64,
    ) -> Result<Response<Body>, ProxyError> {
        let Some(accept) = websocket_accept(request.method(), request.headers()) else {
            return Ok(self.local_response(
                id,
                StatusCode::BAD_REQUEST,
                "invalid websocket upgrade",
            ));
        };
        let path = request
            .uri()
            .path_and_query()
            .map_or("/", |value| value.as_str());
        let url: hyper::Uri = format!("ws://{}{path}", self.upstream_ws_authority).parse()?;
        // Forward the downstream handshake's end-to-end headers. The baseline
        // authenticates `/ws` from `Authorization: Bearer` as well as `?token=`
        // (ui/server/index.js:490-496); building the upstream from the URL alone
        // dropped the header and turned a direct 101 into a proxy 502. The
        // relay generates its own hop-by-hop / WS handshake headers, so those
        // are skipped, and `Sec-WebSocket-Extensions` is not forwarded because
        // this probe terminates WS framing on both legs and cannot honour a
        // negotiated extension.
        let builder = websocket_request(url, request.headers())?;
        let config = websocket_config();
        let (mut upstream, upstream_response) = tokio::time::timeout(
            Duration::from_secs(10),
            connect_async_with_config(builder, Some(config), false),
        )
        .await??;
        let upgraded = hyper::upgrade::on(request);
        let frame_recorder = self
            .recorder
            .clone()
            .or_else(|| self.frames.as_ref().map(|f| f.recorder.clone()));
        let frames = frame_recorder.map(|recorder| {
            Arc::new(FrameLog {
                recorder,
                connection: id,
            })
        });
        if let Some(log) = &self.recorder {
            log.write(serde_json::json!({"kind":"http_response", "id":id,"connection":id,"status":101,
                "headers":recording::headers(upstream_response.headers(), false),
                "handler":upstream_response.headers().get("x-pilotdeck-handler").and_then(|v| v.to_str().ok())}))?;
        }
        connection_names(upstream_response.headers())?;
        tokio::spawn(async move {
            match upgraded.await {
                Ok(io) => {
                    let mut downstream = WebSocketStream::from_raw_socket(
                        TokioIo::new(io),
                        Role::Server,
                        Some(config),
                    )
                    .await;
                    loop {
                        let result = tokio::select! {
                            frame = downstream.next() => {
                                let closing = matches!(&frame, Some(Ok(Message::Close(_))));
                                let result = relay(frame, &mut upstream, frames.as_deref(), "c2u").await;
                                if closing && result.is_ok() {
                                    if let Err(error) = relay_close_reply(&mut upstream, &mut downstream, frames.as_deref()).await {
                                        eprintln!("WS close reply: {error}");
                                    }
                                }
                                result
                            },
                            frame = upstream.next() => {
                                relay(frame, &mut downstream, frames.as_deref(), "u2c").await
                            },
                        };
                        // tungstenite queues an automatic Pong when it reads a
                        // Ping. Flush both legs every round so a long idle link
                        // does not leave that Pong buffered until close.
                        let _ = upstream.flush().await;
                        let _ = downstream.flush().await;
                        match result {
                            Ok(true) => continue,
                            Ok(false) => break,
                            Err(error) => {
                                eprintln!("WS relay: {error}");
                                break;
                            }
                        }
                    }
                }
                Err(error) => eprintln!("downstream upgrade: {error}"),
            }
            if let Some(frames) = &frames {
                let _ = frames
                    .recorder
                    .write(serde_json::json!({"kind":"ws_end", "connection":id}));
            }
        });
        let mut response = websocket_response(upstream_response.into_parts().0.headers, &accept)?;
        if self.recorder.is_none() {
            if let Ok(value) = self.handler.parse() {
                response.headers_mut().insert("x-pilotdeck-handler", value);
            }
        }
        Ok(response)
    }
}

async fn tee_message(
    frames: Option<&FrameLog>,
    dir: &str,
    message: &Message,
) -> Result<(), ProxyError> {
    let Some(frames) = frames else {
        return Ok(());
    };
    match message {
        Message::Text(text) => frames.tee(dir, text.as_bytes(), "text", None).await?,
        Message::Binary(data) => frames.tee(dir, data, "binary", None).await?,
        Message::Ping(data) => frames.tee(dir, data, "ping", None).await?,
        Message::Pong(data) => frames.tee(dir, data, "pong", None).await?,
        Message::Close(close) => {
            let (code, reason) = close
                .as_ref()
                .map(|frame| (u16::from(frame.code), frame.reason.as_ref()))
                .unwrap_or((1005, ""));
            frames.tee(dir, &[], "close", Some((code, reason))).await?;
        }
        Message::Frame(_) => {}
    }
    Ok(())
}

async fn relay_close_reply<U, D>(
    upstream: &mut WebSocketStream<U>,
    downstream: &mut WebSocketStream<D>,
    frames: Option<&FrameLog>,
) -> Result<(), ProxyError>
where
    U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    D: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(frame) = upstream.next().await {
            let frame = frame?;
            tee_message(frames, "u2c", &frame).await?;
            let closing = frame.is_close();
            let frame = match frame {
                Message::Text(data) => Frame::message(data, OpCode::Data(Data::Text), true),
                Message::Binary(data) => Frame::message(data, OpCode::Data(Data::Binary), true),
                Message::Ping(data) => Frame::ping(data),
                Message::Pong(data) => Frame::pong(data),
                Message::Close(close) => Frame::close(close),
                Message::Frame(frame) => frame,
            };
            let mut wire = Vec::new();
            frame.format(&mut wire)?;
            downstream.get_mut().write_all(&wire).await?;
            if closing {
                return Ok::<_, ProxyError>(());
            }
        }
        Err("upstream ended without a close reply".into())
    })
    .await?
}

fn fatal_accept_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::InvalidInput
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::Unsupported
    )
}

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_WS_MESSAGE))
        .max_frame_size(Some(MAX_WS_FRAME))
}

fn websocket_accept(method: &hyper::Method, headers: &hyper::HeaderMap) -> Option<String> {
    let key = headers.get(header::SEC_WEBSOCKET_KEY)?;
    if method != hyper::Method::GET
        || headers
            .get(header::SEC_WEBSOCKET_VERSION)
            .is_none_or(|value| value != "13")
        || !connection_names(headers).ok()?.contains(&header::UPGRADE)
        || STANDARD.decode(key.as_bytes()).ok()?.len() != 16
    {
        return None;
    }
    Some(derive_accept_key(key.as_bytes()))
}

fn websocket_request(
    url: hyper::Uri,
    headers: &hyper::HeaderMap,
) -> Result<Request<()>, ProxyError> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut forwarded = headers.clone();
    strip_hop_headers(&mut forwarded)?;
    let mut request = ClientRequestBuilder::new(url).into_client_request()?;
    for (name, value) in &forwarded {
        if matches!(
            name.as_str(),
            "host" | "sec-websocket-key" | "sec-websocket-version" | "sec-websocket-extensions"
        ) {
            continue;
        }
        request.headers_mut().append(name, value.clone());
    }
    Ok(request)
}

fn websocket_response(
    mut headers: hyper::HeaderMap,
    accept: &str,
) -> Result<Response<Body>, ProxyError> {
    strip_hop_headers(&mut headers)?;
    headers.insert(
        header::UPGRADE,
        hyper::header::HeaderValue::from_static("websocket"),
    );
    headers.insert(
        header::CONNECTION,
        hyper::header::HeaderValue::from_static("Upgrade"),
    );
    headers.insert(
        header::SEC_WEBSOCKET_ACCEPT,
        accept.parse().expect("derived accept key"),
    );
    let mut response = Response::new(empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    *response.headers_mut() = headers;
    Ok(response)
}

async fn relay<S>(
    frame: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    target: &mut WebSocketStream<S>,
    frames: Option<&FrameLog>,
    dir: &str,
) -> Result<bool, ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Some(frame) = frame else {
        return Ok(false);
    };
    let frame = frame?;
    tee_message(frames, dir, &frame).await?;
    let closing = frame.is_close();
    target.send(frame).await?;
    Ok(!closing)
}

fn connection_names(headers: &hyper::HeaderMap) -> Result<Vec<header::HeaderName>, ProxyError> {
    let mut names = Vec::new();
    for value in headers.get_all(header::CONNECTION) {
        for part in value.as_bytes().split(|b| *b == b',') {
            let start = part
                .iter()
                .position(|b| !matches!(b, b' ' | b'\t'))
                .unwrap_or(part.len());
            let end = part
                .iter()
                .rposition(|b| !matches!(b, b' ' | b'\t'))
                .map_or(start, |p| p + 1);
            if start == end {
                return Err("empty Connection token".into());
            }
            names.push(header::HeaderName::from_bytes(&part[start..end])?);
        }
    }
    Ok(names)
}
fn strip_hop_headers(headers: &mut hyper::HeaderMap) -> Result<(), ProxyError> {
    for name in connection_names(headers)? {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
    Ok(())
}

fn empty() -> Body {
    Full::new(Bytes::new())
        .map_err(|never: Infallible| match never {})
        .boxed()
}
fn plain(status: StatusCode, message: &'static str) -> Response<Body> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(message.as_bytes()))
            .map_err(|never: Infallible| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    fn forbidden_headers(headers: &hyper::HeaderMap) -> Vec<&str> {
        [
            "x-private-hop",
            "x-second-hop",
            "proxy-authorization",
            "keep-alive",
            "te",
            "trailer",
            "transfer-encoding",
        ]
        .into_iter()
        .filter(|name| headers.contains_key(*name))
        .collect()
    }

    #[test]
    fn positive_controls_ws_hop_headers() {
        let mut headers = hyper::HeaderMap::new();
        headers.append(
            header::CONNECTION,
            "Upgrade, X-Private-Hop".parse().unwrap(),
        );
        headers.append(
            header::CONNECTION,
            "x-SECOND-hop, keep-alive".parse().unwrap(),
        );
        for name in [
            "x-private-hop",
            "x-second-hop",
            "proxy-authorization",
            "keep-alive",
            "te",
            "trailer",
            "transfer-encoding",
        ] {
            headers.insert(name, "must-not-reach-upstream".parse().unwrap());
        }
        headers.insert(header::AUTHORIZATION, "Bearer end-to-end".parse().unwrap());
        headers.insert(header::SEC_WEBSOCKET_PROTOCOL, "probe".parse().unwrap());
        headers.insert(header::SEC_WEBSOCKET_KEY, "downstream-key".parse().unwrap());
        headers.insert(
            header::SEC_WEBSOCKET_EXTENSIONS,
            "permessage-deflate".parse().unwrap(),
        );
        headers.insert(header::HOST, "downstream.invalid".parse().unwrap());
        // Reproduce the former request builder. Its static skip list accepts
        // Connection-nominated headers and Proxy-Authorization as end-to-end.
        let mut legacy = ClientRequestBuilder::new("ws://127.0.0.1:13002/ws".parse().unwrap());
        for (name, value) in &headers {
            if !matches!(
                name.as_str(),
                "host"
                    | "connection"
                    | "upgrade"
                    | "sec-websocket-key"
                    | "sec-websocket-version"
                    | "sec-websocket-extensions"
            ) {
                legacy = legacy
                    .with_header(name.as_str().to_owned(), value.to_str().unwrap().to_owned());
            }
        }
        let legacy = legacy.into_client_request().unwrap();
        assert!(
            !forbidden_headers(legacy.headers()).is_empty(),
            "injected leak must FAIL"
        );
        let request =
            websocket_request("ws://127.0.0.1:13002/ws".parse().unwrap(), &headers).unwrap();
        assert!(forbidden_headers(request.headers()).is_empty());
        assert_eq!(request.headers()[header::HOST], "127.0.0.1:13002");
        assert_eq!(
            request.headers()[header::AUTHORIZATION],
            "Bearer end-to-end"
        );
        assert_eq!(request.headers()[header::SEC_WEBSOCKET_PROTOCOL], "probe");
        assert_eq!(request.headers()[header::CONNECTION], "Upgrade");
        assert_eq!(request.headers()[header::UPGRADE], "websocket");
        assert_ne!(
            request.headers()[header::SEC_WEBSOCKET_KEY],
            "downstream-key"
        );
        assert!(!request
            .headers()
            .contains_key(header::SEC_WEBSOCKET_EXTENSIONS));
        println!("CONTROL inject:ws-hop-headers before=PASS after=FAIL safe=PASS");
        // Request cloning must leave downstream handshake validation intact.
        assert_eq!(headers.get_all(header::CONNECTION).iter().count(), 2);
    }

    #[test]
    fn connection_bytes_fail_closed() {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            header::HeaderValue::from_bytes(b"x-private-hop, \xff").unwrap(),
        );
        headers.insert("x-private-hop", "secret".parse().unwrap());
        assert!(strip_hop_headers(&mut headers).is_err());
        headers.insert(header::CONNECTION, " X-Private-Hop\t, TE".parse().unwrap());
        strip_hop_headers(&mut headers).unwrap();
        assert!(!headers.contains_key("x-private-hop"));
    }
}
