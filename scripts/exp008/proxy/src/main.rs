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
    sync::Arc,
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
type HttpClient = Client<HttpConnector, Incoming>;

// Match the A1 bounds: buffer at most a complete (possibly fragmented) message.
const MAX_WS_MESSAGE: usize = 16 * 1024 * 1024;
const MAX_WS_FRAME: usize = 4 * 1024 * 1024;
// Bound memory even when the writer is slower than the relay.
const LOG_QUEUE_CAPACITY: usize = 8;

struct FrameLog {
    sender: tokio::sync::mpsc::Sender<serde_json::Value>,
}

impl FrameLog {
    /// One writer owns sequence order. A bounded async queue applies relay
    /// backpressure without blocking Tokio workers on disk I/O. Each drained
    /// batch is flushed once; write failure closes the queue and fails relay.
    fn open(path: &str) -> std::io::Result<Arc<FrameLog>> {
        let mut file = std::fs::File::create(path)?;
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<serde_json::Value>(LOG_QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("exp008-ws-log".into())
            .spawn(move || {
                let mut seq: u64 = 0;
                while let Some(entry) = receiver.blocking_recv() {
                    let mut batch = vec![entry];
                    while batch.len() < LOG_QUEUE_CAPACITY {
                        match receiver.try_recv() {
                            Ok(entry) => batch.push(entry),
                            Err(_) => break,
                        }
                    }
                    let written = (|| -> std::io::Result<()> {
                        for mut entry in batch {
                            seq += 1;
                            entry["seq"] = serde_json::json!(seq);
                            writeln!(file, "{entry}")?;
                        }
                        file.flush()
                    })();
                    if let Err(error) = written {
                        eprintln!("WS frame log: {error}");
                        break;
                    }
                }
            })?;
        Ok(Arc::new(FrameLog { sender }))
    }

    async fn tee(
        &self,
        dir: &str,
        payload: &[u8],
        opcode: &str,
        close: Option<(u16, &str)>,
    ) -> Result<(), ProxyError> {
        let permit = self
            .sender
            .reserve()
            .await
            .map_err(|_| -> ProxyError { "WS frame log writer closed".into() })?;
        let raw = match std::str::from_utf8(payload) {
            Ok(text) if opcode == "text" => serde_json::Value::String(text.to_owned()),
            _ => serde_json::Value::String(STANDARD.encode(payload)),
        };
        let mut entry = serde_json::json!({
            "dir": dir,
            "opcode": opcode,
            "bytes": payload.len(),
            "raw": raw,
        });
        if let Some((code, reason)) = close {
            entry["close"] = serde_json::json!({ "code": code, "reason": reason });
        }
        permit.send(entry);
        Ok(())
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
        Ok(path) if !path.is_empty() => Some(FrameLog::open(&path)?),
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
        let url: hyper::Uri = format!("ws://{}{path}", self.upstream_ws_authority).parse()?;
        // Forward the downstream handshake's end-to-end headers. The baseline
        // authenticates `/ws` from `Authorization: Bearer` as well as `?token=`
        // (ui/server/index.js:490-496); building the upstream from the URL alone
        // dropped the header and turned a direct 101 into a proxy 502. The
        // relay generates its own hop-by-hop / WS handshake headers, so those
        // are skipped, and `Sec-WebSocket-Extensions` is not forwarded because
        // this probe terminates WS framing on both legs and cannot honour a
        // negotiated extension.
        let builder = websocket_request(url, request.headers());
        let config = websocket_config();
        let (mut upstream, upstream_response) = tokio::time::timeout(
            Duration::from_secs(10),
            connect_async_with_config(builder, Some(config), false),
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
        });
        let mut response = websocket_response(upstream_response.into_parts().0.headers, &accept);
        if let Ok(value) = self.handler.parse() {
            response.headers_mut().insert("x-pilotdeck-handler", value);
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

fn websocket_request(url: hyper::Uri, headers: &hyper::HeaderMap) -> ClientRequestBuilder {
    let mut headers = headers.clone();
    strip_hop_headers(&mut headers);
    let mut builder = ClientRequestBuilder::new(url);
    for (name, value) in &headers {
        if matches!(
            name.as_str(),
            "host" | "sec-websocket-key" | "sec-websocket-version" | "sec-websocket-extensions"
        ) {
            continue;
        }
        builder = builder.with_header(
            name.as_str().to_owned(),
            String::from_utf8_lossy(value.as_bytes()).into_owned(),
        );
    }
    builder
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
    tee_message(frames, dir, &frame).await?;
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
        let request = websocket_request("ws://127.0.0.1:13002/ws".parse().unwrap(), &headers)
            .into_client_request()
            .unwrap();
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

    #[tokio::test]
    async fn positive_controls_log_backpressure() {
        let (unbounded, _receiver) = std::sync::mpsc::channel();
        for _ in 0..=LOG_QUEUE_CAPACITY {
            assert!(unbounded.send("frame").is_ok());
        }
        let (sender, mut receiver) = tokio::sync::mpsc::channel(LOG_QUEUE_CAPACITY);
        let log = FrameLog { sender };
        for _ in 0..LOG_QUEUE_CAPACITY {
            log.tee("u2c", b"frame", "text", None).await.unwrap();
        }
        assert_eq!(log.sender.capacity(), 0);
        // Former unbounded send accepts this injection immediately. A stopped
        // disk writer must now leave the extra frame pending, not grow memory.
        assert!(tokio::time::timeout(
            Duration::from_millis(20),
            log.tee("u2c", b"overflow", "text", None)
        )
        .await
        .is_err());
        assert_eq!(receiver.len(), LOG_QUEUE_CAPACITY);
        receiver.recv().await.unwrap();
        log.tee("u2c", b"resumed", "text", None).await.unwrap();
        println!("CONTROL inject:log-queue-overflow before=PASS after=FAIL safe=PASS");
        drop(receiver);
        assert!(log
            .tee("u2c", b"writer-failed", "text", None)
            .await
            .is_err());
    }
}
