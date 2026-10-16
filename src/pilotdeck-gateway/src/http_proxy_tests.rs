//! No listener, sleeps or scheduler guesses: each queued chunk is one read.
use super::*;
use crate::frame::client_frame;
use std::{
    collections::VecDeque,
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    sync::oneshot,
    time::{Duration, timeout},
};

const REQUEST: &[u8] = b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: key\r\n\r\n";

struct Scripted {
    io: DuplexStream,
    reads: VecDeque<Vec<u8>>,
    count: Arc<AtomicUsize>,
}
impl AsyncRead for Scripted {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if let Some(mut chunk) = self.reads.pop_front() {
            let count = chunk.len().min(buf.remaining());
            buf.put_slice(&chunk[..count]);
            if count < chunk.len() {
                self.reads.push_front(chunk.split_off(count));
            }
            self.count.fetch_add(1, Ordering::SeqCst);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for Scripted {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

async fn boundary_case(head: &[u8], split_headers: bool, split_later: bool, later: Vec<u8>) {
    boundary_stream(head, split_headers, split_later, later, false)
        .await
        .expect("complete upstream stream must match exactly through EOF");
}

#[derive(Debug, PartialEq, Eq)]
struct StreamMismatch {
    code: &'static str,
    expected: Vec<u8>,
    actual: Vec<u8>,
}

async fn boundary_stream(
    head: &[u8],
    split_headers: bool,
    split_later: bool,
    later: Vec<u8>,
    duplicate: bool,
) -> Result<(), StreamMismatch> {
    let (io, mut client) = tokio::io::duplex(4096);
    let (upstream, mut host) = tokio::io::duplex(4096);
    let count = Arc::new(AtomicUsize::new(0));
    let after = client_frame(0x81, 127, b"post101 frame preserved once");
    let mut reads = VecDeque::new();
    let split = if split_headers { REQUEST.len() / 2 } else { 0 };
    if split != 0 {
        reads.push_back(REQUEST[..split].to_vec());
    }
    let mut last_http_read = REQUEST[split..].to_vec();
    last_http_read.extend_from_slice(head);
    reads.push_back(last_http_read);
    let http_reads = reads.len();
    if split_later {
        reads.push_back(later[..5].to_vec());
        reads.push_back(later[5..].to_vec());
    } else {
        reads.push_back(later.clone());
    }
    let io = Scripted {
        io,
        reads,
        count: count.clone(),
    };
    let (upgrade_tx, upgrade_rx) = oneshot::channel();
    let upgrade_tx = Mutex::new(Some(upgrade_tx));
    let (release_tx, release_rx) = oneshot::channel();
    let release_rx = Mutex::new(Some(release_rx));
    let mut server = Box::pin(serve_connection_with(io, move |mut request, lease| {
        assert!(lease.is_some());
        let upgraded = hyper::upgrade::on(&mut request);
        upgrade_tx
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send((upgraded, lease))
            .unwrap();
        let release = release_rx.lock().unwrap().take().unwrap();
        async move {
            release.await.unwrap();
            Ok(acceptor::websocket_response(
                hyper::HeaderMap::new(),
                "accept",
            ))
        }
    }));
    // One explicit poll reaches the blocked service and Hyper's extra-read
    // opportunity. The gate must keep every later chunk in the raw stream.
    std::future::poll_fn(|cx| {
        assert!(server.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let (upgrade, lease) = upgrade_rx.await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), http_reads);
    release_tx.send(()).unwrap();
    let server = tokio::spawn(server);
    let upgraded = upgrade.await.unwrap();
    drop(lease);
    let pair = tokio::spawn(pair_upgraded::<Scripted, _>(upgraded, upstream));
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(client.read_u8().await.unwrap());
    }
    assert!(headers.starts_with(b"HTTP/1.1 101"));
    client.write_all(&after).await.unwrap();
    if duplicate {
        client.write_all(&later).await.unwrap();
    }
    let prefix_len = after.len() + later.len();
    // The negative waits for its actual injected bytes before terminating. This
    // is a causal hit barrier, not the verdict: both paths still read through EOF.
    let mut actual = vec![0; prefix_len + if duplicate { later.len() } else { 0 }];
    host.read_exact(&mut actual).await.unwrap();
    let expected = [later, after].concat();
    // Preserve the original upstream-FIN termination path. Once the production
    // pair is reaped its owned writer is dropped, giving this reader real EOF.
    host.shutdown().await.unwrap();
    let mut output = Vec::new();
    client.read_to_end(&mut output).await.unwrap();
    assert!(
        output.is_empty(),
        "no duplicate/local response for opaque frames"
    );
    pair.await.unwrap().unwrap();
    server.await.unwrap().unwrap();
    host.read_to_end(&mut actual).await.unwrap();
    if actual != expected {
        return Err(StreamMismatch {
            code: "UPSTREAM_FULL_STREAM_MISMATCH",
            expected,
            actual,
        });
    }
    Ok(())
}

#[tokio::test]
async fn actual_trailing_duplicate_is_rejected_by_full_stream_gate() {
    timeout(Duration::from_secs(3), async {
        let later = client_frame(0x81, 126, b"pre101 frame preserved once");
        let result = boundary_stream(&[], false, false, later.clone(), true).await;
        let rejected = result.expect_err("actual duplicate must be rejected by common gate");
        assert_eq!(rejected.code, "UPSTREAM_FULL_STREAM_MISMATCH");
        assert_eq!(
            rejected.actual,
            [rejected.expected, later].concat(),
            "negative must actually deliver exactly the trailing duplicate"
        );
    })
    .await
    .expect("actual duplicate must reach the same EOF byte gate");
}

#[tokio::test]
async fn pre101_reads_survive_and_only_initial_head_is_discarded() {
    timeout(Duration::from_secs(3), async {
        let full = client_frame(0x81, 127, b"discarded HTTP head");
        for head in [&[][..], full.as_slice(), &full[..5]] {
            for split_headers in [false, true] {
                for split_later in [false, true] {
                    boundary_case(
                        head,
                        split_headers,
                        split_later,
                        client_frame(0x81, 126, b"pre101 frame preserved once"),
                    )
                    .await;
                }
            }
        }
    })
    .await
    .expect("controlled read-boundary matrix");
}

#[tokio::test]
async fn failed_production_upgrade_releases_gate_for_next_http_request() {
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    timeout(Duration::from_secs(3), async {
        let (server_io, client_io) = tokio::io::duplex(4096);
        let proxy = Arc::new(Proxy {
            upstream: "invalid upstream".into(),
            client: Client::builder(TokioExecutor::new()).build_http(),
        });
        let server = tokio::spawn(proxy.serve_connection(server_io));
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        let upgrade = Request::builder()
            .uri("/ws")
            .header(header::HOST, "localhost")
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header(header::SEC_WEBSOCKET_KEY, "key")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = sender.send_request(upgrade).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "upstream websocket unavailable"
        );
        let ordinary = Request::builder()
            .uri("/health")
            .header(header::HOST, "localhost")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = sender.send_request(ordinary).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "invalid request URI"
        );
        drop(sender);
        connection.await.unwrap().unwrap();
        server.await.unwrap().unwrap();
    })
    .await
    .expect("failed upgrade keepalive must not stay gated");
}

// C1: a legal upgrade awaiting its host response may pause only its own
// connection. The same production Proxy must serve B before A is released.
#[tokio::test]
async fn pending_production_upgrade_does_not_pause_another_connection() {
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    timeout(Duration::from_secs(3), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Arc::new(Proxy {
            upstream: format!("http://{}", listener.local_addr().unwrap()),
            client: Client::builder(TokioExecutor::new()).build_http(),
        });
        let (seen_tx, seen_rx) = oneshot::channel();
        let (release_tx, mut release_rx) = oneshot::channel();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            let (mut a, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(a.read_u8().await.unwrap());
            }
            assert!(request.starts_with(b"GET /ws HTTP/1.1\r\n"));
            seen_tx.send(()).unwrap();
            let (mut b, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(b.read_u8().await.unwrap());
            }
            assert!(request.starts_with(b"GET /health HTTP/1.1\r\n"));
            assert!(matches!(release_rx.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
            b.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nhealthy").await.unwrap();
            b.shutdown().await.unwrap();
            release_rx.await.unwrap();
            a.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            a.shutdown().await.unwrap();
        });
        let (a_server, mut a_client) = tokio::io::duplex(4096);
        let a_proxy = proxy.clone();
        tasks.spawn(async move { a_proxy.serve_connection(a_server).await.unwrap() });
        a_client.write_all(b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==\r\n\r\n").await.unwrap();
        seen_rx.await.unwrap();
        let (b_server, mut b_client) = tokio::io::duplex(4096);
        tasks.spawn(async move { proxy.serve_connection(b_server).await.unwrap() });
        b_client.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut response = String::new();
        b_client.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("healthy"), "{response}");
        a_client.shutdown().await.unwrap();
        release_tx.send(()).unwrap();
        let mut response = String::new();
        a_client.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 502"), "{response}");
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
    }).await.expect("connection B must complete while connection A still awaits its host handshake");
}

#[tokio::test]
async fn empty_and_incomplete_http_eof_finish_without_gating() {
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    timeout(Duration::from_secs(3), async {
        for input in [&[][..], &REQUEST[..20]] {
            let (server_io, mut client) = tokio::io::duplex(4096);
            let proxy = Arc::new(Proxy {
                upstream: "invalid upstream".into(),
                client: Client::builder(TokioExecutor::new()).build_http(),
            });
            let server = tokio::spawn(proxy.serve_connection(server_io));
            client.write_all(input).await.unwrap();
            client.shutdown().await.unwrap();
            let result = server.await.unwrap();
            assert_eq!(result.is_ok(), input.is_empty());
        }
    })
    .await
    .expect("EOF must terminate HTTP connection");
}

#[test]
fn cancelling_service_lease_wakes_paused_reader() {
    use std::task::Wake;
    struct Counter(AtomicUsize);
    impl Wake for Counter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let counter = Arc::new(Counter(AtomicUsize::new(0)));
    let waker = Waker::from(counter.clone());
    let mut cx = Context::from_waker(&waker);
    let state = Arc::new(Mutex::new(ReadState {
        paused: true,
        waker: None,
    }));
    let (io, mut client) = tokio::io::duplex(64);
    let mut gated = UpgradeReadGate {
        stream: io,
        state: state.clone(),
    };
    let lease = ReadLease(state.clone());
    let mut bytes = [0; 1];
    assert!(
        Pin::new(&mut gated)
            .poll_read(&mut cx, &mut ReadBuf::new(&mut bytes))
            .is_pending()
    );
    let mut service_future = Box::pin(async move {
        let _lease = lease;
        std::future::pending::<()>().await;
    });
    assert!(service_future.as_mut().poll(&mut cx).is_pending());
    drop(service_future);
    assert!(!state.lock().unwrap().paused);
    assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    let mut write_context = Context::from_waker(&waker);
    assert!(matches!(
        Pin::new(&mut client).poll_write(&mut write_context, b"R"),
        Poll::Ready(Ok(1))
    ));
    let mut bytes = [0; 1];
    let mut buffer = ReadBuf::new(&mut bytes);
    assert!(matches!(
        Pin::new(&mut gated).poll_read(&mut cx, &mut buffer),
        Poll::Ready(Ok(()))
    ));
    assert_eq!(
        buffer.filled(),
        b"R",
        "same gate must read actual bytes after lease drop"
    );
}

#[tokio::test]
async fn ungated_hyper_control_pre_reads_later_frame_into_discarded_buffer() {
    timeout(Duration::from_secs(3), async {
        let (io, _client) = tokio::io::duplex(4096);
        let count = Arc::new(AtomicUsize::new(0));
        let later = client_frame(0x81, 126, b"lost by old whole-buffer drop");
        let io = Scripted {
            io,
            reads: [REQUEST.to_vec(), later.clone()].into(),
            count: count.clone(),
        };
        let (upgrade_tx, upgrade_rx) = oneshot::channel();
        let upgrade_tx = Mutex::new(Some(upgrade_tx));
        let (release_tx, release_rx) = oneshot::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let service = service_fn(move |mut request| {
            let upgraded = hyper::upgrade::on(&mut request);
            upgrade_tx
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(upgraded)
                .unwrap();
            let release = release_rx.lock().unwrap().take().unwrap();
            async move {
                release.await.unwrap();
                Ok::<_, std::io::Error>(acceptor::websocket_response(
                    hyper::HeaderMap::new(),
                    "accept",
                ))
            }
        });
        let mut server = Box::pin(
            http1::Builder::new()
                .serve_connection(TokioIo::new(io), service)
                .with_upgrades(),
        );
        std::future::poll_fn(|cx| {
            assert!(server.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let upgrade = upgrade_rx.await.unwrap();
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "ungated Hyper consumed the later read before 101"
        );
        release_tx.send(()).unwrap();
        let server = tokio::spawn(server);
        let parts = upgrade
            .await
            .unwrap()
            .downcast::<TokioIo<Scripted>>()
            .unwrap();
        assert_eq!(
            parts.read_buf, later,
            "the old drop(read_buf) loses this actual WS frame"
        );
        server.await.unwrap().unwrap();
    })
    .await
    .expect("ungated control");
}

#[tokio::test]
async fn blocked_failure_wakes_hyper_and_serves_already_available_next_request() {
    timeout(Duration::from_secs(3), async {
        let (io, mut client) = tokio::io::duplex(4096);
        let count = Arc::new(AtomicUsize::new(0));
        let next = b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".to_vec();
        let io = Scripted {
            io,
            reads: [REQUEST.to_vec(), next].into(),
            count: count.clone(),
        };
        let (release_tx, release_rx) = oneshot::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let mut server = Box::pin(serve_connection_with(io, move |request, lease| {
            let release = if request.uri().path() == "/ws" {
                assert!(lease.is_some());
                Some(release_rx.lock().unwrap().take().unwrap())
            } else {
                assert!(lease.is_none());
                assert_eq!(request.uri().path(), "/health");
                None
            };
            async move {
                let response = if let Some(release) = release {
                    release.await.unwrap();
                    plain(StatusCode::BAD_GATEWAY, "failed")
                } else {
                    plain(StatusCode::OK, "healthy")
                };
                drop(lease);
                Ok(response)
            }
        }));
        std::future::poll_fn(|cx| {
            assert!(server.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "next request is gated while upgrade is pending"
        );
        release_tx.send(()).unwrap();
        let server = tokio::spawn(server);
        let mut output = String::new();
        client.read_to_string(&mut output).await.unwrap();
        assert_eq!(output.matches("HTTP/1.1 502").count(), 1);
        assert_eq!(output.matches("HTTP/1.1 200").count(), 1);
        assert!(output.ends_with("healthy"));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        server.await.unwrap().unwrap();
    })
    .await
    .expect("blocked failure must restore already-readable keepalive IO");
}

#[tokio::test]
async fn upgrade_after_client_halfclose_preserves_pending_frame_then_ends() {
    timeout(Duration::from_secs(3), async {
        let (io, mut client) = tokio::io::duplex(4096);
        let (upstream, mut host) = tokio::io::duplex(4096);
        let count = Arc::new(AtomicUsize::new(0));
        let frame = client_frame(0x81, 126, b"frame before FIN");
        let io = Scripted {
            io,
            reads: [REQUEST.to_vec(), frame.clone()].into(),
            count: count.clone(),
        };
        let (upgrade_tx, upgrade_rx) = oneshot::channel();
        let upgrade_tx = Mutex::new(Some(upgrade_tx));
        let (release_tx, release_rx) = oneshot::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let mut server = Box::pin(serve_connection_with(io, move |mut request, lease| {
            let upgraded = hyper::upgrade::on(&mut request);
            upgrade_tx
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send((upgraded, lease))
                .unwrap();
            let release = release_rx.lock().unwrap().take().unwrap();
            async move {
                release.await.unwrap();
                Ok(acceptor::websocket_response(
                    hyper::HeaderMap::new(),
                    "accept",
                ))
            }
        }));
        std::future::poll_fn(|cx| {
            assert!(server.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        client.shutdown().await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let (upgraded, lease) = upgrade_rx.await.unwrap();
        release_tx.send(()).unwrap();
        let server = tokio::spawn(server);
        let upgraded = upgraded.await.unwrap();
        drop(lease);
        let pair = tokio::spawn(pair_upgraded::<Scripted, _>(upgraded, upstream));
        let mut actual = Vec::new();
        host.read_to_end(&mut actual).await.unwrap();
        assert_eq!(actual, frame);
        pair.await.unwrap().unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 101"));
        server.await.unwrap().unwrap();
    })
    .await
    .expect("upgrade FIN must not discard the pending frame or hang");
}

#[tokio::test]
async fn initial_partial_frame_suffix_is_not_rejoined_with_discarded_prefix() {
    timeout(Duration::from_secs(3), async {
        let full = client_frame(0x81, 127, b"discarded HTTP head");
        for split_headers in [false, true] {
            boundary_case(&full[..5], split_headers, false, full[5..].to_vec()).await;
        }
    })
    .await
    .expect("initial head prefix must never be restored");
}

// Pre-registered candidate boundary: after a real upstream handshake rejection,
// invalid initial WS head yields HTTP 400 and termination; a complete ordinary
// HTTP request in the same initial read is forwarded successfully afterward.
#[tokio::test]
async fn production_failure_with_initial_head_terminates_or_recovers_as_http() {
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    timeout(Duration::from_secs(3), async {
        for valid_http_head in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let host = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    headers.push(stream.read_u8().await.unwrap());
                }
                assert!(headers.starts_with(b"GET /ws HTTP/1.1\r\n"));
                stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                stream.shutdown().await.unwrap();
                if valid_http_head {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut headers = Vec::new();
                    while !headers.ends_with(b"\r\n\r\n") {
                        headers.push(stream.read_u8().await.unwrap());
                    }
                    assert!(headers.starts_with(b"GET /health HTTP/1.1\r\n"));
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nhealthy").await.unwrap();
                    stream.shutdown().await.unwrap();
                }
            });
            let (io, mut client) = tokio::io::duplex(4096);
            let count = Arc::new(AtomicUsize::new(0));
            let trailing = if valid_http_head {
                b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".to_vec()
            } else {
                client_frame(0x81, 126, b"invalid HTTP head")
            };
            let initial = [REQUEST, trailing.as_slice()].concat();
            let io = Scripted { io, reads: [initial].into(), count: count.clone() };
            let proxy = Arc::new(Proxy {
                upstream: origin,
                client: Client::builder(TokioExecutor::new()).build_http(),
            });
            let server = tokio::spawn(proxy.serve_connection(io));
            let mut response = String::new();
            client.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 502"), "{response}");
            assert_eq!(response.matches("HTTP/1.1 502").count(), 1);
            assert_eq!(count.load(Ordering::SeqCst), 1);
            let result = server.await.unwrap();
            if valid_http_head {
                assert_eq!(response.matches("HTTP/1.1 200").count(), 1, "{response}");
                assert!(response.ends_with("healthy"));
                result.unwrap();
            } else {
                assert_eq!(response.matches("HTTP/1.1 400").count(), 1, "{response}");
                assert!(result.unwrap_err().is_parse());
            }
            host.await.unwrap();
        }
    }).await.expect("initial head plus failed upgrade must terminate or serve the next HTTP request");
}

#[tokio::test]
async fn production_ws_get_without_token_and_post_upgrade_route_explicitly() {
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    timeout(Duration::from_secs(3), async {
        for (method, upgrade, expected) in [
            (hyper::Method::GET, false, StatusCode::BAD_REQUEST),
            (hyper::Method::POST, true, StatusCode::BAD_GATEWAY),
        ] {
            let (server_io, client_io) = tokio::io::duplex(4096);
            let proxy = Arc::new(Proxy {
                upstream: "invalid upstream".into(),
                client: Client::builder(TokioExecutor::new()).build_http(),
            });
            let server = tokio::spawn(proxy.serve_connection(server_io));
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                    .await
                    .unwrap();
            let connection = tokio::spawn(connection);
            let mut request = Request::builder()
                .method(method)
                .uri("/ws")
                .header(header::HOST, "localhost")
                .header(header::UPGRADE, "websocket");
            if upgrade {
                request = request
                    .header(header::CONNECTION, "Upgrade")
                    .header(header::SEC_WEBSOCKET_KEY, "key");
            }
            let response = sender
                .send_request(request.body(Full::new(Bytes::new())).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            response.into_body().collect().await.unwrap();
            drop(sender);
            connection.await.unwrap().unwrap();
            server.await.unwrap().unwrap();
        }
    })
    .await
    .expect("explicit /ws method and token routing");
}
