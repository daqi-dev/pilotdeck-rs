//! Streaming HTTP reverse proxy and upgrade coordination.
use crate::{Body, HttpClient, ProxyError, acceptor, pairing, upstream};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response, StatusCode, body::Incoming, header, server::conn::http1, service::service_fn,
};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::watch;
pub struct Proxy {
    pub upstream: String,
    pub client: HttpClient,
}

impl Proxy {
    /// All accepted connections use the same synchronous upgrade read boundary.
    pub async fn serve_connection<S>(self: Arc<Self>, stream: S) -> Result<(), hyper::Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        serve_connection_with(stream, move |request, lease| {
            let proxy = self.clone();
            async move { proxy.serve::<S>(request, lease).await }
        })
        .await
    }

    async fn serve<S>(
        &self,
        request: Request<Incoming>,
        lease: Option<ReadLease>,
    ) -> Result<Response<Body>, std::io::Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        self.serve_with_http::<S, _, _>(request, lease, |request| self.http(request))
            .await
    }

    async fn serve_with_http<S, F, Fut>(
        &self,
        mut request: Request<Incoming>,
        lease: Option<ReadLease>,
        http: F,
    ) -> Result<Response<Body>, std::io::Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnOnce(Request<Incoming>) -> Fut,
        Fut: std::future::Future<Output = Response<Body>>,
    {
        // Node dispatches any Upgrade protocol only when Connection also has
        // an upgrade token, then handleUpgrade checks the pathname and key.
        if is_upgrade(&request) {
            if request.uri().path() != "/ws" {
                // A service error makes Hyper close the socket without an HTTP
                // response, matching TS's socket.destroy() upgrade rejection.
                return Err(std::io::Error::other("upgrade endpoint not found"));
            }
            if acceptor::websocket_accept(request.headers()).is_none() {
                return Err(std::io::Error::other("missing websocket key"));
            }
            return Ok(match self.websocket::<S>(&mut request, lease).await {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("WS pairing: {error}");
                    plain(StatusCode::BAD_GATEWAY, "upstream websocket unavailable")
                }
            });
        }
        Ok(http(request).await)
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

    async fn websocket<S>(
        &self,
        request: &mut Request<Incoming>,
        lease: Option<ReadLease>,
    ) -> Result<Response<Body>, ProxyError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
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
            let upgraded = upgraded.await;
            drop(lease);
            match upgraded {
                Ok(downstream) => {
                    if let Err(error) =
                        pair_upgraded::<S, _>(downstream, TokioIo::new(upstream)).await
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

/// Node's upgrade head ends at the read that completed the HTTP headers.
/// Hyper otherwise performs another read while the service awaits upstream 101.
#[derive(Debug, Default)]
struct ReadState {
    paused: bool,
    waker: Option<Waker>,
}

type SharedReadState = Arc<Mutex<ReadState>>;

#[derive(Debug)]
pub(crate) struct ReadLease(SharedReadState);
impl Drop for ReadLease {
    fn drop(&mut self) {
        let waker = {
            let mut state = self.0.lock().expect("upgrade read state");
            state.paused = false;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

struct UpgradeReadGate<S> {
    stream: S,
    state: SharedReadState,
}
impl<S: AsyncRead + Unpin> AsyncRead for UpgradeReadGate<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        {
            let mut state = self.state.lock().expect("upgrade read state");
            if state.paused {
                state.waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
        }
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for UpgradeReadGate<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

pub(crate) fn is_upgrade(request: &Request<Incoming>) -> bool {
    request.headers().contains_key(header::UPGRADE)
        && request
            .headers()
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
}

pub(crate) async fn serve_connection_with<S, F, Fut>(
    stream: S,
    handler: F,
) -> Result<(), hyper::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(Request<Incoming>, Option<ReadLease>) -> Fut,
    Fut: std::future::Future<Output = Result<Response<Body>, std::io::Error>>,
{
    serve_connection_configured(stream, handler, false, None).await
}

/// Node v22.22.0's default `server.keepAliveTimeout` (the TS
/// `GatewayServer` does not override it and advertises
/// `Keep-Alive: timeout=5`): an idle keep-alive connection closes 5 s after
/// its last request/response activity (probe `probe-fixr3-a.out` A). Node
/// adds the fixed `keepAliveTimeoutBuffer` (1 s), so the TS close lands at
/// ~6 s; this port replicates the 5 s timeout itself and closes at exactly
/// 5 s, never later — the unconverged ~1 s buffer is registered in ledger
/// A8.4-NATIVE-HTTP-001 (alignment tracked in #1250).
const NODE_KEEP_ALIVE_IDLE: std::time::Duration = std::time::Duration::from_secs(5);

/// The native listener's connection serving. TS emits literal header cases
/// (`Date`, `Connection` capitalized), so responses render Title-Case, and
/// every native_http handler response sets `Date` explicitly, so the
/// automatic date stays off: the bare parser-level 400s then match TS's
/// undated ones. The byte-level transport diffs this leaves are registered
/// in the gateway-parity diff ledger.
///
/// `stop` mirrors Node's `server.close()` semantics (probes
/// `probe-fixr1.out` B, `probe-fixr2.out` D, `probe-fixr3-b.out` B1): a
/// connection that is idle keep-alive at that instant is closed right away,
/// while a connection with a request in flight — or one that never sent
/// bytes, including requests it sends only after the stop — is answered with
/// the normal keep-alive response and never force-closed; such sockets close
/// at their own 5 s keep-alive idle timeout (below). `graceful_shutdown` is
/// hyper's equivalent for the idle-at-close case and is a no-op for an
/// already-upgraded connection. A dropped sender means no stop signal, and
/// the connection simply runs to its natural end.
pub(crate) async fn serve_connection_native_with_shutdown<S, F, Fut>(
    stream: S,
    handler: F,
    stop: watch::Receiver<bool>,
) -> Result<(), hyper::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(Request<Incoming>, Option<ReadLease>) -> Fut,
    Fut: std::future::Future<Output = Result<Response<Body>, std::io::Error>>,
{
    serve_connection_configured(stream, handler, true, Some(stop)).await
}

/// Decrements the in-flight request count when the response future is done
/// or dropped (hyper can cancel a service future when the connection goes
/// away mid-request), so a stop never mistakes a torn-down request for an
/// idle connection.
struct InFlightRequest(Arc<AtomicUsize>);
impl Drop for InFlightRequest {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn serve_connection_configured<S, F, Fut>(
    stream: S,
    handler: F,
    native: bool,
    stop: Option<watch::Receiver<bool>>,
) -> Result<(), hyper::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(Request<Incoming>, Option<ReadLease>) -> Fut,
    Fut: std::future::Future<Output = Result<Response<Body>, std::io::Error>>,
{
    let state = SharedReadState::default();
    let stream = UpgradeReadGate {
        stream,
        state: state.clone(),
    };
    let served = Arc::new(AtomicBool::new(false));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let (activity, mut activity_rx) = watch::channel(None::<tokio::time::Instant>);
    let served_in_connection = served.clone();
    let in_flight_in_connection = in_flight.clone();
    let activity_in_connection = activity.clone();
    let service = service_fn(move |request| {
        served_in_connection.store(true, Ordering::Relaxed);
        // This must run before returning a future: Hyper can poll read again
        // before it polls that future. Invalid upgrade requests close anyway.
        let lease = if is_upgrade(&request)
            && request.uri().path() == "/ws"
            && acceptor::websocket_accept(request.headers()).is_some()
        {
            state.lock().expect("upgrade read state").paused = true;
            Some(ReadLease(state.clone()))
        } else {
            None
        };
        activity_in_connection.send_replace(Some(tokio::time::Instant::now()));
        in_flight_in_connection.fetch_add(1, Ordering::Relaxed);
        let in_flight = InFlightRequest(in_flight_in_connection.clone());
        let activity = activity_in_connection.clone();
        let handler = handler(request, lease);
        async move {
            let result = handler.await;
            drop(in_flight);
            activity.send_replace(Some(tokio::time::Instant::now()));
            result
        }
    });
    let mut builder = http1::Builder::new();
    if native {
        builder.title_case_headers(true).auto_date_header(false);
        if !crate::mutants::on("native-http-header-count-off") {
            // Node v22.22.0 has no header-count limit: its binding constraint
            // is the 16384-byte URL+header-names+values budget the router
            // replicates (probe `probe-fixr3-c.out` C4: 16363 one-byte
            // headers stay served, 16364 hits the byte budget), so a count
            // cap of 16384 never binds before that budget; it only bounds
            // hyper's per-parse smallvec allocation. hyper's default of 100
            // would 431 requests TS serves.
            if crate::mutants::on("native-http-header-cap-low") {
                builder.max_headers(3000);
            } else {
                builder.max_headers(16384);
            }
        }
    }
    let connection = builder
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades();
    let Some(stop) = stop else {
        return connection.await;
    };
    tokio::pin!(connection);
    // Node's keep-alive idle close, in force with or without a stop: the
    // deadline is the last request-parse or response-completion activity plus
    // 5 s, re-armed on every new activity. While a request is in flight the
    // timer never fires (Node's requestTimeout, not keepAliveTimeout, governs
    // a busy socket), and the next activity update re-arms it.
    let idle = async {
        if crate::mutants::on("native-http-keep-alive-timeout-off") {
            std::future::pending::<()>().await;
        }
        loop {
            let deadline = activity_rx
                .borrow_and_update()
                .map(|at| at + NODE_KEEP_ALIVE_IDLE);
            let Some(deadline) = deadline else {
                if activity_rx.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
                continue;
            };
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    if in_flight.load(Ordering::Relaxed) > 0 {
                        if activity_rx.changed().await.is_err() {
                            std::future::pending::<()>().await;
                        }
                        continue;
                    }
                    return;
                }
                _ = activity_rx.changed() => {}
            }
        }
    };
    tokio::pin!(idle);
    let mut stop = Some(stop);
    loop {
        tokio::select! {
            result = &mut connection => return result,
            changed = async {
                match stop.as_mut() {
                    Some(receiver) => receiver
                        .wait_for(|stopped| *stopped)
                        .await
                        .map(|stopped| *stopped),
                    None => std::future::pending::<
                        Result<bool, tokio::sync::watch::error::RecvError>,
                    >()
                    .await,
                }
            } => {
                if changed.is_err() {
                    // The sender is dropped without a stop: no stop can ever
                    // arrive, so only the idle close and the peer end this.
                    stop = None;
                    continue;
                }
                let idle_now = (served.load(Ordering::Relaxed)
                    && (in_flight.load(Ordering::Relaxed) == 0
                        || crate::mutants::on("native-http-inflight-close")))
                    || crate::mutants::on("native-http-unserved-close");
                if idle_now && !crate::mutants::on("native-http-idle-close-off") {
                    connection.as_mut().graceful_shutdown();
                }
                // close() only destroys the connections that were idle at
                // that instant; requests in flight and later requests are
                // answered normally, and their sockets still end at the
                // keep-alive idle close below, so the stop is consumed but
                // the connection keeps being served.
                stop = None;
                continue;
            }
            _ = &mut idle => {
                connection.as_mut().graceful_shutdown();
                return connection.as_mut().await;
            }
        }
    }
}

async fn pair_upgraded<S, U>(
    downstream: hyper::upgrade::Upgraded,
    upstream: U,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin,
{
    pairing::run(upgraded_stream::<S>(downstream)?, upstream).await
}

pub(crate) fn upgraded_stream<S>(downstream: hyper::upgrade::Upgraded) -> std::io::Result<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let parts = downstream
        .downcast::<TokioIo<UpgradeReadGate<S>>>()
        .map_err(|_| std::io::Error::other("unexpected downstream upgrade IO type"))?;
    // GatewayServer.ts:67 ignores Node's upgrade `head`. Hyper's equivalent
    // is read_buf: the synchronous gate ensures it contains only bytes read
    // with the HTTP headers, never a later read while waiting for upstream.
    // Upstream response read-ahead remains intact because it is TS output.
    drop(parts.read_buf);
    Ok(parts.io.into_inner().stream)
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
    use crate::test_native_ws_sample::RFC6455_SAMPLE_NONCE;
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
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
            RFC6455_SAMPLE_NONCE.parse().unwrap(),
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

    async fn rejected_upgrade(path: &str, key: Option<&str>, protocol: &str, connection: &str) {
        let (server_io, mut client_io) = tokio::io::duplex(4096);
        let proxy = Arc::new(Proxy {
            upstream: "http://127.0.0.1:28790".into(),
            client: Client::builder(TokioExecutor::new()).build_http(),
        });
        let server = tokio::spawn(async move { proxy.serve_connection(server_io).await });
        let key = key
            .map(|key| format!("Sec-WebSocket-Key: {key}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nUpgrade: {protocol}\r\nConnection: {connection}\r\n{key}\r\n"
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
        rejected_upgrade("/ws?allowed=query", None, "websocket", "Upgrade").await;
    }

    #[tokio::test]
    async fn only_ws_path_upgrades_and_other_paths_destroy_socket() {
        for path in ["/", "/WS", "/ws/", "/other?path=/ws"] {
            rejected_upgrade(path, Some("arbitrary-key"), "websocket", "Upgrade").await;
        }
    }

    #[tokio::test]
    async fn arbitrary_upgrade_protocol_uses_ts_path_and_key_rejections() {
        for protocol in ["h2c", "WebSocket, custom", "custom-protocol"] {
            rejected_upgrade("/other", Some("key"), protocol, "Upgrade").await;
            rejected_upgrade("/ws", None, protocol, "Upgrade").await;
        }
    }

    #[tokio::test]
    async fn upgrade_header_without_connection_token_uses_http_proxy() {
        timeout(Duration::from_secs(3), async {
            // Inject the HTTP handler so this routing regression needs no
            // listener or upstream service. Production uses Proxy::http.
            for connection in [None, Some("keep-alive"), Some("x-upgrade")] {
                let (server_io, client_io) = tokio::io::duplex(4096);
                let proxy = Arc::new(Proxy {
                    upstream: "http://fixture.invalid".into(),
                    client: Client::builder(TokioExecutor::new()).build_http(),
                });
                let server = tokio::spawn(async move {
                    serve_connection_with(server_io, move |request, lease| {
                        let proxy = proxy.clone();
                        async move {
                            proxy
                                .serve_with_http::<tokio::io::DuplexStream, _, _>(
                                    request,
                                    lease,
                                    |request| async move {
                                        assert_eq!(request.uri().path(), "/health");
                                        assert_eq!(request.headers()[header::UPGRADE], "h2c");
                                        plain(StatusCode::OK, "healthy")
                                    },
                                )
                                .await
                        }
                    })
                    .await
                    .unwrap();
                });
                let (mut sender, client) =
                    hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                        .await
                        .unwrap();
                let client = tokio::spawn(client);
                let mut request = Request::builder()
                    .uri("/health")
                    .header(header::HOST, "localhost")
                    .header(header::UPGRADE, "h2c")
                    .body(Full::new(Bytes::new()))
                    .unwrap();
                if let Some(connection) = connection {
                    request
                        .headers_mut()
                        .insert(header::CONNECTION, connection.parse().unwrap());
                }
                let response = sender.send_request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(
                    response.into_body().collect().await.unwrap().to_bytes(),
                    "healthy"
                );
                drop(sender);
                client.await.unwrap().unwrap();
                server.await.unwrap();
            }
        })
        .await
        .expect("HTTP routing test timeout");
    }

    #[tokio::test]
    async fn connection_upgrade_token_list_uses_h2c_path_and_key_rejections() {
        for connection in [
            "keep-alive, Upgrade",
            "keep-alive, uPgRaDe",
            "keep-alive\r\nConnection: \tUPGRADE\t, close",
        ] {
            rejected_upgrade("/health", Some("key"), "h2c", connection).await;
            rejected_upgrade("/ws", None, "h2c", connection).await;
        }
    }

    #[tokio::test]
    async fn get_and_first_frame_in_same_write_discards_upgrade_head() {
        use crate::frame::{client_frame, server_frame};
        use tokio::io::DuplexStream;

        timeout(Duration::from_secs(3), async {
            let (downstream, mut client) = tokio::io::duplex(4096);
            let (upstream, mut fake_ts) = tokio::io::duplex(4096);
            let first = client_frame(0x81, 127, b"discard this first frame");
            let second = client_frame(0x81, 126, b"forward only after upgrade");
            let mut request = b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: key\r\n\r\n".to_vec();
            request.extend_from_slice(&first);
            // Queue the entire packet before Hyper starts reading so the first
            // frame deterministically belongs to upgrade head, not a later read.
            client.write_all(&request).await.unwrap();
            let (upgrade_tx, upgrade_rx) = tokio::sync::oneshot::channel();
            let upgrade_tx = Arc::new(std::sync::Mutex::new(Some(upgrade_tx)));
            let http = tokio::spawn(async move {
                let handler = move |mut request: Request<Incoming>, lease| {
                    let upgraded = hyper::upgrade::on(&mut request);
                    upgrade_tx.lock().unwrap().take().unwrap().send((upgraded, lease)).unwrap();
                    async { Ok(acceptor::websocket_response(hyper::HeaderMap::new(), "accept")) }
                };
                serve_connection_with(downstream, handler).await.unwrap();
            });
            let (upgraded, _lease) = upgrade_rx.await.unwrap();
            let upgraded = upgraded.await.unwrap();
            let pair = tokio::spawn(pair_upgraded::<DuplexStream, _>(upgraded, upstream));
            let mut response = Vec::new();
            while !response.ends_with(b"\r\n\r\n") {
                response.push(client.read_u8().await.unwrap());
            }
            assert!(response.starts_with(b"HTTP/1.1 101"));
            client.write_all(&second).await.unwrap();
            let mut forwarded = vec![0; second.len()];
            fake_ts.read_exact(&mut forwarded).await.unwrap();
            assert_eq!(forwarded, second, "pre-read first frame must be discarded");
            let reply = server_frame(1, b"only second frame reached TS");
            fake_ts.write_all(&reply).await.unwrap();
            fake_ts.shutdown().await.unwrap();
            let mut output = Vec::new();
            client.read_to_end(&mut output).await.unwrap();
            assert_eq!(output, reply);
            pair.await.unwrap().unwrap();
            http.await.unwrap();
        }).await.expect("upgrade head test timeout");
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

#[cfg(test)]
#[path = "http_proxy_tests.rs"]
mod read_boundary_tests;
