//! Streaming HTTP reverse proxy and upgrade coordination.
use crate::{Body, HttpClient, ProxyError, acceptor, pairing, upstream};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Incoming, header};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
pub struct Proxy {
    pub upstream: String,
    pub client: HttpClient,
}

impl Proxy {
    pub async fn serve(
        &self,
        mut request: Request<Incoming>,
    ) -> Result<Response<Body>, std::io::Error> {
        let is_upgrade = request
            .headers()
            .get(header::UPGRADE)
            .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"));
        if is_upgrade {
            if request.uri().path() != "/ws" {
                // A service error makes Hyper close the socket without an HTTP
                // response, matching TS's socket.destroy() upgrade rejection.
                return Err(std::io::Error::other("upgrade endpoint not found"));
            }
            if acceptor::websocket_accept(request.headers()).is_none() {
                return Err(std::io::Error::other("missing websocket key"));
            }
            return Ok(match self.websocket(&mut request).await {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("WS pairing: {error}");
                    plain(StatusCode::BAD_GATEWAY, "upstream websocket unavailable")
                }
            });
        }
        Ok(self.http(request).await)
    }

    async fn http(&self, mut request: Request<Incoming>) -> Response<Body> {
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
        let accept = acceptor::websocket_accept(request.headers()).expect("validated key");
        let path = request
            .uri()
            .path_and_query()
            .map_or("/ws", |value| value.as_str());
        let (upstream, upstream_headers) = upstream::connect(
            &self.upstream,
            path,
            request.method(),
            request.headers(),
            &accept,
        )
        .await?;
        let upgraded = hyper::upgrade::on(request);
        tokio::spawn(async move {
            match upgraded.await {
                Ok(downstream) => {
                    if let Err(error) =
                        pairing::run(TokioIo::new(downstream), TokioIo::new(upstream)).await
                    {
                        eprintln!("WS pairing: {error}");
                    }
                }
                Err(error) => eprintln!("downstream upgrade: {error}"),
            }
        });
        Ok(acceptor::websocket_response(upstream_headers, &accept))
    }
}

pub(crate) fn strip_hop_headers(headers: &mut hyper::HeaderMap) {
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

pub(crate) fn empty() -> Body {
    Full::new(Bytes::new())
        .map_err(|never: Infallible| match never {})
        .boxed()
}
pub(crate) fn plain(status: StatusCode, message: &'static str) -> Response<Body> {
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
    use hyper::{server::conn::http1, service::service_fn};
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    use std::sync::Arc;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        time::{Duration, timeout},
    };

    #[test]
    fn upgrade_key_acceptance_matches_ts_without_rfc_validation() {
        let mut headers = hyper::HeaderMap::new();
        assert!(acceptor::websocket_accept(&headers).is_none());
        for key in ["", "not-base64", "YQ=="] {
            headers.insert(header::SEC_WEBSOCKET_KEY, key.parse().unwrap());
            assert!(acceptor::websocket_accept(&headers).is_some());
        }
        headers.insert(
            header::SEC_WEBSOCKET_KEY,
            "dGhlIHNhbXBsZSBub25jZQ==".parse().unwrap(),
        );
        assert_eq!(
            acceptor::websocket_accept(&headers).as_deref(),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
        // Method and Sec-WebSocket-Version do not participate in TS accept.
        headers.insert(header::SEC_WEBSOCKET_VERSION, "12".parse().unwrap());
        assert!(acceptor::websocket_accept(&headers).is_some());
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
        let response = acceptor::websocket_response(headers, "downstream-key");
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

    async fn rejected_upgrade(path: &str, key: Option<&str>) {
        let (server_io, mut client_io) = tokio::io::duplex(4096);
        let proxy = Arc::new(Proxy {
            upstream: "http://127.0.0.1:28790".into(),
            client: Client::builder(TokioExecutor::new()).build_http(),
        });
        let server = tokio::spawn(async move {
            let service = service_fn(move |request| {
                let proxy = proxy.clone();
                async move { proxy.serve(request).await }
            });
            http1::Builder::new()
                .serve_connection(TokioIo::new(server_io), service)
                .with_upgrades()
                .await
        });
        let key = key
            .map(|key| format!("Sec-WebSocket-Key: {key}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n{key}\r\n"
        );
        timeout(
            Duration::from_secs(3),
            client_io.write_all(request.as_bytes()),
        )
        .await
        .unwrap()
        .unwrap();
        let mut bytes = Vec::new();
        timeout(Duration::from_secs(3), client_io.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(bytes.is_empty(), "TS destroys socket without HTTP/WS reply");
        assert!(
            timeout(Duration::from_secs(3), server)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn missing_client_key_destroys_socket_before_upstream_connect() {
        rejected_upgrade("/ws?allowed=query", None).await;
    }

    #[tokio::test]
    async fn only_ws_path_upgrades_and_other_paths_destroy_socket() {
        for path in ["/", "/WS", "/ws/", "/other?path=/ws"] {
            rejected_upgrade(path, Some("arbitrary-key")).await;
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
