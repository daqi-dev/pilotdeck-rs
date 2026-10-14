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
use tokio::net::TcpListener;
use tokio_tungstenite::{
    client_async_with_config,
    tungstenite::{
        handshake::derive_accept_key,
        protocol::{Role, WebSocketConfig},
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
// Baseline ws/lib/websocket.js: closeTimeout defaults to 30000 ms.
const CLOSE_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

// Readers retain tungstenite validation/reassembly, but their automatic control
// writes are discarded. Only a separate write-only encoder may put frames on
// the wire. During the HTTP handshake, writes are still enabled.
struct RelayIo<S> {
    inner: S,
    discard_writes: bool,
}
impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for RelayIo<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for RelayIo<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.discard_writes {
            return std::task::Poll::Ready(Ok(buf.len()));
        }
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.discard_writes {
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.discard_writes {
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
async fn reader_socket<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    stream: S,
    role: Role,
) -> WebSocketStream<RelayIo<S>> {
    WebSocketStream::from_raw_socket(
        RelayIo {
            inner: stream,
            discard_writes: true,
        },
        role,
        Some(websocket_config()),
    )
    .await
}

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
                    if let Err(error) = downstream_http()
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
    fn record_local_error(&self, id: u64, status: StatusCode, message: &'static str) {
        if let Some(log) = &self.recorder {
            let _ = log.write(serde_json::json!({"kind":"recorder_error", "id":id,
                "status":status.as_u16(), "headers":[], "body_b64":STANDARD.encode(message.as_bytes())}));
        }
    }
    fn local_response(&self, id: u64, status: StatusCode, message: &'static str) -> Response<Body> {
        self.record_local_error(id, status, message);
        plain(status, message)
    }
    fn abort_handshake(&self, id: u64) -> Response<Body> {
        let status = StatusCode::BAD_REQUEST;
        let message = "invalid websocket upgrade";
        self.record_local_error(id, status, message);
        // ws abortHandshake writes directly to the socket without a Date header.
        plain_without_date(status, message)
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
            return Ok(self.abort_handshake(id));
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
        let response = websocket_response(response.into_parts().0.headers)?;
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
        if websocket_accept(request.method(), request.headers()).is_none() {
            return Ok(self.abort_handshake(id));
        }
        let path = request
            .uri()
            .path_and_query()
            .map_or("/", |value| value.as_str());
        let url: hyper::Uri = format!("ws://{}{path}", self.upstream_ws_authority).parse()?;
        // Forward the downstream handshake's end-to-end headers. The baseline
        // authenticates `/ws` from `Authorization: Bearer` as well as `?token=`
        // (ui/server/index.js:490-496); building the upstream from the URL alone
        // dropped the header and turned a direct 101 into a proxy 502. The
        // relay rebuilds hop-by-hop headers but retains the client challenge.
        // `Sec-WebSocket-Extensions` is not forwarded because this probe
        // terminates WS framing on both legs and cannot honour an extension.
        let builder = websocket_request(url, request.headers())?;
        let config = websocket_config();
        let (mut upstream, upstream_response) =
            tokio::time::timeout(Duration::from_secs(10), async {
                let socket = tokio::net::TcpStream::connect(&self.upstream_ws_authority).await?;
                client_async_with_config(
                    builder,
                    RelayIo {
                        inner: socket,
                        discard_writes: false,
                    },
                    Some(config),
                )
                .await
                .map_err(ProxyError::from)
            })
            .await??;
        upstream.get_mut().discard_writes = true;
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
                    let mut downstream = reader_socket(TokioIo::new(io), Role::Server).await;
                    if let Err(error) =
                        relay_websocket(&mut upstream, &mut downstream, frames.as_deref()).await
                    {
                        eprintln!("WS relay: {error}");
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
        let mut response = websocket_response(upstream_response.into_parts().0.headers)?;
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

// Once either peer sends Close, never flush/read that leg's tungstenite again:
// reading Close queues an automatic receipt. Only the other peer's actual reply
// belongs on the wire; flushing the queued receipt would send a second close.
async fn relay_websocket<U, D>(
    upstream: &mut WebSocketStream<RelayIo<U>>,
    downstream: &mut WebSocketStream<RelayIo<D>>,
    frames: Option<&FrameLog>,
) -> Result<(), ProxyError>
where
    U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    D: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let keep_open = tokio::select! {
            frame = downstream.next() => {
                let closing = matches!(&frame, Some(Ok(Message::Close(_))));
                let keep_open = forward_frame(frame, upstream, frames, "c2u", Role::Client).await?;
                if closing {
                    relay_close_reply(upstream, downstream, frames, "u2c", Role::Server).await?;
                }
                keep_open
            },
            frame = upstream.next() => {
                let closing = matches!(&frame, Some(Ok(Message::Close(_))));
                let keep_open = forward_frame(frame, downstream, frames, "u2c", Role::Server).await?;
                if closing {
                    relay_close_reply(downstream, upstream, frames, "c2u", Role::Client).await?;
                }
                keep_open
            },
        };
        if !keep_open {
            return Ok(());
        }
        // Readers may flush queued automatic Pong/Close internally on next(),
        // but RelayIo discards those bytes. Peer frames alone reach the wire.
    }
}

async fn relay_close_reply<S, T>(
    source: &mut WebSocketStream<RelayIo<S>>,
    target: &mut WebSocketStream<RelayIo<T>>,
    frames: Option<&FrameLog>,
    dir: &str,
    role: Role,
) -> Result<(), ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(CLOSE_REPLY_TIMEOUT, async {
        // A write-only encoder bypasses target's queued automatic receipt. It
        // still masks client-to-upstream frames with fresh random mask keys.
        let mut writer = WebSocketStream::from_raw_socket(
            &mut target.get_mut().inner,
            role,
            Some(websocket_config()),
        )
        .await;
        while let Some(frame) = source.next().await {
            let frame = frame?;
            tee_message(frames, dir, &frame).await?;
            let closing = frame.is_close();
            writer.send(frame).await?;
            if closing {
                return Ok::<_, ProxyError>(());
            }
        }
        Err("peer ended without a close reply".into())
    })
    .await?
}

// Hyper's default adds Date even to an upstream 101 that has none. A proxy
// preserves upstream Date (including absence); it does not manufacture one.
fn downstream_http() -> http1::Builder {
    let mut builder = http1::Builder::new();
    builder.auto_date_header(false);
    builder
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
    if headers.get_all(header::SEC_WEBSOCKET_KEY).iter().count() != 1 {
        return None;
    }
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
    if headers.get_all(header::SEC_WEBSOCKET_KEY).iter().count() != 1 {
        return Err("expected exactly one Sec-WebSocket-Key".into());
    }
    let mut forwarded = headers.clone();
    strip_hop_headers(&mut forwarded)?;
    let mut request = ClientRequestBuilder::new(url).into_client_request()?;
    for (name, value) in &forwarded {
        if matches!(
            name.as_str(),
            "host" | "sec-websocket-version" | "sec-websocket-extensions"
        ) {
            continue;
        }
        if name == header::SEC_WEBSOCKET_KEY {
            // Retain the challenge so the upstream accept value can pass unchanged.
            request.headers_mut().insert(name, value.clone());
        } else {
            request.headers_mut().append(name, value.clone());
        }
    }
    Ok(request)
}

fn websocket_response(mut headers: hyper::HeaderMap) -> Result<Response<Body>, ProxyError> {
    strip_hop_headers(&mut headers)?;
    headers.insert(
        header::UPGRADE,
        hyper::header::HeaderValue::from_static("websocket"),
    );
    headers.insert(
        header::CONNECTION,
        hyper::header::HeaderValue::from_static("Upgrade"),
    );
    let mut response = Response::new(empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    *response.headers_mut() = headers;
    Ok(response)
}

async fn forward_frame<S>(
    frame: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    target: &mut WebSocketStream<RelayIo<S>>,
    frames: Option<&FrameLog>,
    dir: &str,
    role: Role,
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
    let mut writer = WebSocketStream::from_raw_socket(
        &mut target.get_mut().inner,
        role,
        Some(websocket_config()),
    )
    .await;
    writer.send(frame).await?;
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
    let mut response = plain_without_date(status, message);
    response.headers_mut().insert(
        header::DATE,
        httpdate::fmt_http_date(std::time::SystemTime::now())
            .parse()
            .expect("HTTP date"),
    );
    response
}
fn plain_without_date(status: StatusCode, message: &'static str) -> Response<Body> {
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tokio_tungstenite::tungstenite::{
        client::IntoClientRequest,
        protocol::{frame::Frame, CloseFrame},
    };

    const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

    // Exercise Hyper's actual HTTP serialization over memory IO. Merely checking
    // websocket_response().headers() would miss Hyper's implicit Date header.
    async fn serialized_handshake(
        upstream: hyper::HeaderMap,
        automatic_date: bool,
    ) -> hyper::HeaderMap {
        let (client, server) = tokio::io::duplex(8192);
        let task = tokio::spawn(async move {
            let service = service_fn(move |_: Request<Incoming>| {
                let response = websocket_response(upstream.clone());
                async move { Ok::<_, Infallible>(response.unwrap()) }
            });
            let mut builder = downstream_http();
            if automatic_date {
                builder.auto_date_header(true);
            }
            builder
                .serve_connection(TokioIo::new(server), service)
                .with_upgrades()
                .await
                .unwrap();
        });
        let mut request = "ws://memory/ws".into_client_request().unwrap();
        request
            .headers_mut()
            .insert(header::SEC_WEBSOCKET_KEY, KEY.parse().unwrap());
        let (socket, response) = tokio_tungstenite::client_async(request, client)
            .await
            .unwrap();
        drop(socket);
        task.await.unwrap();
        response.into_parts().0.headers
    }

    #[tokio::test]
    async fn ws_101_serialized_headers_equal_upstream() {
        for date in [None, Some("Wed, 14 Oct 2026 07:00:00 GMT")] {
            let mut headers = hyper::HeaderMap::new();
            headers.insert(header::CONNECTION, "Upgrade".parse().unwrap());
            headers.insert(header::UPGRADE, "websocket".parse().unwrap());
            headers.insert(
                header::SEC_WEBSOCKET_ACCEPT,
                derive_accept_key(KEY.as_bytes()).parse().unwrap(),
            );
            headers.append(header::SET_COOKIE, "first=a; HttpOnly".parse().unwrap());
            headers.append(
                header::SET_COOKIE,
                "second=b; SameSite=Strict".parse().unwrap(),
            );
            headers.insert(
                "x-bytes",
                header::HeaderValue::from_bytes(b"\xffopaque").unwrap(),
            );
            if let Some(date) = date {
                headers.insert(header::DATE, date.parse().unwrap());
            }
            let actual = serialized_handshake(headers.clone(), false).await;
            assert_eq!(actual, headers);
            assert_eq!(
                actual
                    .get_all(header::SET_COOKIE)
                    .iter()
                    .collect::<Vec<_>>(),
                headers
                    .get_all(header::SET_COOKIE)
                    .iter()
                    .collect::<Vec<_>>()
            );
            if date.is_none() {
                let legacy = serialized_handshake(headers.clone(), true).await;
                assert!(
                    legacy.contains_key(header::DATE),
                    "positive control: Hyper default adds Date"
                );
                assert_ne!(legacy, headers);
            }
            // B1 keeps its handler provenance marker; nominated/private hops
            // must still disappear before serialization, even with duplicate Connection.
            let mut with_hops = headers.clone();
            with_hops.append(header::CONNECTION, "x-hop, keep-alive".parse().unwrap());
            with_hops.insert("x-hop", "private".parse().unwrap());
            with_hops.insert("keep-alive", "timeout=5".parse().unwrap());
            with_hops.insert("x-pilotdeck-handler", "ts".parse().unwrap());
            headers.insert("x-pilotdeck-handler", "ts".parse().unwrap());
            assert_eq!(serialized_handshake(with_hops, false).await, headers);
        }
    }

    fn close_wire(close: Option<CloseFrame>, masked: bool) -> Vec<u8> {
        let mut frame = Frame::close(close);
        if masked {
            frame.header_mut().mask = Some([1, 2, 3, 4]);
        }
        let mut wire = Vec::new();
        frame.format(&mut wire).unwrap();
        wire
    }

    async fn read_close(socket: &mut DuplexStream, masked: bool) -> Vec<u8> {
        let mut head = [0; 2];
        socket.read_exact(&mut head).await.unwrap();
        assert_eq!(head[0], 0x88);
        assert_eq!(head[1] & 0x80 != 0, masked);
        let len = usize::from(head[1] & 0x7f);
        assert!(len <= 125);
        let mut mask = [0; 4];
        if masked {
            socket.read_exact(&mut mask).await.unwrap();
        }
        let mut payload = vec![0; len];
        socket.read_exact(&mut payload).await.unwrap();
        if masked {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
        }
        payload
    }

    async fn close_roundtrip(
        client_first: bool,
        close: Option<CloseFrame>,
        reply: Option<CloseFrame>,
    ) {
        tokio::time::timeout(Duration::from_secs(3), async {
            let (mut browser, proxy_down) = tokio::io::duplex(4096);
            let (proxy_up, mut server) = tokio::io::duplex(4096);
            let task = tokio::spawn(async move {
                let mut upstream = reader_socket(proxy_up, Role::Client).await;
                let mut downstream = reader_socket(proxy_down, Role::Server).await;
                relay_websocket(&mut upstream, &mut downstream, None)
                    .await
                    .unwrap();
            });
            let (initiator, peer) = if client_first {
                (&mut browser, &mut server)
            } else {
                (&mut server, &mut browser)
            };
            initiator
                .write_all(&close_wire(close.clone(), client_first))
                .await
                .unwrap();
            assert_eq!(
                read_close(peer, client_first).await,
                close_wire(close, false)[2..]
            );
            // No proxy-generated receipt may reach the initiator before the peer replies.
            let mut byte = [0];
            assert!(
                tokio::time::timeout(Duration::from_millis(20), initiator.read(&mut byte))
                    .await
                    .is_err()
            );
            peer.write_all(&close_wire(reply.clone(), !client_first))
                .await
                .unwrap();
            assert_eq!(
                read_close(initiator, !client_first).await,
                close_wire(reply, false)[2..]
            );
            task.await.unwrap();
            // Inspect raw IO, not a WS reader that might hide a duplicate close.
            let mut extra = Vec::new();
            browser.read_to_end(&mut extra).await.unwrap();
            server.read_to_end(&mut extra).await.unwrap();
            assert!(
                extra.is_empty(),
                "each direction must have exactly one close: {extra:?}"
            );
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn client_close_one_upstream_one_downstream_receipt() {
        for close in [
            None,
            Some(CloseFrame {
                code: 1000.into(),
                reason: "".into(),
            }),
            Some(CloseFrame {
                code: 1001.into(),
                reason: "离开".into(),
            }),
            Some(CloseFrame {
                code: 4001.into(),
                reason: "custom".into(),
            }),
        ] {
            close_roundtrip(true, close.clone(), close).await;
        }
    }

    #[tokio::test]
    async fn upstream_close_one_downstream_one_upstream_receipt() {
        for close in [
            None,
            Some(CloseFrame {
                code: 1000.into(),
                reason: "".into(),
            }),
            Some(CloseFrame {
                code: 1001.into(),
                reason: "离开".into(),
            }),
            Some(CloseFrame {
                code: 4001.into(),
                reason: "custom".into(),
            }),
        ] {
            close_roundtrip(false, close.clone(), close).await;
        }
    }

    #[tokio::test]
    async fn forwards_actual_receipt_instead_of_echoing_initiator() {
        for client_first in [true, false] {
            close_roundtrip(
                client_first,
                Some(CloseFrame {
                    code: 4001.into(),
                    reason: "request".into(),
                }),
                Some(CloseFrame {
                    code: 4002.into(),
                    reason: "peer receipt".into(),
                }),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn legacy_flush_after_forwarded_receipt_reproduces_duplicate_close() {
        let close = Some(CloseFrame {
            code: 1000.into(),
            reason: "".into(),
        });
        let (mut browser, proxy_down) = tokio::io::duplex(4096);
        let (proxy_up, mut server) = tokio::io::duplex(4096);
        let mut downstream = WebSocketStream::from_raw_socket(proxy_down, Role::Server, None).await;
        let mut upstream = WebSocketStream::from_raw_socket(proxy_up, Role::Client, None).await;
        browser
            .write_all(&close_wire(close.clone(), true))
            .await
            .unwrap();
        let frame = downstream.next().await; // queues tungstenite's automatic receipt
        upstream.send(frame.unwrap().unwrap()).await.unwrap();
        assert_eq!(read_close(&mut server, true).await, [3, 232]);
        server.write_all(&close_wire(close, false)).await.unwrap();
        let receipt = upstream.next().await.unwrap().unwrap();
        let mut writer =
            WebSocketStream::from_raw_socket(downstream.get_mut(), Role::Server, None).await;
        writer.send(receipt).await.unwrap();
        drop(writer);
        let _ = downstream.flush().await; // the pre-#72 loop did this unconditionally
        drop(downstream);
        assert_eq!(read_close(&mut browser, false).await, [3, 232]);
        assert_eq!(read_close(&mut browser, false).await, [3, 232]);
        let mut extra = Vec::new();
        browser.read_to_end(&mut extra).await.unwrap();
        assert!(extra.is_empty());
    }

    #[tokio::test]
    async fn peer_eof_does_not_manufacture_close_receipt() {
        for client_first in [true, false] {
            tokio::time::timeout(Duration::from_secs(3), async {
                let (mut browser, proxy_down) = tokio::io::duplex(4096);
                let (proxy_up, mut server) = tokio::io::duplex(4096);
                let task = tokio::spawn(async move {
                    let mut upstream = reader_socket(proxy_up, Role::Client).await;
                    let mut downstream = reader_socket(proxy_down, Role::Server).await;
                    assert!(relay_websocket(&mut upstream, &mut downstream, None)
                        .await
                        .is_err());
                });
                let (initiator, peer) = if client_first {
                    (&mut browser, &mut server)
                } else {
                    (&mut server, &mut browser)
                };
                initiator
                    .write_all(&close_wire(None, client_first))
                    .await
                    .unwrap();
                assert!(read_close(peer, client_first).await.is_empty());
                peer.shutdown().await.unwrap();
                task.await.unwrap();
                let mut extra = Vec::new();
                initiator.read_to_end(&mut extra).await.unwrap();
                assert!(extra.is_empty());
            })
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn handshake_rejections_have_no_date_over_memory_http() {
        let single = format!("Sec-WebSocket-Key: {KEY}\r\n");
        let duplicate = format!(
            "Sec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Key: {}\r\n",
            STANDARD.encode(b"another nonce123")
        );
        for recorded in [false, true] {
            for (method, version, keys) in [
                ("GET", "13", duplicate.as_str()),
                ("POST", "13", single.as_str()),
                ("GET", "12", single.as_str()),
                ("GET", "13", "Sec-WebSocket-Key: invalid\r\n"),
            ] {
                let (mut client, server) = tokio::io::duplex(8192);
                let task = tokio::spawn(async move {
                    let proxy = Arc::new(Proxy {
                        upstream: "http://memory".into(),
                        upstream_ws_authority: "memory".into(),
                        client: Client::builder(TokioExecutor::new()).build_http(),
                        handler: "ts".into(),
                        frames: None,
                        recorder: None,
                        next_id: AtomicU64::new(1),
                    });
                    let service = service_fn(move |mut request: Request<Incoming>| {
                        let proxy = proxy.clone();
                        async move {
                            assert!(websocket_accept(request.method(), request.headers()).is_none());
                            if request
                                .headers()
                                .get_all(header::SEC_WEBSOCKET_KEY)
                                .iter()
                                .count()
                                > 1
                            {
                                assert!(websocket_request(
                                    "ws://memory/ws".parse().unwrap(),
                                    request.headers()
                                )
                                .is_err());
                            }
                            let response = if recorded {
                                proxy.recorded_websocket(&mut request, 1).await
                            } else {
                                proxy.websocket(&mut request, 1).await
                            };
                            Ok::<_, Infallible>(response.unwrap())
                        }
                    });
                    downstream_http()
                        .serve_connection(TokioIo::new(server), service)
                        .await
                        .unwrap();
                });
                client.write_all(format!("{method} /ws HTTP/1.1\r\nHost: memory\r\nConnection: Upgrade, close\r\nUpgrade: websocket\r\nSec-WebSocket-Version: {version}\r\n{keys}\r\n").as_bytes()).await.unwrap();
                let mut response = Vec::new();
                client.read_to_end(&mut response).await.unwrap();
                let text = String::from_utf8(response).unwrap();
                assert!(text.starts_with("HTTP/1.1 400"));
                assert!(
                    !text
                        .lines()
                        .any(|line| line.to_ascii_lowercase().starts_with("date:")),
                    "{text}"
                );
                assert!(text.ends_with("invalid websocket upgrade"));
                task.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn local_errors_have_date_over_memory_http() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
        ] {
            let (mut client, server) = tokio::io::duplex(8192);
            let task = tokio::spawn(async move {
                let service = service_fn(move |_: Request<Incoming>| async move {
                    Ok::<_, Infallible>(plain(status, "local error"))
                });
                downstream_http()
                    .serve_connection(TokioIo::new(server), service)
                    .await
                    .unwrap();
            });
            client
                .write_all(b"GET / HTTP/1.1\r\nHost: memory\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut response = Vec::new();
            client.read_to_end(&mut response).await.unwrap();
            let text = String::from_utf8(response).unwrap();
            assert!(text.starts_with(&format!("HTTP/1.1 {}", status.as_u16())));
            let dates: Vec<_> = text
                .lines()
                .filter_map(|line| line.strip_prefix("date: "))
                .collect();
            assert_eq!(dates.len(), 1);
            assert!(httpdate::parse_http_date(dates[0]).is_ok());
            task.await.unwrap();
        }
    }

    fn control_wire(opcode: u8, payload: &[u8], masked: bool) -> Vec<u8> {
        let mut wire = vec![
            0x80 | opcode,
            payload.len() as u8 | if masked { 0x80 } else { 0 },
        ];
        let mask = [1, 2, 3, 4];
        if masked {
            wire.extend(mask);
        }
        wire.extend(
            payload
                .iter()
                .enumerate()
                .map(|(i, b)| if masked { b ^ mask[i % 4] } else { *b }),
        );
        wire
    }

    async fn read_control(socket: &mut DuplexStream, opcode: u8, masked: bool) -> Vec<u8> {
        let mut head = [0; 2];
        socket.read_exact(&mut head).await.unwrap();
        assert_eq!(head[0], 0x80 | opcode);
        assert_eq!(head[1] & 128 != 0, masked);
        let mut mask = [0; 4];
        if masked {
            socket.read_exact(&mut mask).await.unwrap();
        }
        let mut payload = vec![0; usize::from(head[1] & 127)];
        socket.read_exact(&mut payload).await.unwrap();
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        payload
    }

    #[tokio::test]
    async fn ping_only_forwards_peer_pong_even_after_more_reads() {
        for client_first in [true, false] {
            tokio::time::timeout(Duration::from_secs(3), async {
                let (mut browser, proxy_down) = tokio::io::duplex(4096);
                let (proxy_up, mut server) = tokio::io::duplex(4096);
                let task = tokio::spawn(async move {
                    let mut upstream = reader_socket(proxy_up, Role::Client).await;
                    let mut downstream = reader_socket(proxy_down, Role::Server).await;
                    relay_websocket(&mut upstream, &mut downstream, None)
                        .await
                        .unwrap();
                });
                let (initiator, peer) = if client_first {
                    (&mut browser, &mut server)
                } else {
                    (&mut server, &mut browser)
                };
                for payload in [b"first".as_slice(), b"second".as_slice()] {
                    initiator
                        .write_all(&control_wire(9, payload, client_first))
                        .await
                        .unwrap();
                    assert_eq!(read_control(peer, 9, client_first).await, payload);
                    let mut byte = [0];
                    assert!(tokio::time::timeout(
                        Duration::from_millis(20),
                        initiator.read(&mut byte)
                    )
                    .await
                    .is_err());
                    peer.write_all(&control_wire(10, b"actual peer receipt", !client_first))
                        .await
                        .unwrap();
                    assert_eq!(
                        read_control(initiator, 10, !client_first).await,
                        b"actual peer receipt"
                    );
                }
                initiator
                    .write_all(&close_wire(None, client_first))
                    .await
                    .unwrap();
                assert!(read_close(peer, client_first).await.is_empty());
                peer.write_all(&close_wire(None, !client_first))
                    .await
                    .unwrap();
                assert!(read_close(initiator, !client_first).await.is_empty());
                task.await.unwrap();
                let mut extra = Vec::new();
                browser.read_to_end(&mut extra).await.unwrap();
                server.read_to_end(&mut extra).await.unwrap();
                assert!(extra.is_empty(), "no synthetic/duplicate Pong: {extra:?}");
            })
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn slow_close_receipt_within_node_budget_is_forwarded() {
        let (mut browser, proxy_down) = tokio::io::duplex(4096);
        let (proxy_up, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            let mut upstream = reader_socket(proxy_up, Role::Client).await;
            let mut downstream = reader_socket(proxy_down, Role::Server).await;
            relay_websocket(&mut upstream, &mut downstream, None)
                .await
                .unwrap();
        });
        browser.write_all(&close_wire(None, true)).await.unwrap();
        assert!(read_close(&mut server, true).await.is_empty());
        tokio::time::sleep(Duration::from_millis(1100)).await;
        server.write_all(&close_wire(None, false)).await.unwrap();
        assert!(read_close(&mut browser, false).await.is_empty());
        task.await.unwrap();
        assert_eq!(CLOSE_REPLY_TIMEOUT, Duration::from_secs(30));
    }

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
        assert_eq!(
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
