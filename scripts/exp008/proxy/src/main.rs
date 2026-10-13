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
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
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
        Message,
    },
    WebSocketStream,
};

type ProxyError = Box<dyn Error + Send + Sync>;
type Body = BoxBody<Bytes, ProxyError>;
type HttpClient = Client<HttpConnector, Incoming>;

// Match the A1 bounds: buffer at most a complete (possibly fragmented) message.
const MAX_WS_MESSAGE: usize = 16 * 1024 * 1024;
const MAX_WS_FRAME: usize = 4 * 1024 * 1024;

struct FrameLog {
    file: Mutex<std::fs::File>,
    seq: AtomicU64,
}

impl FrameLog {
    fn tee(&self, dir: &str, payload: &[u8], opcode: &str, close: Option<(u16, &str)>) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let raw = match std::str::from_utf8(payload) {
            Ok(text) if opcode == "text" => serde_json::Value::String(text.to_owned()),
            _ => serde_json::Value::String(STANDARD.encode(payload)),
        };
        let mut entry = serde_json::json!({
            "seq": seq,
            "dir": dir,
            "opcode": opcode,
            "bytes": payload.len(),
            "raw": raw,
        });
        if let Some((code, reason)) = close {
            entry["close"] = serde_json::json!({ "code": code, "reason": reason });
        }
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{entry}");
        }
    }
}

struct Proxy {
    upstream: String,
    upstream_ws_authority: String,
    client: HttpClient,
    handler: String,
    frames: Option<Arc<FrameLog>>,
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
        Ok(path) if !path.is_empty() => Some(Arc::new(FrameLog {
            file: Mutex::new(std::fs::File::create(path)?),
            seq: AtomicU64::new(0),
        })),
        _ => None,
    };
    let proxy = Arc::new(Proxy {
        upstream: upstream.trim_end_matches('/').to_owned(),
        upstream_ws_authority: authority,
        client: Client::builder(TokioExecutor::new()).build_http(),
        handler,
        frames,
    });
    let listener = TcpListener::bind(listen).await?;
    eprintln!("exp008 proxy listening {listen} -> {}", proxy.upstream);
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
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

impl Proxy {
    async fn serve(&self, mut request: Request<Incoming>) -> Response<Body> {
        let is_upgrade = request
            .headers()
            .get(header::UPGRADE)
            .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"));
        if is_upgrade {
            return match self.websocket(&mut request).await {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("WS pairing: {error}");
                    plain(StatusCode::BAD_GATEWAY, "upstream websocket unavailable")
                }
            };
        }
        let path = request
            .uri()
            .path_and_query()
            .map_or("/", |value| value.as_str());
        let uri: hyper::Uri = match format!("{}{path}", self.upstream).parse() {
            Ok(uri) => uri,
            Err(_) => return plain(StatusCode::BAD_REQUEST, "invalid request URI"),
        };
        let host = uri
            .authority()
            .expect("validated upstream")
            .as_str()
            .to_owned();
        *request.uri_mut() = uri;
        strip_hop_headers(request.headers_mut());
        request
            .headers_mut()
            .insert(header::HOST, host.parse().expect("URI authority"));
        match self.client.request(request).await {
            Ok(mut response) => {
                strip_hop_headers(response.headers_mut());
                if let Ok(value) = self.handler.parse() {
                    response.headers_mut().insert("x-pilotdeck-handler", value);
                }
                response.map(|body| {
                    body.map_err(|error| -> ProxyError { Box::new(error) })
                        .boxed()
                })
            }
            Err(error) => {
                eprintln!("HTTP upstream: {error}");
                plain(StatusCode::BAD_GATEWAY, "upstream HTTP unavailable")
            }
        }
    }

    async fn websocket(
        &self,
        request: &mut Request<Incoming>,
    ) -> Result<Response<Body>, ProxyError> {
        let Some(accept) = websocket_accept(request.method(), request.headers()) else {
            return Ok(plain(StatusCode::BAD_REQUEST, "invalid websocket upgrade"));
        };
        let path = request
            .uri()
            .path_and_query()
            .map_or("/", |value| value.as_str());
        let url = format!("ws://{}{path}", self.upstream_ws_authority);
        let config = websocket_config();
        let (mut upstream, upstream_response) = tokio::time::timeout(
            Duration::from_secs(10),
            connect_async_with_config(url, Some(config), false),
        )
        .await??;
        let upgraded = hyper::upgrade::on(request);
        let frames = self.frames.clone();
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
                                let result = relay(frame, &mut downstream, frames.as_deref(), "u2c").await;
                                if matches!(result, Ok(false)) {
                                    let _ = upstream.flush().await;
                                }
                                result
                            },
                        };
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
        });
        let mut response = websocket_response(upstream_response.into_parts().0.headers, &accept);
        if let Ok(value) = self.handler.parse() {
            response.headers_mut().insert("x-pilotdeck-handler", value);
        }
        Ok(response)
    }
}

fn tee_message(frames: Option<&FrameLog>, dir: &str, message: &Message) {
    let Some(frames) = frames else {
        return;
    };
    match message {
        Message::Text(text) => frames.tee(dir, text.as_bytes(), "text", None),
        Message::Binary(data) => frames.tee(dir, data, "binary", None),
        Message::Ping(data) => frames.tee(dir, data, "ping", None),
        Message::Pong(data) => frames.tee(dir, data, "pong", None),
        Message::Close(close) => {
            let (code, reason) = close
                .as_ref()
                .map(|frame| (u16::from(frame.code), frame.reason.as_ref()))
                .unwrap_or((1005, ""));
            frames.tee(dir, &[], "close", Some((code, reason)));
        }
        Message::Frame(_) => {}
    }
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
            tee_message(frames, "u2c", &frame);
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
        || !headers.get_all(header::CONNECTION).iter().any(|value| {
            value.to_str().is_ok_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
            })
        })
        || STANDARD.decode(key.as_bytes()).ok()?.len() != 16
    {
        return None;
    }
    Some(derive_accept_key(key.as_bytes()))
}

fn websocket_response(mut headers: hyper::HeaderMap, accept: &str) -> Response<Body> {
    strip_hop_headers(&mut headers);
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
    response
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
    tee_message(frames, dir, &frame);
    let closing = frame.is_close();
    target.send(frame).await?;
    Ok(!closing)
}

fn strip_hop_headers(headers: &mut hyper::HeaderMap) {
    let nominated: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(',').map(|part| part.trim().to_owned()))
        .collect();
    for name in nominated {
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
