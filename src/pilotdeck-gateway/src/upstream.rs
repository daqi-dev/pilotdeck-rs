//! One raw upgraded TS connection per downstream. No RFC-strict frame codec.
use crate::{ProxyError, frame::Frame};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{HeaderMap, Method, Request, StatusCode, header, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use std::time::Duration;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};

pub(crate) async fn connect(
    origin: &str,
    path: &str,
    method: &Method,
    headers: &HeaderMap,
    accept: &str,
) -> Result<(Upgraded, HeaderMap), ProxyError> {
    let origin: hyper::Uri = origin.parse()?;
    let authority = origin.authority().expect("validated upstream").as_str();
    tokio::time::timeout(Duration::from_secs(10), async {
        let socket = TcpStream::connect(authority).await?;
        handshake(socket, authority, path, method, headers, accept).await
    })
    .await?
}

async fn handshake<S>(
    socket: S,
    authority: &str,
    path: &str,
    method: &Method,
    headers: &HeaderMap,
    accept: &str,
) -> Result<(Upgraded, HeaderMap), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(socket)).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            eprintln!("upstream HTTP upgrade: {error}");
        }
    });
    let mut upgrade = Request::builder()
        .method(method.clone())
        .uri(path)
        .body(Full::new(Bytes::new()))?;
    *upgrade.headers_mut() = headers.clone();
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
}

pub(crate) async fn forward<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Frame,
) -> std::io::Result<()> {
    // Decoder admits only masked client frames. Retaining the original mask key
    // preserves both the wire header and the decoded payload; never re-key it.
    writer.write_all(&frame.wire).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{client_frame, server_frame};
    use crate::test_native_ws_sample::RFC6455_SAMPLE_NONCE;
    use tokio::{io::AsyncReadExt, time::timeout};

    const ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("host", "downstream.invalid"),
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-key", RFC6455_SAMPLE_NONCE),
            ("sec-websocket-version", "13"),
            ("x-caller", "unchanged"),
        ] {
            headers.insert(
                hyper::header::HeaderName::from_static(name),
                value.parse().unwrap(),
            );
        }
        headers
    }

    async fn request(io: &mut tokio::io::DuplexStream) -> String {
        let mut bytes = Vec::new();
        loop {
            bytes.push(io.read_u8().await.unwrap());
            if bytes.ends_with(b"\r\n\r\n") {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn upgrade_preserves_query_headers_and_bytes_after_http_response() {
        timeout(Duration::from_secs(3), async {
            let (socket, mut server) = tokio::io::duplex(4096);
            let early = server_frame(1, br#"{"type":"hello_ok","exact":true}"#);
            let masked = client_frame(0xf1, 127, b"hello unchanged");
            let expected = masked.clone();
            let server_task = tokio::spawn(async move {
                let request = request(&mut server).await;
                assert!(request.starts_with("GET /ws?kept=query HTTP/1.1\r\n"));
                let lowercase = request.to_ascii_lowercase();
                assert!(lowercase.contains("host: fixture.invalid:28790\r\n"));
                assert!(lowercase.contains("x-caller: unchanged\r\n"));
                assert!(request.contains(RFC6455_SAMPLE_NONCE));
                let mut response = format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {ACCEPT}\r\nX-Upstream: retained\r\n\r\n").into_bytes();
                response.extend_from_slice(&early);
                server.write_all(&response).await.unwrap();
                let mut bytes = vec![0; expected.len()];
                server.read_exact(&mut bytes).await.unwrap();
                assert_eq!(bytes, expected);
                early
            });
            let (upgraded, response_headers) = handshake(socket, "fixture.invalid:28790", "/ws?kept=query", &Method::GET, &headers(), ACCEPT).await.unwrap();
            assert_eq!(response_headers["x-upstream"], "retained");
            let mut io = TokioIo::new(upgraded);
            io.write_all(&masked).await.unwrap();
            let early = server_task.await.unwrap();
            let mut actual = Vec::new();
            io.read_to_end(&mut actual).await.unwrap();
            assert_eq!(actual, early, "Hyper must retain bytes read past upgrade");
        }).await.expect("upgrade test timeout");
    }

    #[tokio::test]
    async fn upstream_rejection_or_wrong_accept_fails_handshake() {
        timeout(Duration::from_secs(3), async {
            for response in [
                "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n",
                "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: wrong\r\n\r\n",
            ] {
                let (socket, mut server) = tokio::io::duplex(4096);
                let server_task = tokio::spawn(async move {
                    request(&mut server).await;
                    server.write_all(response.as_bytes()).await.unwrap();
                });
                assert!(handshake(socket, "fixture.invalid", "/ws", &Method::GET, &headers(), ACCEPT).await.is_err());
                server_task.await.unwrap();
            }
        }).await.expect("rejection test timeout");
    }
}
