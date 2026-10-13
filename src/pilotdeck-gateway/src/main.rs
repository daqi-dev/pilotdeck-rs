//! A1: one downstream connection owns exactly one upstream connection.
//! Application frames and HTTP bodies are never parsed or rewritten here.
use std::{convert::Infallible, error::Error, net::SocketAddr, sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{
    Request, Response, StatusCode, body::Incoming, header, server::conn::http1, service::service_fn,
};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

type ProxyError = Box<dyn Error + Send + Sync>;
type Body = BoxBody<Bytes, ProxyError>;
type HttpClient = Client<HttpConnector, Incoming>;

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
        let origin: hyper::Uri = self.upstream.parse()?;
        let authority = origin.authority().expect("validated upstream").as_str();
        // Hyper preserves bytes read beyond the HTTP upgrade in Upgraded.
        // No WS codec on either peer: TS alone owns RSV/continuation/masking,
        // control frames, close replies and application connection state.
        let (upstream, upstream_headers) = tokio::time::timeout(Duration::from_secs(10), async {
            let socket = TcpStream::connect(authority).await?;
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(socket)).await?;
            tokio::spawn(async move {
                if let Err(error) = connection.with_upgrades().await {
                    eprintln!("upstream HTTP upgrade: {error}");
                }
            });
            let mut upgrade = Request::builder()
                .method(request.method().clone())
                .uri(path)
                .body(Full::new(Bytes::new()))?;
            *upgrade.headers_mut() = request.headers().clone();
            upgrade
                .headers_mut()
                .insert(header::HOST, authority.parse()?);
            let mut response = sender.send_request(upgrade).await?;
            if response.status() != StatusCode::SWITCHING_PROTOCOLS
                || response
                    .headers()
                    .get(header::SEC_WEBSOCKET_ACCEPT)
                    .is_none_or(|value| value.as_bytes() != accept.as_bytes())
            {
                return Err::<_, ProxyError>("upstream rejected websocket upgrade".into());
            }
            let headers = response.headers().clone();
            let upgraded = hyper::upgrade::on(&mut response).await?;
            Ok((upgraded, headers))
        })
        .await??;
        let upgraded = hyper::upgrade::on(request);
        tokio::spawn(async move {
            match upgraded.await {
                Ok(downstream) => {
                    let mut downstream = TokioIo::new(downstream);
                    let mut upstream = TokioIo::new(upstream);
                    if let Err(error) = copy_bidirectional(&mut downstream, &mut upstream).await {
                        eprintln!("WS byte tunnel: {error}");
                    }
                    // Dropping the paired streams closes both owned endpoints.
                }
                Err(error) => eprintln!("downstream upgrade: {error}"),
            }
        });
        Ok(websocket_response(upstream_headers, &accept))
    }
}

fn fatal_accept_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::InvalidInput
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::Unsupported
    )
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn byte_tunnel_preserves_nonstandard_frames_and_half_close() {
        let (mut client, mut downstream) = tokio::io::duplex(64 * 1024);
        let (mut upstream, mut server) = tokio::io::duplex(64 * 1024);
        let relay = tokio::spawn(async move {
            copy_bidirectional(&mut downstream, &mut upstream)
                .await
                .unwrap()
        });
        // RSV text, orphan continuation, unmasked text, custom close: exact
        // bytes reach TS; no local validation or synthesized close response.
        let mut sent = vec![
            0xc1,
            0x81,
            1,
            2,
            3,
            4,
            b'x' ^ 1,
            0x80,
            0x80,
            1,
            2,
            3,
            4,
            0x81,
            1,
            b'x',
            0x88,
            0x82,
            1,
            2,
            3,
            4,
            0x11 ^ 1,
            0x5c ^ 2,
        ];
        sent.extend(vec![42; 128 * 1024]);
        let expected = sent.clone();
        let client_task = tokio::spawn(async move {
            client.write_all(&sent).await.unwrap();
            client.shutdown().await.unwrap();
            let mut reply = Vec::new();
            client.read_to_end(&mut reply).await.unwrap();
            reply
        });
        let mut received = Vec::new();
        server.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, expected);
        let reply = vec![0x88, 2, 3, 232]; // actual TS close 1000, empty reason
        server.write_all(&reply).await.unwrap();
        server.shutdown().await.unwrap();
        assert_eq!(client_task.await.unwrap(), reply);
        assert_eq!(relay.await.unwrap(), (expected.len() as u64, 4));
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
