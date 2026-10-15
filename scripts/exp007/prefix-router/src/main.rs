//! Unfrozen exp007 adapter: prefix selection and raw HTTP upgrade tunnels.
//! Default traffic goes through the unchanged, frozen exp008 proxy.
use std::{convert::Infallible, error::Error, net::SocketAddr, sync::Arc};

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::{body::Incoming, header, service::service_fn, Request, Response, StatusCode};
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo},
};
use tokio::net::TcpListener;

type RouterError = Box<dyn Error + Send + Sync>;
type Body = BoxBody<Bytes, RouterError>;

struct Routes {
    prefix: String,
    old: String,
    new: String,
}

fn address(value: &str) -> Result<SocketAddr, RouterError> {
    let address: SocketAddr = value.parse()?;
    if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() < 20000 {
        return Err("address must be 127.0.0.1 with a port >= 20000".into());
    }
    Ok(address)
}

fn origin(value: &str) -> Result<String, RouterError> {
    let uri: hyper::Uri = value.parse()?;
    if uri.scheme_str() != Some("http") || uri.path() != "/" || uri.query().is_some() {
        return Err("upstream must be an HTTP origin without a path/query".into());
    }
    address(uri.authority().ok_or("missing authority")?.as_str())?;
    Ok(value.trim_end_matches('/').to_owned())
}

impl Routes {
    fn from_env() -> Result<Self, RouterError> {
        let prefix = std::env::var("EXP007_PREFIX").unwrap_or_else(|_| "/probe".into());
        // A single literal path segment keeps classification identical to boundary.js.
        if !prefix.starts_with('/')
            || prefix.len() < 2
            || !prefix[1..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("prefix must be one literal path segment, e.g. /probe or /settings".into());
        }
        Ok(Self {
            prefix,
            old: origin(&std::env::var("EXP007_OLD_UPSTREAM")?)?,
            new: origin(&std::env::var("EXP007_NEW_UPSTREAM")?)?,
        })
    }

    fn upstream(&self, path: &str) -> &str {
        if within(path, &self.prefix) || within(path, "/shared") || within(path, "/vendor") {
            &self.new
        } else {
            &self.old
        }
    }

    fn prepare<B>(&self, request: &mut Request<B>) -> Result<bool, RouterError> {
        reject_dot_segments(request.uri().path())?;
        let upgrade = request.headers().get(header::UPGRADE).cloned();
        let wants_upgrade = connection_tokens(request.headers())?
            .iter()
            .any(|name| name.eq_ignore_ascii_case("upgrade"))
            && upgrade.is_some();
        let path = request.uri().path_and_query().map_or("/", |p| p.as_str());
        let uri: hyper::Uri = format!("{}{path}", self.upstream(request.uri().path())).parse()?;
        strip_hop_headers(request.headers_mut())?;
        request.headers_mut().insert(
            header::HOST,
            uri.authority().ok_or("authority")?.as_str().parse()?,
        );
        if wants_upgrade {
            request.headers_mut().insert(
                header::CONNECTION,
                header::HeaderValue::from_static("Upgrade"),
            );
            request
                .headers_mut()
                .insert(header::UPGRADE, upgrade.expect("checked"));
        }
        *request.uri_mut() = uri;
        Ok(wants_upgrade)
    }
}

fn within(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|tail| tail.starts_with('/'))
}

fn reject_dot_segments(path: &str) -> Result<(), RouterError> {
    // Inspect one URL-decoding pass without changing the forwarded URI.
    // Encoded separators also delimit segments after an upstream decode.
    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if let (Some(hi), Some(lo)) = (
                bytes.get(index + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(index + 2).and_then(|b| (*b as char).to_digit(16)),
            ) {
                decoded.push((hi * 16 + lo) as u8);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    if decoded
        .split(|b| *b == b'/')
        .any(|s| s == b"." || s == b"..")
    {
        return Err("dot path segment".into());
    }
    Ok(())
}

fn connection_tokens(headers: &hyper::HeaderMap) -> Result<Vec<String>, RouterError> {
    let mut tokens = Vec::new();
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
            let name = header::HeaderName::from_bytes(&part[start..end])?;
            tokens.push(name.as_str().to_owned());
        }
    }
    Ok(tokens)
}

fn strip_hop_headers(headers: &mut hyper::HeaderMap) -> Result<(), RouterError> {
    for name in connection_tokens(headers)? {
        headers.remove(name);
    }
    for name in [
        "connection",
        "upgrade",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
    ] {
        headers.remove(name);
    }
    Ok(())
}

fn failure(status: StatusCode, message: &'static str) -> Response<Body> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(message.as_bytes()))
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    response
}

fn streamed(response: Response<Incoming>) -> Response<Body> {
    response.map(|body| body.map_err(|e| -> RouterError { Box::new(e) }).boxed())
}

async fn tunnel<A, B>(mut down: A, mut up: B) -> std::io::Result<(u64, u64)>
where
    A: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    B: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::io::copy_bidirectional(&mut down, &mut up).await
}

async fn serve(
    routes: &Routes,
    client: &Client<hyper_util::client::legacy::connect::HttpConnector, Body>,
    mut request: Request<Incoming>,
) -> Response<Body> {
    let upgrading = match routes.prepare(&mut request) {
        Ok(value) => value,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid request headers or URI"),
    };
    let downstream = upgrading.then(|| hyper::upgrade::on(&mut request));
    let forwarded = request.map(|body| body.map_err(|e| -> RouterError { Box::new(e) }).boxed());
    let mut response = match client.request(forwarded).await {
        Ok(response) => response,
        Err(error) => {
            eprintln!("upstream: {error}");
            return failure(StatusCode::BAD_GATEWAY, "upstream unavailable");
        }
    };
    let switched = upgrading && response.status() == StatusCode::SWITCHING_PROTOCOLS;
    let upgrade_header = response.headers().get(header::UPGRADE).cloned();
    if strip_hop_headers(response.headers_mut()).is_err() {
        return failure(
            StatusCode::BAD_GATEWAY,
            "invalid upstream Connection header",
        );
    }
    if switched {
        let Some(upgrade_header) = upgrade_header else {
            return failure(StatusCode::BAD_GATEWAY, "missing upstream Upgrade header");
        };
        response.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("Upgrade"),
        );
        response
            .headers_mut()
            .insert(header::UPGRADE, upgrade_header);
        let upstream = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            match tokio::try_join!(downstream.expect("upgrade requested"), upstream) {
                Ok((down, up)) => {
                    if let Err(error) = tunnel(TokioIo::new(down), TokioIo::new(up)).await {
                        eprintln!("upgrade tunnel: {error}");
                    }
                }
                Err(error) => eprintln!("upgrade pairing: {error}"),
            }
        });
    }
    // Incoming is polled by the downstream HTTP writer: SSE/NDJSON are never collected.
    streamed(response)
}

#[tokio::main]
async fn main() -> Result<(), RouterError> {
    let listen =
        address(&std::env::var("EXP007_LISTEN").unwrap_or_else(|_| "127.0.0.1:28375".into()))?;
    let routes = Arc::new(Routes::from_env()?);
    let client = Client::builder(TokioExecutor::new()).build_http();
    let listener = TcpListener::bind(listen).await?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    eprintln!(
        "exp007 prefix router {listen}: {} -> {}, otherwise -> {}",
        routes.prefix, routes.new, routes.old
    );
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                let routes = routes.clone();
                let client = client.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        let routes = routes.clone();
                        let client = client.clone();
                        async move { Ok::<_, Infallible>(serve(&routes, &client, request).await) }
                    });
                    if let Err(error) = hyper::server::conn::http1::Builder::new()
                        .auto_date_header(false)
                        .serve_connection(TokioIo::new(socket), service).with_upgrades().await {
                        eprintln!("downstream: {error}");
                    }
                });
            }
            _ = terminate.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn routes(prefix: &str) -> Routes {
        Routes {
            prefix: prefix.into(),
            old: "http://127.0.0.1:28373".into(),
            new: "http://127.0.0.1:28370".into(),
        }
    }

    async fn rejected_in_memory(path: &str, connections: &[&str]) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let (client_io, server_io) = tokio::io::duplex(4096);
            let server = tokio::spawn(async move {
                let routes = routes("/probe");
                let client = Client::builder(TokioExecutor::new()).build_http();
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(server_io),
                        service_fn(|request| async {
                            Ok::<_, Infallible>(serve(&routes, &client, request).await)
                        }),
                    )
                    .await
                    .unwrap();
            });
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                    .await
                    .unwrap();
            let connection = tokio::spawn(connection);
            let mut request = Request::builder().uri(path);
            for value in connections {
                request = request.header(header::CONNECTION, *value);
            }
            let response = sender
                .send_request(request.body(Full::new(Bytes::new())).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{path} {connections:?}"
            );
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                "invalid request headers or URI"
            );
            drop(sender);
            connection.await.unwrap().unwrap();
            server.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn dot_segments_return_400_without_listening() {
        for path in [
            "/probe/../api",
            "/./probe",
            "/probe/.",
            "/probe/..",
            "/probe/%2e/api",
            "/probe/%2E%2e/api",
            "/probe/.%2E/api",
            "/%2e./probe",
            "/probe/a%2f..%2Fapi",
        ] {
            rejected_in_memory(path, &[]).await;
        }
        // Negative controls: dots within a name and query never form path segments.
        for path in [
            "/probe/a.b",
            "/probe/..asset",
            "/probe/%2efile",
            "/probe/%2e%2efoo",
            // Static servers decode once: these name literal percent directories.
            "/probe/%252e%252e/asset",
            "/probe/%252e%252e%252fasset",
            "/probe/file?next=/../api",
            "/api/v1.2",
        ] {
            let mut request = Request::builder().uri(path).body(()).unwrap();
            assert!(routes("/probe").prepare(&mut request).is_ok(), "{path}");
            assert_eq!(request.uri().path_and_query().unwrap().as_str(), path);
        }
    }

    #[test]
    fn upstream_response_rejects_empty_connection_tokens() {
        for value in ["", "keep-alive,", "keep-alive,,upgrade", " \t"] {
            let mut response = Response::builder()
                .header(header::CONNECTION, value)
                .body(())
                .unwrap();
            assert!(
                strip_hop_headers(response.headers_mut()).is_err(),
                "{value:?}"
            );
        }
        let mut response = Response::builder()
            .header(header::CONNECTION, "keep-alive, x-private-hop")
            .header("x-private-hop", "private")
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(())
            .unwrap();
        strip_hop_headers(response.headers_mut()).unwrap();
        assert!(!response.headers().contains_key(header::CONNECTION));
        assert!(!response.headers().contains_key("x-private-hop"));
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
    }

    #[tokio::test]
    async fn empty_connection_tokens_return_400_without_listening() {
        for values in [
            vec![""],
            vec![" \t"],
            vec![",upgrade"],
            vec!["upgrade,"],
            vec!["keep-alive,,upgrade"],
            vec!["keep-alive, \t,upgrade"],
            vec!["keep-alive", ""],
        ] {
            rejected_in_memory("/probe", &values).await;
        }
        for values in [
            vec![],
            vec![" \tkeep-alive\t "],
            vec!["keep-alive, upgrade"],
            vec!["keep-alive", "upgrade"],
        ] {
            let mut request = Request::builder().uri("/probe");
            for value in values {
                request = request.header(header::CONNECTION, value);
            }
            assert!(routes("/probe")
                .prepare(&mut request.body(()).unwrap())
                .is_ok());
        }
    }

    #[test]
    fn segment_boundary_and_query_preserved() {
        for prefix in ["/probe", "/settings"] {
            let routes = routes(prefix);
            for path in [
                prefix.to_owned(),
                format!("{prefix}/leptos/probe_bg.wasm"),
                "/shared/island.js".into(),
                "/vendor/xterm.js".into(),
            ] {
                let mut request = Request::builder()
                    .uri(format!("{path}?token=a%2Fb&x=1&x=2"))
                    .body(())
                    .unwrap();
                assert!(!routes.prepare(&mut request).unwrap());
                assert_eq!(
                    request.uri().authority().unwrap().as_str(),
                    "127.0.0.1:28370"
                );
                assert_eq!(request.uri().path(), path);
                assert_eq!(request.uri().query(), Some("token=a%2Fb&x=1&x=2"));
            }
            for path in [
                "/",
                "/probex",
                "/settingsx",
                "/ws",
                "/shell",
                "/plugin-ws/plugin",
                "/api/auth/login",
                "/__probe/sse",
            ] {
                assert_eq!(routes.upstream(path), routes.old);
            }
        }
    }

    #[test]
    fn headers_filter_all_connection_values_and_keep_end_to_end() {
        let mut request = Request::builder()
            .uri("/ws?token=secret")
            .header("connection", "Upgrade, X-Private-Hop")
            .header("connection", "x-second-hop, keep-alive")
            .header("upgrade", "websocket")
            .header("x-private-hop", "private")
            .header("x-second-hop", "private")
            .header("proxy-authorization", "private")
            .header("authorization", "Bearer test")
            .header("cookie", "session=test")
            .header("sec-websocket-protocol", "probe")
            .header("sec-websocket-key", "key")
            .body(())
            .unwrap();
        assert!(routes("/probe").prepare(&mut request).unwrap());
        for name in ["x-private-hop", "x-second-hop", "proxy-authorization"] {
            assert!(!request.headers().contains_key(name));
        }
        assert_eq!(request.headers()["connection"], "Upgrade");
        assert_eq!(request.headers()["sec-websocket-key"], "key");
        assert_eq!(request.headers()["authorization"], "Bearer test");
        assert_eq!(request.headers()["cookie"], "session=test");
        assert_eq!(request.headers()["sec-websocket-protocol"], "probe");
        assert_eq!(request.headers()["host"], "127.0.0.1:28373");
    }

    #[test]
    fn invalid_headers_and_unsafe_addresses_rejected() {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            header::HeaderValue::from_bytes(b"x-hop, \xff").unwrap(),
        );
        assert!(strip_hop_headers(&mut headers).is_err());
        for value in [
            "0.0.0.0:28375",
            "127.0.0.1:3001",
            "127.0.0.1:18789",
            "[::1]:28375",
        ] {
            assert!(address(value).is_err());
        }
        for value in [
            "https://127.0.0.1:28374",
            "http://localhost:28374",
            "http://127.0.0.1:28374/path",
            "http://127.0.0.1:28374/?query",
            "http://127.0.0.1:3001",
        ] {
            assert!(origin(value).is_err());
        }
        assert!(origin("http://127.0.0.1:28374").is_ok());
    }

    #[tokio::test]
    async fn raw_tunnel_retains_binary_control_frames_and_half_close() {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let (mut browser, down) = tokio::io::duplex(64);
            let (up, mut server) = tokio::io::duplex(64);
            let relay = tokio::spawn(tunnel(down, up));
            let bytes = b"\x89\x82\x01\x02\x03\x04\x00\xff";
            browser.write_all(bytes).await.unwrap();
            let mut received = [0; 8];
            server.read_exact(&mut received).await.unwrap();
            assert_eq!(&received, bytes);
            browser.shutdown().await.unwrap();
            let mut end = Vec::new();
            server.read_to_end(&mut end).await.unwrap();
            server.write_all(b"\x8a\x02\x00\xff").await.unwrap();
            server.shutdown().await.unwrap();
            browser.read_to_end(&mut end).await.unwrap();
            assert_eq!(end, b"\x8a\x02\x00\xff");
            assert_eq!(relay.await.unwrap().unwrap(), (8, 4));
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn sse_body_exposes_first_chunk_before_upstream_end() {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let (client_io, mut upstream) = tokio::io::duplex(4096);
            let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client_io)).await.unwrap();
            let connection = tokio::spawn(connection);
            let sent = tokio::spawn(async move {
                sender.send_request(Request::builder().uri("/events").body(Full::new(Bytes::new())).unwrap()).await.unwrap()
            });
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                upstream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
                assert!(request.len() < 1024);
            }
            upstream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\nD\r\ndata: first\n\n\r\n").await.unwrap();
            let mut response = streamed(sent.await.unwrap());
            assert_eq!(response.headers()["content-type"], "text/event-stream");
            assert_eq!(response.body_mut().frame().await.unwrap().unwrap().into_data().unwrap(), "data: first\n\n");
            assert!(tokio::time::timeout(std::time::Duration::from_millis(20), response.body_mut().frame()).await.is_err());
            upstream.write_all(b"E\r\ndata: second\n\n\r\n0\r\n\r\n").await.unwrap();
            assert_eq!(response.body_mut().frame().await.unwrap().unwrap().into_data().unwrap(), "data: second\n\n");
            assert!(response.body_mut().frame().await.is_none());
            connection.abort();
        }).await.unwrap();
    }
}
