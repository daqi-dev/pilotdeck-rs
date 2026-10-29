use super::*;
#[path = "../../tests/common/native_ws_sample.rs"]
mod native_ws_sample;
use crate::{
    negative_assert, negative_assert_eq,
    runtime::{
        connection::{Call, RequestError},
        turn::gateway::{GatewayRequests, tests::native_ws_fixture},
    },
};
use native_ws_sample::RFC6455_SAMPLE_NONCE;
use pilotdeck_protocol::{JsString, JsValue};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::{sync::Notify, time::timeout};
const DEADLINE: Duration = Duration::from_secs(5);
const ORACLE: &str = include_str!("../../tests/fixtures/native-ws-entry/oracle.json");
#[derive(Default)]
struct Requests {
    hold: AtomicBool,
    large: AtomicBool,
    calls: AtomicUsize,
    changed: Notify,
    entered: Notify,
    release: Notify,
}
impl GatewayRequests for Requests {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.changed.notify_one();
            if self.hold.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(crate::metadata::object([
                (
                    "name",
                    if self.large.load(Ordering::SeqCst) {
                        JsValue::String("x".repeat(65536).into())
                    } else {
                        "native-ws-probe".into()
                    },
                ),
                ("capabilities", JsValue::Array(vec![])),
            ]))
        })
    }
    fn request(&self, _: JsString, _: JsValue) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async { Err(RequestError::new("#749 unexpected method")) })
    }
}
struct Client {
    socket: TcpStream,
    decoder: Decoder,
}
impl Client {
    async fn connect(address: SocketAddr) -> Self {
        let mut socket = TcpStream::connect(address).await.expect("#749 TCP connect");
        socket.write_all(format!("GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {RFC6455_SAMPLE_NONCE}\r\n\r\n").as_bytes()).await.expect("#749 upgrade write");
        let mut header = vec![];
        timeout(DEADLINE, async {
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.expect("#749 upgrade response byte"));
            }
        })
        .await
        .expect("#749 upgrade deadline");
        assert!(
            String::from_utf8_lossy(&header).starts_with("HTTP/1.1 101"),
            "#749 handshake status"
        );
        Self {
            socket,
            decoder: Decoder::default(),
        }
    }
    async fn frame(&mut self, opcode: u8, payload: &[u8]) {
        let code = if payload.len() < 126 {
            payload.len() as u8
        } else {
            126
        };
        self.socket
            .write_all(&frame::client_frame(0x80 | opcode, code, payload))
            .await
            .expect("#749 client frame write");
    }
    async fn send(&mut self, value: Value) {
        self.frame(1, value.to_string().as_bytes()).await;
    }
    async fn read(&mut self) -> (u8, Vec<u8>) {
        let result = timeout(DEADLINE, async {
            loop {
                if let Some(frame) = self
                    .decoder
                    .next_server()
                    .expect("#749 server frame decode")
                {
                    return (frame.opcode, frame.payload);
                }
                let n = self
                    .socket
                    .read_buf(&mut self.decoder.buffer)
                    .await
                    .expect("#749 response read");
                assert!(n > 0, "#749 response unexpected EOF");
            }
        })
        .await;
        crate::test_negative_receipt::expect_ok(result, "#749 text delivered")
    }
    async fn json(&mut self) -> Value {
        let (op, bytes) = self.read().await;
        assert_eq!(op, 1, "#749 text opcode");
        serde_json::from_slice(&bytes).expect("#749 response JSON")
    }
    async fn hello(&mut self) {
        self.send(hello("fixture-token")).await;
    }
    async fn close(&mut self) {
        self.frame(8, &[]).await;
        let (op, payload) = self.read().await;
        assert_eq!(
            (op, payload),
            (8, 1000u16.to_be_bytes().to_vec()),
            "#749 normal close reply"
        );
        self.socket
            .shutdown()
            .await
            .expect("#749 close publishes FIN");
    }
}
fn hello(token: &str) -> Value {
    json!({"type":"hello","protocolVersion":"1.2","clientName":"ui","clientVersion":"probe","token":token})
}
async fn start(
    requests: Arc<Requests>,
    budget: Duration,
) -> (
    SocketAddr,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), ProxyError>>,
) {
    let host = native_ws_fixture(requests);
    let server = NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#749 listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#749 bind native");
    let address = server.local_addr().expect("#749 bound address");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        budget,
    ));
    (address, stop, task)
}
async fn finish(stop: oneshot::Sender<()>, task: tokio::task::JoinHandle<Result<(), ProxyError>>) {
    stop.send(()).expect("#749 stop server");
    timeout(DEADLINE, task)
        .await
        .expect("#749 server finish deadline")
        .expect("#749 server task")
        .expect("#749 successful drain");
}
fn oracle() -> Value {
    crate::test_baseline::oracle(ORACLE);
    serde_json::from_str::<Value>(ORACLE).expect("#749 oracle JSON")["observations"].clone()
}
#[tokio::test]
async fn handshake_and_auth() {
    let expected = oracle();
    let (address, stop, task) = start(Arc::default(), DEADLINE).await;
    let mut client = Client::connect(address).await;
    client.hello().await;
    negative_assert_eq!(
        client.json().await["type"],
        expected["hello"],
        "#749 hello reply"
    );
    client.close().await;
    for (input, key) in [
        (hello("wrong").to_string(), "badToken"),
        ("{".into(), "invalidJson"),
    ] {
        let mut client = Client::connect(address).await;
        client.frame(1, input.as_bytes()).await;
        let (op, bytes) = client.read().await;
        assert_eq!(op, 8, "#749 rejected hello close opcode");
        assert!(bytes.len() >= 2, "#749 rejected hello close payload");
        let actual = json!([
            u16::from_be_bytes([bytes[0], bytes[1]]),
            String::from_utf8_lossy(&bytes[2..])
        ]);
        negative_assert_eq!(actual, expected[key], "#749 auth close matches TS");
    }
    finish(stop, task).await;
}
#[tokio::test]
async fn invalid_upgrade() {
    let (address, stop, task) = start(Arc::default(), DEADLINE).await;
    let mut replies = vec![];
    for (path, key) in [("/wrong", "Sec-WebSocket-Key: key\r\n"), ("/ws", "")] {
        let mut socket = TcpStream::connect(address)
            .await
            .expect("#749 invalid upgrade connect");
        socket.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n{key}\r\n").as_bytes()).await.expect("#749 invalid upgrade send");
        let mut bytes = vec![];
        let reply = crate::test_negative_receipt::expect_ok(
            timeout(DEADLINE, socket.read_to_end(&mut bytes)).await,
            "#749 invalid upgrade rejected",
        );
        crate::test_negative_receipt::expect_ok(reply, "#749 invalid upgrade rejected");
        replies.push(String::from_utf8(bytes).expect("#749 invalid upgrade UTF8"));
    }
    negative_assert_eq!(
        json!(replies),
        oracle()["invalidUpgrade"],
        "#749 invalid upgrade rejected"
    );
    finish(stop, task).await;
}
#[tokio::test]
async fn concurrent_connections() {
    let requests = Arc::new(Requests::default());
    requests.hold.store(true, Ordering::SeqCst);
    let (address, stop, task) = start(requests.clone(), DEADLINE).await;
    let mut a = Client::connect(address).await;
    a.hello().await;
    timeout(DEADLINE, requests.entered.notified())
        .await
        .expect("#749 predecessor entered");
    let mut b = Client::connect(address).await;
    b.hello().await;
    negative_assert_eq!(
        b.json().await["type"],
        oracle()["concurrent"],
        "#749 independent connection"
    );
    requests.release.notify_one();
    assert_eq!(
        a.json().await["type"],
        oracle()["predecessor"],
        "#749 predecessor released"
    );
    a.close().await;
    b.close().await;
    finish(stop, task).await;
}
#[tokio::test]
async fn pending_request_and_disconnect() {
    for disconnect in [false, true] {
        let (address, stop, task) = start(Arc::default(), DEADLINE).await;
        let mut client = Client::connect(address).await;
        client.hello().await;
        client.json().await;
        client.send(json!({"type":"request","id":"turn","method":"submit_turn","params":{"sessionKey":"s","runId":"r","message":"hello","channelKey":"test"}})).await;
        assert_eq!(
            client.json().await["event"]["type"],
            "turn_started",
            "#749 submit entered before cancellation"
        );
        if disconnect {
            client.close().await;
        } else {
            client.send(json!({"type":"request","id":"abort","method":"abort_turn","params":{"sessionKey":"s"}})).await;
            loop {
                let row = client.json().await;
                if row["id"] == "abort" {
                    assert_eq!(row["ok"], true, "#749 same socket abort");
                    break;
                }
            }
            client.close().await;
        }
        let stopped = stop.send(());
        assert!(stopped.is_ok(), "#749 stop cancelled submit");
        let finished = timeout(DEADLINE, task).await;
        let result = crate::test_negative_receipt::expect_ok(
            finished,
            "#749 disconnect drains pending submit",
        )
        .expect("#749 pending server join");
        crate::test_negative_receipt::expect_ok(result, "#749 disconnect drains pending submit");
    }
}
#[tokio::test]
async fn stop_accepting_drains_existing() {
    let (address, stop, task) = start(Arc::default(), DEADLINE).await;
    let mut client = Client::connect(address).await;
    client.hello().await;
    client.json().await;
    stop.send(()).expect("#749 stop accepting");
    timeout(DEADLINE, async {
        loop {
            if TcpStream::connect(address).await.is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#749 listener stopped deadline");
    negative_assert!(!task.is_finished(), "#749 drain waits for existing socket");
    client.frame(9, b"alive").await;
    assert_eq!(
        client.read().await,
        (10, b"alive".to_vec()),
        "#749 existing socket after stop"
    );
    negative_assert!(!task.is_finished(), "#749 drain waits for existing socket");
    client.close().await;
    timeout(DEADLINE, task)
        .await
        .expect("#749 drain finish deadline")
        .expect("#749 drain join")
        .expect("#749 drain completed");
}
#[tokio::test]
async fn bounded_drain() {
    let requests = Arc::new(Requests::default());
    requests.hold.store(true, Ordering::SeqCst);
    let (address, stop, task) = start(requests.clone(), Duration::from_millis(50)).await;
    let mut client = Client::connect(address).await;
    client.hello().await;
    timeout(DEADLINE, requests.entered.notified())
        .await
        .expect("#749 never completing predecessor entered");
    stop.send(()).expect("#749 stop blocked server");
    let result = timeout(DEADLINE, task)
        .await
        .expect("#749 bounded host exit")
        .expect("#749 blocked task join");
    negative_assert!(result.is_err(), "#749 stuck drain is an error");
}
#[tokio::test]
async fn shutdown_cancels_handshake() {
    let (address, stop, task) = start(Arc::default(), Duration::from_millis(50)).await;
    let mut socket = TcpStream::connect(address)
        .await
        .expect("#749 half handshake connect");
    socket
        .write_all(b"GET /ws HTTP/1.1\r\n")
        .await
        .expect("#749 half handshake write");
    // A second completed handshake is an accept-loop barrier for the first.
    let mut barrier = Client::connect(address).await;
    barrier.close().await;
    stop.send(()).expect("#749 stop half handshake");
    let result = timeout(DEADLINE, task)
        .await
        .expect("#749 half handshake bounded")
        .expect("#749 half handshake join");
    negative_assert!(result.is_err(), "#749 unfinished handshake drain is error");
}
#[test]
fn default_mode_unchanged() {
    use sha2::{Digest, Sha256};
    negative_assert_eq!(
        format!("{:x}", Sha256::digest(include_bytes!("../main.rs"))),
        "398167116815fdca70188a7c8e9230ba87010d6db11a0fae42a82a2bb22f5823",
        "#749 A4 executable remains unchanged"
    );
}
#[tokio::test]
async fn close_tail_resumes_on_next_data() {
    let requests = Arc::new(Requests::default());
    let host = native_ws_fixture(requests.clone());
    let (server, mut client) = tokio::io::duplex(4096);
    let task = tokio::spawn(transport(
        server,
        host.clone(),
        "fixture-token".into(),
        "probe".into(),
    ));
    let hello = hello("fixture-token").to_string();
    client
        .write_all(&frame::client_frame(
            0x81,
            hello.len() as u8,
            hello.as_bytes(),
        ))
        .await
        .expect("#749 next-data hello");
    timeout(DEADLINE, requests.changed.notified())
        .await
        .expect("#749 next-data hello entered");
    let request = json!({"type":"request","id":"tail","method":"describe_server"}).to_string();
    let wire = [
        frame::client_frame(0x88, 0, b""),
        frame::client_frame(0x81, request.len() as u8, request.as_bytes()),
    ]
    .concat();
    client.write_all(&wire).await.expect("#749 next-data batch");
    // The entire batch fits in the duplex read buffer. Writer EOF is the
    // deterministic barrier proving its close frame has been processed.
    let mut replies = vec![];
    timeout(DEADLINE, client.read_to_end(&mut replies))
        .await
        .expect("#749 next-data close deadline")
        .expect("#749 next-data close read");
    negative_assert_eq!(
        requests.calls.load(Ordering::SeqCst),
        1 + oracle()["opcodeCloseTailCalls"]
            .as_u64()
            .expect("#749 first-data oracle") as usize,
        "#749 tail waits for next data"
    );
    crate::test_negative_receipt::expect_ok(
        client.write_all(&frame::client_frame(0x88, 0, b"")).await,
        "#749 next data resumes buffered tail",
    );
    crate::test_negative_receipt::expect_ok(
        timeout(DEADLINE, requests.changed.notified()).await,
        "#749 next data resumes buffered tail",
    );
    client.shutdown().await.expect("#749 next-data FIN");
    timeout(DEADLINE, task)
        .await
        .expect("#749 next-data task deadline")
        .expect("#749 next-data task join")
        .expect("#749 next-data transport finish");
    negative_assert_eq!(
        requests.calls.load(Ordering::SeqCst),
        1 + oracle()["opcodeCloseNextDataTailCalls"]
            .as_u64()
            .expect("#749 next-data oracle") as usize,
        "#749 next data resumes buffered tail"
    );
    host.shutdown(DEADLINE)
        .await
        .expect("#749 next-data host stop");
}
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn regenerates_identically() {
    crate::test_baseline::regenerates_identically("native-ws-entry");
}

async fn closed_batch(wire: Vec<u8>) -> usize {
    let requests = Arc::new(Requests::default());
    let host = native_ws_fixture(requests.clone());
    let (server, mut client) = tokio::io::duplex(4096);
    let task = tokio::spawn(transport(
        server,
        host.clone(),
        "fixture-token".into(),
        "probe".into(),
    ));
    let hello = hello("fixture-token").to_string();
    client
        .write_all(&frame::client_frame(
            0x81,
            hello.len() as u8,
            hello.as_bytes(),
        ))
        .await
        .expect("#749 batch hello write");
    timeout(DEADLINE, requests.changed.notified())
        .await
        .expect("#749 batch hello entered");
    // Each complete batch is queued atomically before the transport is polled.
    client.write_all(&wire).await.expect("#749 batch write");
    let mut replies = vec![];
    timeout(DEADLINE, client.read_to_end(&mut replies))
        .await
        .expect("#749 batch close deadline")
        .expect("#749 batch close read");
    client.shutdown().await.expect("#749 batch FIN");
    timeout(DEADLINE, task)
        .await
        .expect("#749 batch task deadline")
        .expect("#749 batch join")
        .expect("#749 batch finish");
    host.shutdown(DEADLINE).await.expect("#749 batch host stop");
    requests.calls.load(Ordering::SeqCst) - 1
}
#[tokio::test]
async fn close_discards_following_frames() {
    let request = json!({"type":"request","id":"late","method":"describe_server"}).to_string();
    let wire = [
        frame::client_frame(0x89, 1, b"x"),
        frame::client_frame(0x88, 0, b""),
        frame::client_frame(0x81, request.len() as u8, request.as_bytes()),
    ]
    .concat();
    negative_assert_eq!(
        json!(closed_batch(wire).await),
        oracle()["opcodeCloseTailCalls"],
        "#749 close discards following requests"
    );
}
#[tokio::test]
async fn read_continues_under_writer_backpressure() {
    let requests = Arc::new(Requests::default());
    requests.large.store(true, Ordering::SeqCst);
    let host = native_ws_fixture(requests.clone());
    let (server, mut client) = tokio::io::duplex(64);
    let worker_host = host.clone();
    let task = tokio::spawn(transport(
        server,
        worker_host,
        "fixture-token".into(),
        "probe".into(),
    ));
    let wire = hello("fixture-token").to_string();
    client
        .write_all(&frame::client_frame(
            0x81,
            wire.len() as u8,
            wire.as_bytes(),
        ))
        .await
        .expect("#749 backpressure hello");
    timeout(DEADLINE, requests.changed.notified())
        .await
        .expect("#749 first request admitted");
    // 64-byte transport cannot fit hello_ok. No response bytes are consumed.
    let wire = json!({"type":"request","id":"next","method":"describe_server"}).to_string();
    let next = timeout(DEADLINE, async {
        client
            .write_all(&frame::client_frame(
                0x81,
                wire.len() as u8,
                wire.as_bytes(),
            ))
            .await
            .expect("#749 request under backpressure");
        requests.changed.notified().await;
    })
    .await;
    crate::test_negative_receipt::expect_ok(next, "#749 read under write backlog");
    negative_assert_eq!(
        requests.calls.load(Ordering::SeqCst),
        2,
        "#749 second request reached host"
    );
    client.shutdown().await.expect("#749 backpressure EOF");
    let mut output = vec![];
    timeout(DEADLINE, client.read_to_end(&mut output))
        .await
        .expect("#749 drain output deadline")
        .expect("#749 drain output");
    timeout(DEADLINE, task)
        .await
        .expect("#749 writer task deadline")
        .expect("#749 writer task join")
        .expect("#749 writer completed");
    host.shutdown(DEADLINE)
        .await
        .expect("#749 backpressure host stopped");
}
#[tokio::test]
async fn cancelled_server_stops_retained_host() {
    let requests = Arc::new(Requests::default());
    let host = native_ws_fixture(requests);
    let server = NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#749 cancel address"),
        host.clone(),
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#749 cancel bind");
    let address = server.local_addr().expect("#749 cancel assigned port");
    let task = tokio::spawn(server.run(std::future::pending(), Duration::from_secs(1)));
    let mut client = Client::connect(address).await;
    client.hello().await;
    client.json().await;
    task.abort();
    assert!(
        task.await.expect_err("#749 aborted task").is_cancelled(),
        "#749 actual cancellation"
    );
    crate::test_negative_receipt::expect_ok(
        timeout(DEADLINE, host.stopped()).await,
        "#749 cancelled server signals retained host",
    )
    .expect("#749 cancelled server cleans host");
}
#[tokio::test]
async fn native_configuration() {
    let host = native_ws_fixture(Arc::new(Requests::default()));
    for (address, token) in [
        ("0.0.0.0:0", "token"),
        ("127.0.0.1:80", "token"),
        ("127.0.0.1:0", ""),
    ] {
        let result = NativeWsServer::bind(
            address.parse().expect("#749 rejected address"),
            host.clone(),
            token.into(),
            "probe".into(),
        )
        .await;
        negative_assert!(
            result.is_err(),
            "#749 native configuration refuses implicit authority"
        );
    }
    host.shutdown(DEADLINE)
        .await
        .expect("#749 configuration host stop");
}

#[tokio::test]
async fn unpolled_server_stops_retained_host() {
    let host = native_ws_fixture(Arc::new(Requests::default()));
    let server = NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#749 unpolled address"),
        host.clone(),
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#749 unpolled bind");
    drop(server.run(std::future::pending(), Duration::from_secs(1)));
    crate::test_negative_receipt::expect_ok(
        timeout(DEADLINE, host.stopped()).await,
        "#749 unpolled server signals retained host",
    )
    .expect("#749 unpolled host cleanup");
}
struct CleanupDrop(Option<oneshot::Sender<()>>);
impl Drop for CleanupDrop {
    fn drop(&mut self) {
        if let Some(signal) = self.0.take() {
            let _ = signal.send(());
        }
    }
}
#[tokio::test]
async fn cancelled_last_host_keeps_deadline() {
    for polled in [false, true] {
        let (send, dropped) = oneshot::channel();
        let cleanup = CleanupDrop(Some(send));
        let host = crate::runtime::turn::gateway::tests::native_ws_fixture_with_cleanup(
            Arc::new(Requests::default()),
            async move {
                let _cleanup = cleanup;
                std::future::pending().await
            },
        );
        let server = NativeWsServer::bind(
            "127.0.0.1:0".parse().expect("#749 last-host address"),
            host,
            "fixture-token".into(),
            "probe".into(),
        )
        .await
        .expect("#749 last-host bind");
        let address = server.local_addr().expect("#749 last-host port");
        let running = server.run(std::future::pending(), Duration::from_millis(25));
        if polled {
            let task = tokio::spawn(running);
            let mut client = Client::connect(address).await;
            client.hello().await;
            client.json().await;
            task.abort();
            assert!(
                task.await
                    .expect_err("#749 last-host aborted")
                    .is_cancelled(),
                "#749 last-host cancellation"
            );
        } else {
            drop(running);
        }
        crate::test_negative_receipt::expect_ok(
            timeout(DEADLINE, dropped).await,
            "#749 last host retains explicit deadline",
        )
        .expect("#749 cleanup drop received");
    }
}
struct DropIo {
    io: tokio::io::DuplexStream,
    dropped: Option<oneshot::Sender<()>>,
}
impl Drop for DropIo {
    fn drop(&mut self) {
        if let Some(send) = self.dropped.take() {
            let _ = send.send(());
        }
    }
}
impl tokio::io::AsyncRead for DropIo {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl tokio::io::AsyncWrite for DropIo {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
    }
}
#[tokio::test]
async fn socket_drops_before_business_drains() {
    let requests = Arc::new(Requests::default());
    requests.hold.store(true, Ordering::SeqCst);
    let host = native_ws_fixture(requests.clone());
    let (io, mut client) = tokio::io::duplex(4096);
    let (send, dropped) = oneshot::channel();
    let task = tokio::spawn(transport(
        DropIo {
            io,
            dropped: Some(send),
        },
        host.clone(),
        "fixture-token".into(),
        "probe".into(),
    ));
    let hello = hello("fixture-token").to_string();
    client
        .write_all(&frame::client_frame(
            0x81,
            hello.len() as u8,
            hello.as_bytes(),
        ))
        .await
        .expect("#749 retained hello");
    timeout(DEADLINE, requests.entered.notified())
        .await
        .expect("#749 retained request entered");
    client
        .write_all(&frame::client_frame(0x88, 0, b""))
        .await
        .expect("#749 retained close");
    client.shutdown().await.expect("#749 retained close FIN");
    crate::test_negative_receipt::expect_ok(
        timeout(DEADLINE, dropped).await,
        "#749 closed socket released before business drain",
    )
    .expect("#749 socket drop signal");
    assert!(!task.is_finished(), "#749 business still pending");
    requests.release.notify_one();
    timeout(DEADLINE, task)
        .await
        .expect("#749 retained task deadline")
        .expect("#749 retained join")
        .expect("#749 retained finish");
    host.shutdown(DEADLINE)
        .await
        .expect("#749 retained host stop");
}

#[tokio::test]
async fn application_close_preserves_same_batch_callbacks() {
    let request = json!({"type":"request","id":"tail","method":"describe_server"}).to_string();
    let wire = [
        frame::client_frame(0x81, 1, b"{"),
        frame::client_frame(0x81, request.len() as u8, request.as_bytes()),
    ]
    .concat();
    negative_assert_eq!(
        json!(closed_batch(wire).await),
        oracle()["applicationCloseTailCalls"],
        "#749 application close same batch matches TS"
    );
}

#[tokio::test]
async fn host_cleanup_precedes_runtime_exit() {
    let (entered, ready) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let host = crate::runtime::turn::gateway::tests::native_ws_fixture_with_cleanup(
        Arc::new(Requests::default()),
        async move {
            entered.send(()).expect("#749 cleanup entered signal");
            released.await.expect("#749 cleanup release signal");
            Ok(())
        },
    );
    let running = host.clone();
    let task = tokio::spawn(async move { running.shutdown(DEADLINE).await });
    let entered = crate::test_negative_receipt::expect_ok(
        timeout(DEADLINE, ready).await,
        "#749 owned cleanup called",
    );
    crate::test_negative_receipt::expect_ok(entered, "#749 owned cleanup called");
    negative_assert!(
        !task.is_finished(),
        "#749 cleanup completes before runtime exit"
    );
    release.send(()).expect("#749 release cleanup");
    timeout(DEADLINE, task)
        .await
        .expect("#749 cleanup deadline")
        .expect("#749 cleanup join")
        .expect("#749 cleanup complete");
}
#[tokio::test]
async fn host_cleanup_is_bounded() {
    let host = crate::runtime::turn::gateway::tests::native_ws_fixture_with_cleanup(
        Arc::new(Requests::default()),
        std::future::pending(),
    );
    let result = timeout(DEADLINE, host.shutdown(Duration::from_millis(25)))
        .await
        .expect("#749 cleanup outer deadline");
    negative_assert_eq!(
        result,
        Err("gateway host drain deadline exceeded".to_owned()),
        "#749 cleanup timeout is failure"
    );
}
#[tokio::test]
async fn host_cleanup_runs_after_session_failure() {
    let (cleaned, cleanup) = oneshot::channel();
    let host = crate::runtime::turn::gateway::tests::native_ws_fixture_with_disposal(
        Arc::new(Requests::default()),
        async move {
            cleaned.send(()).expect("#749 failure cleanup signal");
            Ok(())
        },
        true,
    );
    let server = NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#749 failure address"),
        host.clone(),
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#749 failure bind");
    let address = server.local_addr().expect("#749 failure local address");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        DEADLINE,
    ));
    let mut client = Client::connect(address).await;
    client.hello().await;
    client.json().await;
    client.send(json!({"type":"request","id":"turn","method":"submit_turn","params":{"sessionKey":"s","runId":"r","message":"hello","channelKey":"test"}})).await;
    assert_eq!(
        client.json().await["event"]["type"],
        "turn_started",
        "#749 failure live session exists"
    );
    client.close().await;
    stop.send(()).expect("#749 failure stop");
    let result = timeout(DEADLINE, task)
        .await
        .expect("#749 failure drain deadline")
        .expect("#749 failure task join");
    assert!(
        result
            .expect_err("#749 session error must propagate")
            .to_string()
            .contains("Failed to shut down all agent sessions"),
        "#749 session shutdown error preserved"
    );
    crate::test_negative_receipt::expect_ok(
        cleanup.await,
        "#749 cleanup runs after session failure",
    );
}
