//! A1: one downstream connection owns exactly one upstream connection.
//! Application frames and HTTP bodies are never parsed or rewritten here.
use std::{convert::Infallible, error::Error, net::SocketAddr, sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{
    Request, Response, StatusCode, body::Incoming, header, server::conn::http1, service::service_fn,
};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use tokio::{io::AsyncWriteExt, net::TcpListener};
use tokio_tungstenite::{
    WebSocketStream, connect_async_with_config,
    tungstenite::{
        Message,
        handshake::derive_accept_key,
        protocol::frame::{
            Frame,
            coding::{Data, OpCode},
        },
        protocol::{Role, WebSocketConfig},
    },
};

type ProxyError = Box<dyn Error + Send + Sync>;
type Body = BoxBody<Bytes, ProxyError>;
type HttpClient = Client<HttpConnector, Incoming>;

// A1 buffers complete messages; bound both peers, including fragmented messages.
const MAX_WS_MESSAGE: usize = 16 * 1024 * 1024;
const MAX_WS_FRAME: usize = 4 * 1024 * 1024;

struct Proxy {
    upstream: String,
    client: HttpClient,
}

#[tokio::main]
async fn main() -> Result<(), ProxyError> {
    let listen: SocketAddr = std::env::var("PILOTDECK_PROXY_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:28789".into())
        .parse()?;
    if listen.ip() != std::net::Ipv4Addr::LOCALHOST {
        return Err("proxy must listen on 127.0.0.1".into());
    }
    let upstream = std::env::var("PILOTDECK_PROXY_UPSTREAM")
        .unwrap_or_else(|_| "http://127.0.0.1:28790".into());
    let uri: hyper::Uri = upstream.parse()?;
    if uri.scheme_str() != Some("http") || uri.authority().is_none() || uri.path() != "/" {
        return Err("upstream must be an http origin (no path)".into());
    }
    let proxy = Arc::new(Proxy {
        upstream: upstream.trim_end_matches('/').to_owned(),
        client: Client::builder(TokioExecutor::new()).build_http(),
    });
    let listener = TcpListener::bind(listen).await?;
    eprintln!("proxy listening {listen} -> {}", proxy.upstream);
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
                        // EMFILE and transient network errors must not stop the listener
                        // or spin at full CPU while resources remain exhausted.
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
                    if let Err(error) = http1::Builder::new().serve_connection(TokioIo::new(socket), service).with_upgrades().await {
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
            if request.uri().path() != "/ws" {
                return plain(StatusCode::NOT_FOUND, "upgrade endpoint not found");
            }
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
            .map_or("/ws", |value| value.as_str());
        let url = format!("ws://{}{path}", self.upstream.trim_start_matches("http://"));
        let config = websocket_config();
        // Establish the upstream before accepting downstream. No synthetic hello/binding.
        let (mut upstream, upstream_response) = tokio::time::timeout(
            Duration::from_secs(10),
            connect_async_with_config(url, Some(config), false),
        )
        .await??;
        let upgraded = hyper::upgrade::on(request);
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
                        // No detached sender task: leaving either arm drops both sockets.
                        let result = tokio::select! {
                            frame = downstream.next() => {
                                let closing = matches!(&frame, Some(Ok(Message::Close(_))));
                                let result = relay(frame, &mut upstream).await;
                                if closing && result.is_ok() {
                                    // The baseline replies 1000 even to an empty/custom
                                    // client close. Return that upstream reply, bypassing
                                    // Tungstenite's already queued local echo (1005/custom).
                                    if let Err(error) = relay_close_reply(&mut upstream, &mut downstream).await {
                                        eprintln!("WS close reply: {error}");
                                    }
                                }
                                result
                            },
                            frame = upstream.next() => {
                                let result = relay(frame, &mut downstream).await;
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
        Ok(websocket_response(
            upstream_response.into_parts().0.headers,
            &accept,
        ))
    }
}

async fn relay_close_reply<U, D>(
    upstream: &mut WebSocketStream<U>,
    downstream: &mut WebSocketStream<D>,
) -> Result<(), ProxyError>
where
    U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    D: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(frame) = upstream.next().await {
            let frame = frame?;
            let closing = frame.is_close();
            // Tungstenite forbids send() after receiving Close and has queued a
            // local close echo. Write upstream frames directly so in-flight data
            // precedes the real upstream close reply, without flushing that echo.
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
            // The downstream endpoint is a server: all frames stay unmasked.
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
) -> Result<bool, ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Some(frame) = frame else {
        return Ok(false);
    };
    let frame = frame?;
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
    use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

    #[tokio::test]
    async fn large_payload_and_upstream_close_survive_relay() {
        // In-memory transport: tests never bind another experiment port.
        let (sender, receiver) = tokio::io::duplex(256 * 1024);
        let mut sender = WebSocketStream::from_raw_socket(sender, Role::Server, None).await;
        let mut receiver = WebSocketStream::from_raw_socket(receiver, Role::Client, None).await;
        let text = Message::text("原帧".repeat(20_000));
        assert!(relay(Some(Ok(text.clone())), &mut sender).await.unwrap());
        assert_eq!(receiver.next().await.unwrap().unwrap(), text);
        let close = Message::Close(Some(CloseFrame {
            code: CloseCode::Library(4002),
            reason: "invalid_frame".into(),
        }));
        assert!(!relay(Some(Ok(close.clone())), &mut sender).await.unwrap());
        assert_eq!(receiver.next().await.unwrap().unwrap(), close);
    }

    #[tokio::test]
    async fn client_close_receives_upstream_code_instead_of_local_echo() {
        for original in [
            None,
            Some(CloseFrame {
                code: CloseCode::Library(4444),
                reason: "client".into(),
            }),
        ] {
            let (down_io, client_io) = tokio::io::duplex(1024);
            let (up_io, server_io) = tokio::io::duplex(1024);
            let mut downstream =
                WebSocketStream::from_raw_socket(down_io, Role::Server, None).await;
            let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
            let mut upstream = WebSocketStream::from_raw_socket(up_io, Role::Client, None).await;
            let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
            client.send(Message::Close(original.clone())).await.unwrap();
            let incoming = downstream.next().await;
            assert!(!relay(incoming, &mut upstream).await.unwrap());
            // Simulate the TS server's hard-coded normal close reply.
            let mut wire = Vec::new();
            Frame::message(
                Bytes::from_static(b"in-flight"),
                OpCode::Data(Data::Text),
                true,
            )
            .format(&mut wire)
            .unwrap();
            Frame::message(
                Bytes::from_static(b"\x00\x01"),
                OpCode::Data(Data::Binary),
                true,
            )
            .format(&mut wire)
            .unwrap();
            Frame::pong(Bytes::from_static(b"pending-pong"))
                .format(&mut wire)
                .unwrap();
            let reply = Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "".into(),
            });
            Frame::close(reply.clone()).format(&mut wire).unwrap();
            server.get_mut().write_all(&wire).await.unwrap();
            relay_close_reply(&mut upstream, &mut downstream)
                .await
                .unwrap();
            assert_eq!(
                client.next().await.unwrap().unwrap(),
                Message::text("in-flight")
            );
            assert_eq!(
                client.next().await.unwrap().unwrap(),
                Message::binary(vec![0, 1])
            );
            assert_eq!(
                client.next().await.unwrap().unwrap(),
                Message::Pong(Bytes::from_static(b"pending-pong"))
            );
            assert_eq!(client.next().await.unwrap().unwrap(), Message::Close(reply));
        }
    }

    #[test]
    fn malformed_client_handshakes_are_rejected_before_upstream_connect() {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(header::CONNECTION, "Upgrade".parse().unwrap());
        headers.insert(header::SEC_WEBSOCKET_VERSION, "13".parse().unwrap());
        assert!(websocket_accept(&hyper::Method::GET, &headers).is_none());
        for key in ["", "not-base64", "YQ=="] {
            headers.insert(header::SEC_WEBSOCKET_KEY, key.parse().unwrap());
            assert!(websocket_accept(&hyper::Method::GET, &headers).is_none());
        }
        headers.insert(
            header::SEC_WEBSOCKET_KEY,
            "dGhlIHNhbXBsZSBub25jZQ==".parse().unwrap(),
        );
        assert_eq!(
            websocket_accept(&hyper::Method::GET, &headers).as_deref(),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
        assert!(websocket_accept(&hyper::Method::POST, &headers).is_none());
        headers.insert(header::SEC_WEBSOCKET_VERSION, "12".parse().unwrap());
        assert!(websocket_accept(&hyper::Method::GET, &headers).is_none());
    }

    #[test]
    fn upstream_upgrade_headers_survive_with_downstream_accept_key() {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_ACCEPT,
            "upstream-key".parse().unwrap(),
        );
        headers.insert("x-upstream", "retained".parse().unwrap());
        headers.append(header::SET_COOKIE, "one=1".parse().unwrap());
        headers.append(header::SET_COOKIE, "two=2".parse().unwrap());
        let response = websocket_response(headers, "downstream-key");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            response.headers()[header::SEC_WEBSOCKET_ACCEPT],
            "downstream-key"
        );
        assert_eq!(response.headers()["x-upstream"], "retained");
        assert_eq!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn missing_client_key_returns_http_400() {
        let (server_io, client_io) = tokio::io::duplex(4096);
        let proxy = Arc::new(Proxy {
            upstream: "http://127.0.0.1:28790".into(),
            client: Client::builder(TokioExecutor::new()).build_http(),
        });
        let server = tokio::spawn(async move {
            let service = service_fn(move |request| {
                let proxy = proxy.clone();
                async move { Ok::<_, Infallible>(proxy.serve(request).await) }
            });
            http1::Builder::new()
                .serve_connection(TokioIo::new(server_io), service)
                .await
                .unwrap();
        });
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        let response = sender
            .send_request(
                Request::builder()
                    .uri("/ws")
                    .header(header::HOST, "127.0.0.1")
                    .header(header::UPGRADE, "websocket")
                    .header(header::CONNECTION, "Upgrade")
                    .header(header::SEC_WEBSOCKET_VERSION, "13")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        connection.abort();
        server.abort();
    }

    #[test]
    fn transient_accept_errors_do_not_stop_listener() {
        for error in [
            std::io::Error::from_raw_os_error(24),
            std::io::ErrorKind::ConnectionAborted.into(),
            std::io::ErrorKind::Interrupted.into(),
        ] {
            assert!(!fatal_accept_error(&error));
        }
        assert!(fatal_accept_error(&std::io::ErrorKind::InvalidInput.into()));
    }

    #[tokio::test]
    async fn frame_and_fragmented_message_limits_are_enforced() {
        for fragmented in [false, true] {
            let (sender, receiver) = tokio::io::duplex(64 * 1024);
            let mut receiver =
                WebSocketStream::from_raw_socket(receiver, Role::Client, Some(websocket_config()))
                    .await;
            let writer = tokio::spawn(async move {
                let mut sender = WebSocketStream::from_raw_socket(sender, Role::Server, None).await;
                let count = if fragmented {
                    MAX_WS_MESSAGE / MAX_WS_FRAME + 1
                } else {
                    1
                };
                for index in 0..count {
                    let data = vec![0; MAX_WS_FRAME + usize::from(!fragmented)];
                    let opcode = if index == 0 {
                        Data::Binary
                    } else {
                        Data::Continue
                    };
                    let frame = Frame::message(data, OpCode::Data(opcode), index + 1 == count);
                    if sender.send(Message::Frame(frame)).await.is_err() {
                        break;
                    }
                }
            });
            let error = tokio::time::timeout(Duration::from_secs(5), receiver.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(matches!(
                error,
                tokio_tungstenite::tungstenite::Error::Capacity(_)
            ));
            writer.abort();
            let _ = writer.await;
        }
    }

    #[test]
    fn http_hop_headers_do_not_remove_end_to_end_headers() {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            "keep-alive, x-private-hop".parse().unwrap(),
        );
        headers.insert("x-private-hop", "remove".parse().unwrap());
        headers.insert(header::AUTHORIZATION, "Bearer isolated".parse().unwrap());
        headers.insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
        strip_hop_headers(&mut headers);
        assert!(!headers.contains_key(header::CONNECTION));
        assert!(!headers.contains_key("x-private-hop"));
        assert_eq!(headers[header::AUTHORIZATION], "Bearer isolated");
        assert_eq!(headers[header::CONTENT_TYPE], "text/event-stream");
    }
}
