use super::*;
use crate::{
    negative_assert, negative_assert_eq,
    runtime::{
        connection::{Call, RequestError},
        turn::gateway::{GatewayRequests, tests::native_ws_fixture},
    },
    test_native_ws_sample::RFC6455_SAMPLE_NONCE,
};
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
    hold_all: AtomicBool,
    calls: AtomicUsize,
    changed: Notify,
    entered: Notify,
    release: Notify,
    after_entered: Option<Arc<Semaphore>>,
}
impl GatewayRequests for Requests {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.changed.notify_one();
            if self.hold_all.load(Ordering::SeqCst) || self.hold.swap(false, Ordering::SeqCst) {
                // Register before publishing entered: notify_waiters keeps no
                // permit for a Notified created after the release broadcast.
                let released = self.release.notified();
                tokio::pin!(released);
                released.as_mut().enable();
                self.entered.notify_one();
                if let Some(gate) = &self.after_entered {
                    gate.acquire()
                        .await
                        .expect("#1123 after-entered gate stays open")
                        .forget();
                }
                released.await;
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

// Poll only at explicit events, so the release-before-await window does not
// depend on scheduling, wall-clock delays, or a timeout to detect a lost wakeup.
fn poll_request<F: Future + ?Sized>(request: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
    request.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
}

#[test]
fn requests_release_before_await_reaches_all_waiters() {
    let gate = Arc::new(Semaphore::new(0));
    let requests = Requests {
        hold_all: AtomicBool::new(true),
        after_entered: Some(gate.clone()),
        ..Requests::default()
    };
    let mut first = requests.describe_server();
    let mut second = requests.describe_server();
    negative_assert!(
        poll_request(first.as_mut()).is_pending() && poll_request(second.as_mut()).is_pending(),
        "#1123 both requests stop at after-entered gate"
    );
    requests.release.notify_waiters(); // #1123 broadcast before either release await
    gate.add_permits(2);
    negative_assert_eq!(
        gate.available_permits(),
        0,
        "#1123 both requests registered at after-entered gate"
    );
    negative_assert!(
        matches!(poll_request(first.as_mut()), std::task::Poll::Ready(Ok(_)))
            && matches!(poll_request(second.as_mut()), std::task::Poll::Ready(Ok(_))),
        "#1123 release before await reaches every entered request"
    );
}

#[test]
fn requests_release_after_await_reaches_all_waiters() {
    let requests = Requests {
        hold_all: AtomicBool::new(true),
        ..Requests::default()
    };
    let mut first = requests.describe_server();
    let mut second = requests.describe_server();
    negative_assert!(
        poll_request(first.as_mut()).is_pending() && poll_request(second.as_mut()).is_pending(),
        "#1123 both requests await release"
    );
    requests.release.notify_waiters();
    negative_assert!(
        matches!(poll_request(first.as_mut()), std::task::Poll::Ready(Ok(_)))
            && matches!(poll_request(second.as_mut()), std::task::Poll::Ready(Ok(_))),
        "#1123 release after await reaches every entered request"
    );
}

#[test]
fn requests_unreleased_or_cancelled_predecessor_does_not_block() {
    let requests = Requests {
        hold: AtomicBool::new(true),
        ..Requests::default()
    };
    let mut predecessor = requests.describe_server();
    negative_assert!(
        poll_request(predecessor.as_mut()).is_pending(),
        "#1123 predecessor remains held without release"
    );
    let mut unheld = requests.describe_server();
    negative_assert!(
        matches!(poll_request(unheld.as_mut()), std::task::Poll::Ready(Ok(_))),
        "#1123 unheld successor bypasses unreleased predecessor"
    );
    drop(predecessor);
    requests.hold.store(true, Ordering::SeqCst);
    let mut successor = requests.describe_server();
    negative_assert!(
        poll_request(successor.as_mut()).is_pending(),
        "#1123 held successor waits after predecessor cancellation"
    );
    requests.release.notify_waiters();
    negative_assert!(
        matches!(
            poll_request(successor.as_mut()),
            std::task::Poll::Ready(Ok(_))
        ),
        "#1123 cancelled predecessor leaves successor releasable"
    );
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
        } else if payload.len() <= u16::MAX as usize {
            126
        } else {
            127
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
    let mut observed =
        serde_json::from_str::<Value>(ORACLE).expect("#749 oracle JSON")["observations"].clone();
    // Mutate the parsed real fixture, before the named test inspects it. These
    // switches prove that missing rows/fields and wrong content cannot pass.
    if crate::mutants::on("native-ws-large-oracle-empty") {
        observed["resources"]["largeOutput"] = json!([]);
    }
    if crate::mutants::on("native-ws-large-oracle-missing") {
        observed["resources"]["largeOutput"]
            .as_array_mut()
            .expect("#749 large oracle array")
            .pop();
    }
    if crate::mutants::on("native-ws-large-oracle-size") {
        observed["resources"]["largeOutput"][0]["size"] = json!(1);
    }
    if crate::mutants::on("native-ws-large-oracle-content") {
        observed["resources"]["largeOutput"][1]["sha256"] = json!("wrong-content");
    }
    if crate::mutants::on("native-ws-full-oracle-before-count") {
        observed["resources"]["fullQueueEnd"]["before"]["completed"] = json!(0);
    }
    if crate::mutants::on("native-ws-full-oracle-before-finished") {
        observed["resources"]["fullQueueEnd"]["before"]["finished"] = json!(true);
    }
    if crate::mutants::on("native-ws-full-oracle-completed") {
        observed["resources"]["fullQueueEnd"]["completed"] = json!(32);
    }
    if crate::mutants::on("native-ws-full-oracle-finished") {
        observed["resources"]["fullQueueEnd"]["finished"] = json!(false);
    }
    if crate::mutants::on("native-ws-full-oracle-destroyed") {
        observed["resources"]["fullQueueEnd"]["destroyed"] = json!(false);
    }
    if crate::mutants::on("native-ws-close-oracle-missing") {
        observed["resources"]["closeFlush"] = json!([]);
    }
    observed
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
    let result = crate::test_negative_receipt::expect_ok(
        timeout(DEADLINE, task).await,
        "#749 bounded host exit",
    )
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
    let result = crate::test_negative_receipt::expect_ok(
        timeout(DEADLINE, task).await,
        "#749 half handshake bounded",
    )
    .expect("#749 half handshake join");
    let mut tail = Vec::new();
    crate::test_negative_receipt::expect_ok(
        timeout(DEADLINE, socket.read_to_end(&mut tail)).await,
        "#749 half handshake reclaimed",
    )
    .expect("#749 half handshake EOF");
    negative_assert!(result.is_err(), "#749 unfinished handshake drain is error");
}

#[tokio::test]
async fn idle_socket_drain_reports_deadline() {
    let (address, stop, task) = start(Arc::default(), Duration::from_millis(50)).await;
    let mut client = Client::connect(address).await;
    client.hello().await;
    client.json().await;
    stop.send(()).expect("#749 idle drain stop");
    let result = crate::test_negative_receipt::expect_ok(
        timeout(DEADLINE, task).await,
        "#749 idle drain bounded",
    )
    .expect("#749 idle drain join");
    negative_assert_eq!(
        result.err().map(|e| e.to_string()),
        Some("native WS drain deadline exceeded".into()),
        "#749 idle drain cannot claim success"
    );
}

#[tokio::test]
async fn oversized_header_closes_before_payload() {
    for authenticated in [false, true] {
        let (address, stop, task) = start(Arc::default(), DEADLINE).await;
        let mut client = Client::connect(address).await;
        if authenticated {
            client.hello().await;
            client.json().await;
        }
        let mut header = vec![0x81, 0xff];
        header.extend_from_slice(&(MAX_INPUT_PAYLOAD as u64 + 1).to_be_bytes());
        header.extend_from_slice(&[1, 2, 3, 4]);
        client
            .socket
            .write_all(&header)
            .await
            .expect("#749 oversize header send");
        // No payload is ever supplied: only the header can trigger rejection.
        let result = crate::test_negative_receipt::expect_ok(
            timeout(Duration::from_secs(2), client.read()).await,
            "#749 input limit rejects header before allocation",
        );
        negative_assert_eq!(
            result,
            (
                8,
                [1009u16.to_be_bytes().as_slice(), b"frame too large"].concat()
            ),
            "#749 input limit close code"
        );
        drop(client);
        finish(stop, task).await;
    }
}

#[test]
fn input_limit_boundary_preserves_proxy() {
    let mut decoder = Decoder::default();
    decoder.buffer.extend_from_slice(&[0x81, 0xff]);
    decoder
        .buffer
        .extend_from_slice(&(MAX_INPUT_PAYLOAD as u64).to_be_bytes());
    decoder.buffer.extend_from_slice(&[1, 2, 3, 4]);
    assert!(
        crate::test_negative_receipt::expect_ok(
            decoder.next_limited(MAX_INPUT_PAYLOAD as u64),
            "#749 exact input boundary remains accepted"
        )
        .is_none(),
        "#749 exact input limit waits for payload"
    );
    assert!(
        decoder
            .next()
            .expect("#749 proxy boundary decode")
            .is_none(),
        "#749 proxy default unchanged"
    );
    decoder.buffer.clear();
    decoder
        .buffer
        .extend_from_slice(&frame::client_frame(0x81, 127, b"x"));
    assert_eq!(
        decoder
            .next_limited(MAX_INPUT_PAYLOAD as u64)
            .expect("#749 nonminimal decode")
            .expect("#749 nonminimal complete frame")
            .payload,
        b"x",
        "#749 nonminimal lengths preserved"
    );
}

#[tokio::test]
async fn output_byte_budget_includes_inflight_and_recovers() {
    let (text, mut outgoing, _) = Text::channel();
    text.send_text("x".repeat(MAX_OUTPUT_PAYLOAD));
    let held = crate::test_negative_receipt::expect_ok(
        outgoing.try_recv(),
        "#749 byte limit includes blocked writer",
    );
    negative_assert!(
        matches!(held, Output::Frame(..)),
        "#749 byte limit includes blocked writer"
    );
    negative_assert!(
        text.closing.borrow().is_none(),
        "#749 byte limit includes blocked writer"
    );
    text.send_text("extra".into());
    negative_assert!(
        text.closing.borrow().is_some(),
        "#749 byte limit includes blocked writer"
    );
    negative_assert!(
        matches!(outgoing.try_recv(), Ok(Output::Close(..))),
        "#749 byte limit includes blocked writer"
    );
    drop(held);
    let (text, mut outgoing, _) = Text::channel();
    for _ in 0..2 {
        text.send_text("x".repeat(MAX_OUTPUT_PAYLOAD));
        let item = crate::test_negative_receipt::expect_ok(
            outgoing.try_recv(),
            "#749 byte limit includes blocked writer",
        );
        negative_assert!(
            matches!(item, Output::Frame(..)),
            "#749 byte limit includes blocked writer"
        );
        negative_assert!(
            text.closing.borrow().is_none(),
            "#749 byte limit includes blocked writer"
        );
        drop(item);
    }
}

#[tokio::test]
async fn output_slot_budget_and_full_close() {
    for close in [false, true] {
        let (text, mut outgoing, _) = Text::channel();
        for _ in 0..MAX_OUTPUT_FRAMES {
            text.send_text("x".into());
        }
        negative_assert!(
            text.closing.borrow().is_none(),
            "#749 full output queue preserves close frame"
        );
        if close {
            text.close(1000, "");
        } else {
            text.send_text("overflow".into());
        }
        negative_assert!(
            text.closing.borrow().is_some(),
            "#749 full output queue preserves close frame"
        );
        for _ in 0..MAX_OUTPUT_FRAMES {
            negative_assert!(
                matches!(outgoing.try_recv(), Ok(Output::Frame(..))),
                "#749 full output queue preserves close frame"
            );
        }
        let last = crate::test_negative_receipt::expect_ok(
            outgoing.try_recv(),
            "#749 full output queue preserves close frame",
        );
        let Output::Close(wire) = last else {
            negative_assert!(false, "#749 full output queue preserves close frame");
            return;
        };
        negative_assert_eq!(
            wire.get(2..4),
            Some(
                if close { 1000u16 } else { 1009u16 }
                    .to_be_bytes()
                    .as_slice()
            ),
            "#749 full output queue preserves close frame"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn writer_deadline_releases_socket_before_stuck_business() {
    for stall_shutdown in [false, true] {
        let requests = Arc::new(Requests::default());
        requests.hold.store(true, Ordering::SeqCst);
        let host = native_ws_fixture(requests.clone());
        let (io, mut client) = tokio::io::duplex(256);
        let (send, dropped) = oneshot::channel();
        let task = tokio::spawn(transport(
            DropIo {
                io,
                dropped: Some(send),
                stall_shutdown,
            },
            host.clone(),
            "fixture-token".into(),
            "probe".into(),
        ));
        let message = hello("fixture-token").to_string();
        client
            .write_all(&frame::client_frame(
                0x81,
                message.len() as u8,
                message.as_bytes(),
            ))
            .await
            .expect("#749 stuck writer hello");
        manual_clock(timeout(DEADLINE, requests.entered.notified()))
            .await
            .expect("#749 stuck business entered");
        if !stall_shutdown {
            // A pong larger than the duplex capacity blocks write_all while
            // the admitted hello is still held on the independent host.
            client
                .write_all(&frame::client_frame(0x89, 126, &[b'x'; 4096]))
                .await
                .expect("#749 fill writer without reading");
        }
        client
            .shutdown()
            .await
            .expect("#749 writer peer half-close");
        crate::test_negative_receipt::expect_ok(
            timeout(CLOSE_BUDGET + Duration::from_secs(105), dropped).await,
            "#749 writer deadline releases socket despite held business",
        )
        .expect("#749 writer socket drop signal");
        assert!(
            !task.is_finished(),
            "#749 held business still pending after socket reclaimed"
        );
        requests.release.notify_one();
        let result = manual_clock(timeout(DEADLINE, task))
            .await
            .expect("#749 writer deadline task bound")
            .expect("#749 writer deadline task join");
        assert!(result.is_err(), "#749 writer expiry remains an error");
        manual_clock(host.shutdown(DEADLINE))
            .await
            .expect("#749 writer deadline host stopped");
    }
}

#[tokio::test(start_paused = true)]
async fn close_deadline_reclaims_peer_without_fin() {
    let host = native_ws_fixture(Arc::new(Requests::default()));
    let (io, mut client) = tokio::io::duplex(256);
    let (send, dropped) = oneshot::channel();
    let task = tokio::spawn(transport(
        DropIo {
            io,
            dropped: Some(send),
            stall_shutdown: false,
        },
        host.clone(),
        "fixture-token".into(),
        "probe".into(),
    ));
    client
        .write_all(&frame::client_frame(0x88, 0, b""))
        .await
        .expect("#749 close without FIN");
    let mut close = [0; 4];
    client
        .read_exact(&mut close)
        .await
        .expect("#749 close reply before deadline");
    assert_eq!(close, [0x88, 2, 3, 232], "#749 close reply matches TS");
    crate::test_negative_receipt::expect_ok(
        timeout(CLOSE_BUDGET + Duration::from_secs(105), dropped).await,
        "#749 reader close deadline reclaims peer without FIN",
    )
    .expect("#749 close drop signal");
    let result = manual_clock(timeout(DEADLINE, task))
        .await
        .expect("#749 close deadline task bound")
        .expect("#749 close task join");
    assert!(result.is_err(), "#749 reader expiry remains error");
    manual_clock(host.shutdown(DEADLINE))
        .await
        .expect("#749 close host stopped");
}

#[tokio::test]
async fn pending_requests_are_bounded_before_enqueue() {
    let gate = Arc::new(Semaphore::new(0));
    let requests = Arc::new(Requests {
        after_entered: Some(gate.clone()),
        ..Requests::default()
    });
    let host = native_ws_fixture(requests.clone());
    let (io, mut client) = tokio::io::duplex(4096);
    let (send, dropped) = oneshot::channel();
    let task = tokio::spawn(transport(
        DropIo {
            io,
            dropped: Some(send),
            stall_shutdown: false,
        },
        host.clone(),
        "fixture-token".into(),
        "probe".into(),
    ));
    let message = hello("fixture-token").to_string();
    client
        .write_all(&frame::client_frame(
            0x81,
            message.len() as u8,
            message.as_bytes(),
        ))
        .await
        .expect("#1123 pending cap hello");
    // Consume the complete hello before holding subsequent requests.
    let mut response = [0; 4096];
    assert!(
        client
            .read(&mut response)
            .await
            .expect("#1123 pending cap hello reply")
            > 0,
        "#1123 pending hello bytes"
    );
    requests.hold_all.store(true, Ordering::SeqCst);
    let message = json!({"type":"request","id":"held","method":"describe_server"}).to_string();
    let wire = frame::client_frame(0x81, message.len() as u8, message.as_bytes());
    for _ in 0..MAX_PENDING_REQUESTS {
        client
            .write_all(&wire)
            .await
            .expect("#1123 pending cap held request");
        timeout(DEADLINE, requests.entered.notified())
            .await
            .expect("#1123 pending cap request entered");
    }
    client
        .write_all(&wire)
        .await
        .expect("#1123 pending cap overflow request");
    crate::test_negative_receipt::expect_ok(
        timeout(Duration::from_secs(3), dropped).await,
        "#1123 pending cap rejects before host enqueue",
    )
    .expect("#1123 pending socket reclaimed");
    negative_assert_eq!(
        requests.calls.load(Ordering::SeqCst),
        MAX_PENDING_REQUESTS + 1,
        "#1123 pending cap rejects before host enqueue"
    );
    requests.hold_all.store(false, Ordering::SeqCst);
    requests.release.notify_waiters();
    gate.add_permits(MAX_PENDING_REQUESTS);
    let result = timeout(DEADLINE, task)
        .await
        .expect("#1123 pending task bound")
        .expect("#1123 pending task join");
    assert!(result.is_err(), "#1123 pending overload error");
    host.shutdown(DEADLINE)
        .await
        .expect("#1123 pending host stopped");
}
#[test]
fn default_mode_unchanged() {
    use sha2::{Digest, Sha256};
    // #1071 explicitly adds this opt-in before the existing A4 entry. Keep
    // hashing every legacy byte; the real executable test checks both paths.
    const OPT_IN: &str = "    let mut args = std::env::args_os().skip(1);\n    if pilotdeck_gateway::native_process::native_requested(args.next().as_deref()) {\n        return pilotdeck_gateway::native_process::run(\n            pilotdeck_gateway::native_process::Options::parse(args)?,\n        )\n        .await;\n    }\n";
    let source = include_str!("../main.rs");
    negative_assert_eq!(
        source.matches(OPT_IN).count(),
        1,
        "#1071 exact explicit opt-in branch"
    );
    negative_assert_eq!(
        format!("{:x}", Sha256::digest(source.replacen(OPT_IN, "", 1))),
        "398167116815fdca70188a7c8e9230ba87010d6db11a0fae42a82a2bb22f5823",
        "#749 A4 executable remains unchanged"
    );
}
// Keep the paused runtime runnable while an independent host thread reaches
// an event barrier. Virtual time moves only through explicit advance(). The
// wall guard diagnoses a stuck test; it is never a negative-control receipt.
async fn manual_clock<F: Future>(future: F) -> F::Output {
    tokio::pin!(future);
    let wall = std::time::Instant::now();
    loop {
        tokio::select! {
            biased;
            result = &mut future => return result,
            _ = tokio::task::yield_now() => {}
        }
        assert!(
            wall.elapsed() < Duration::from_secs(60),
            "#749 manual-clock harness stalled"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn close_tail_resumes_on_next_data() {
    manual_clock(async {
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
        // A real second of CPU scheduling must not expire this paused clock.
        tokio::time::advance(Duration::from_secs(2)).await;
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
    })
    .await;
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
    stall_shutdown: bool,
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
        if self.stall_shutdown {
            std::task::Poll::Pending
        } else {
            std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
        }
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
            stall_shutdown: false,
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

#[test]
fn buffered_input_never_exceeds_frame_budget() {
    for (buffered, expected) in [
        (0, 8192),
        (MAX_INPUT_PAYLOAD + frame::MAX_CLIENT_HEADER_BYTES - 1, 1),
        (MAX_INPUT_PAYLOAD + frame::MAX_CLIENT_HEADER_BYTES, 0),
        (usize::MAX, 0),
    ] {
        negative_assert_eq!(
            input_capacity(buffered, 8192),
            expected,
            "#749 buffered input capped before read allocation"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn close_deadline_is_not_extended_by_eof() {
    let (text, _outgoing, _) = Text::channel();
    text.close(1000, "");
    let first = *text.closing.borrow();
    tokio::time::advance(CLOSE_BUDGET / 2).await;
    text.end();
    negative_assert_eq!(
        *text.closing.borrow(),
        first,
        "#749 EOF preserves first close deadline"
    );
    text.deadline(true);
    let overloaded = *text.closing.borrow();
    tokio::time::advance(Duration::from_millis(1)).await;
    text.deadline(true);
    negative_assert_eq!(
        *text.closing.borrow(),
        overloaded,
        "#749 EOF preserves first close deadline"
    );
}

#[test]
fn resource_baseline_records_actual_ts_transport() {
    let expected = oracle()["resources"].clone();
    for size in [
        MAX_INPUT_PAYLOAD as u64,
        MAX_INPUT_PAYLOAD as u64 + 1,
        1 << 40,
    ] {
        assert_eq!(
            expected[size.to_string()],
            json!({"messages":0,"buffered":14,"writes":0,"ends":0}),
            "#749 TS custom decoder waits on large declared lengths"
        );
    }
    assert_eq!(
        expected["writeFalse"],
        json!({"writes":65,"ends":1,"closeCode":1000}),
        "#749 TS ignores socket backpressure"
    );
    assert_eq!(
        expected["halfCloseBlocked"],
        json!({"readEnded":true,"pendingBytes":true,"destroyed":false,"serverClosed":false}),
        "#749 TS half close awaits blocked write"
    );
    assert_eq!(
        expected["halfCloseCancelled"],
        json!({"destroyed":true,"serverClosed":true}),
        "#749 TS cancellation releases blocked write"
    );
    assert_eq!(
        expected["heldRequests"], 65,
        "#749 TS admits 65 held requests"
    );
}

#[tokio::test]
async fn completed_writes_and_requests_release_capacity() {
    let requests = Arc::new(Requests::default());
    requests.large.store(true, Ordering::SeqCst);
    let host = native_ws_fixture(requests);
    let (server, mut client) = tokio::io::duplex(4096);
    let task = tokio::spawn(transport(
        server,
        host.clone(),
        "fixture-token".into(),
        "probe".into(),
    ));
    let mut decoder = Decoder::default();
    for index in 0..(MAX_PENDING_REQUESTS + 8) {
        let message = if index == 0 {
            hello("fixture-token")
        } else {
            json!({"type":"request","id":index.to_string(),"method":"describe_server"})
        }
        .to_string();
        client
            .write_all(&frame::client_frame(
                0x81,
                message.len() as u8,
                message.as_bytes(),
            ))
            .await
            .expect("#749 capacity successor send");
        let received = timeout(DEADLINE, async {
            loop {
                if let Some(frame) = decoder
                    .next_server()
                    .expect("#749 capacity response decode")
                {
                    return frame;
                }
                let count = client
                    .read_buf(&mut decoder.buffer)
                    .await
                    .expect("#749 capacity response read");
                negative_assert!(
                    count > 0,
                    "#749 completed writes and requests release capacity"
                );
            }
        })
        .await;
        let frame = crate::test_negative_receipt::expect_ok(
            received,
            "#749 completed writes and requests release capacity",
        );
        negative_assert_eq!(
            frame.opcode,
            1,
            "#749 completed writes and requests release capacity"
        );
    }
    client.shutdown().await.expect("#749 capacity peer EOF");
    timeout(DEADLINE, task)
        .await
        .expect("#749 capacity task bound")
        .expect("#749 capacity task join")
        .expect("#749 capacity task completed");
    host.shutdown(DEADLINE)
        .await
        .expect("#749 capacity host stopped");
}

#[tokio::test]
async fn large_output_matches_ts() {
    use sha2::{Digest, Sha256};
    let observed = oracle();
    let cases = crate::test_negative_receipt::expect_some(
        observed["resources"]["largeOutput"].as_array(),
        "#749 large output fixture has exactly two required sizes",
    );
    negative_assert_eq!(
        cases.len(),
        2,
        "#749 large output fixture has exactly two required sizes"
    );
    for (expected, size) in cases.iter().zip([2 * 1024 * 1024, MAX_OUTPUT_PAYLOAD]) {
        negative_assert_eq!(
            expected["size"],
            json!(size),
            "#749 large output fixture has exactly two required sizes"
        );
        let (text, mut outgoing, _) = Text::channel();
        text.send_text("x".repeat(size));
        let item = crate::test_negative_receipt::expect_ok(
            outgoing.try_recv(),
            "#749 legal large output matches TS",
        );
        let Output::Frame(wire, _permit) = item else {
            negative_assert!(false, "#749 legal large output matches TS");
            return;
        };
        let mut decoder = Decoder::default();
        decoder.buffer.extend_from_slice(&wire);
        let frame = crate::test_negative_receipt::expect_ok(
            decoder.next_server(),
            "#749 legal large output matches TS",
        );
        negative_assert!(frame.is_some(), "#749 legal large output matches TS");
        let frame = frame.expect("#749 legal large output matches TS");
        negative_assert_eq!(
            json!({"size":frame.payload.len(),"opcode":frame.opcode,
            "declared":frame.payload.len(),"wireBytes":wire.len(),
            "sha256":format!("{:x}", Sha256::digest(&frame.payload)),"ends":0}),
            *expected,
            "#749 legal large output matches TS"
        );
        negative_assert!(
            decoder.buffer.is_empty() && text.closing.borrow().is_none(),
            "#749 legal large output matches TS"
        );
    }
}

#[tokio::test]
async fn oversized_output_sends_close() {
    let (text, mut outgoing, _) = Text::channel();
    text.send_text("x".repeat(MAX_OUTPUT_PAYLOAD + 1));
    let item = crate::test_negative_receipt::expect_ok(
        outgoing.try_recv(),
        "#749 oversized output sends close1009",
    );
    let Output::Close(wire) = item else {
        negative_assert!(false, "#749 oversized output sends close1009");
        return;
    };
    negative_assert_eq!(
        wire,
        frame::server_frame(
            8,
            &[1009u16.to_be_bytes().as_slice(), b"output limit exceeded"].concat()
        ),
        "#749 oversized output sends close1009"
    );
    negative_assert!(
        text.closing.borrow().is_some(),
        "#749 oversized output sends close1009"
    );
    text.send_text("after close".into());
    negative_assert!(
        outgoing.try_recv().is_err(),
        "#749 oversized output sends close1009"
    );
}

#[tokio::test(start_paused = true)]
async fn full_queue_eof_flushes_successfully() {
    let expected = &oracle()["resources"]["fullQueueEnd"];
    negative_assert_eq!(
        expected,
        &json!({"before":{"completed":1,"finished":false},
            "completed":MAX_OUTPUT_FRAMES,"finished":true,"destroyed":true}),
        "#749 full queue EOF fixture lifecycle"
    );
    let (text, outgoing, closing) = Text::channel();
    for _ in 0..MAX_OUTPUT_FRAMES {
        text.send_text("queued".into());
    }
    text.end(); // Writer has never been polled: the queue is exactly full.
    let (writer, mut peer) = tokio::io::duplex(16);
    let writing = write_output(writer, outgoing, closing);
    let reading = async {
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).await.map(|_| bytes)
    };
    let result = timeout(CLOSE_BUDGET + Duration::from_secs(2), async {
        tokio::join!(writing, reading)
    })
    .await;
    let (written, received) =
        crate::test_negative_receipt::expect_ok(result, "#749 full queue EOF flushes successfully");
    crate::test_negative_receipt::expect_ok(written, "#749 full queue EOF flushes successfully");
    let received = crate::test_negative_receipt::expect_ok(
        received,
        "#749 full queue EOF flushes successfully",
    );
    negative_assert_eq!(
        received,
        frame::server_frame(1, b"queued").repeat(MAX_OUTPUT_FRAMES),
        "#749 full queue EOF flushes successfully"
    );
    // Retaining Text (and its Sender) must not keep recv() alive after EOF.
    negative_assert_eq!(
        text.bytes.available_permits(),
        MAX_OUTPUT_BYTES,
        "#749 full queue EOF flushes successfully"
    );
}

#[tokio::test(start_paused = true)]
async fn completed_writer_wins_ready_deadline() {
    let (text, _outgoing, closing) = Text::channel();
    text.deadline(false);
    tokio::time::advance(CLOSE_BUDGET + Duration::from_millis(1)).await;
    crate::test_negative_receipt::expect_ok(
        finish_write(std::future::ready(Ok(())), closing).await,
        "#749 completed writer wins ready deadline",
    );
}

#[tokio::test]
async fn output_permit_released_after_write() {
    let (text, outgoing, closing) = Text::channel();
    text.send_text("reserved".into());
    negative_assert!(
        text.bytes.available_permits() < MAX_OUTPUT_BYTES,
        "#749 written frame releases byte permit"
    );
    text.end();
    crate::test_negative_receipt::expect_ok(
        write_output(tokio::io::sink(), outgoing, closing).await,
        "#749 written frame releases byte permit",
    );
    negative_assert_eq!(
        text.bytes.available_permits(),
        MAX_OUTPUT_BYTES,
        "#749 written frame releases byte permit"
    );
}

#[tokio::test]
async fn buffer_capacity_close_uses_1009() {
    let (text, mut outgoing, _) = Text::channel();
    // Deterministic defensive-state injection into the production admission
    // function; a legal complete frame cannot leave this state after decode.
    negative_assert_eq!(
        read_capacity(
            &text,
            MAX_INPUT_PAYLOAD + frame::MAX_CLIENT_HEADER_BYTES,
            8192
        ),
        0,
        "#749 saturated input admission sends close1009"
    );
    let item = crate::test_negative_receipt::expect_ok(
        outgoing.try_recv(),
        "#749 saturated input admission sends close1009",
    );
    let Output::Close(wire) = item else {
        negative_assert!(false, "#749 saturated input admission sends close1009");
        return;
    };
    negative_assert_eq!(
        wire,
        frame::server_frame(
            8,
            &[1009u16.to_be_bytes().as_slice(), b"frame too large"].concat()
        ),
        "#749 saturated input admission sends close1009"
    );
}

#[test]
fn exact_input_frame_is_consumed_before_next_read() {
    let wire = frame::client_frame(0x81, 127, &vec![b'x'; MAX_INPUT_PAYLOAD]);
    let mut decoder = Decoder::default();
    decoder.buffer.extend_from_slice(&wire);
    assert_eq!(
        input_capacity(decoder.buffer.len(), 8192),
        0,
        "#749 full frame raw buffer at cap"
    );
    let frame = decoder
        .next_limited(MAX_INPUT_PAYLOAD as u64)
        .expect("#749 exact frame decode")
        .expect("#749 exact frame complete");
    assert_eq!(
        frame.payload.len(),
        MAX_INPUT_PAYLOAD,
        "#749 exact frame payload"
    );
    assert!(
        decoder.buffer.is_empty(),
        "#749 exact frame consumed before next read"
    );
    assert_eq!(
        input_capacity(decoder.buffer.len(), 8192),
        8192,
        "#749 exact frame next read has capacity"
    );
    println!("#749 exact MAX frame consumed; buffered=0; next_read_capacity=8192");
}

#[test]
fn frame_header_budgets_match_encoding() {
    let payload = vec![0; 65536];
    negative_assert_eq!(
        frame::server_frame(1, &payload).len() - payload.len(),
        frame::MAX_SERVER_HEADER_BYTES,
        "#749 header budgets cover actual wire encoding"
    );
    negative_assert_eq!(
        frame::client_frame(0x81, 127, &payload).len() - payload.len(),
        frame::MAX_CLIENT_HEADER_BYTES,
        "#749 header budgets cover actual wire encoding"
    );
}

#[tokio::test(start_paused = true)]
async fn close_budget_covers_accepted_bytes() {
    for (size, seconds) in [(0, 30), (2 * 1024 * 1024, 33), (MAX_OUTPUT_PAYLOAD, 131)] {
        let (text, mut outgoing, _) = Text::channel();
        let held = if size == 0 {
            None
        } else {
            text.send_text("x".repeat(size));
            Some(crate::test_negative_receipt::expect_ok(
                outgoing.try_recv(),
                "#749 close budget covers accepted bytes",
            ))
        };
        let start = tokio::time::Instant::now();
        text.close(1000, "");
        negative_assert_eq!(
            *text.closing.borrow(),
            Some(start + Duration::from_secs(seconds)),
            "#749 close budget covers accepted bytes"
        );
        drop(held);
        tokio::time::advance(Duration::from_secs(2)).await;
        text.end();
        negative_assert_eq!(
            *text.closing.borrow(),
            Some(start + Duration::from_secs(seconds)),
            "#749 close budget covers accepted bytes"
        );
    }
}

// A single explicit poll establishes the write predecessor, independent of
// scheduler wake order. This helper never spins or advances the clock.
async fn is_pending<F: Future>(future: std::pin::Pin<&mut F>) -> bool {
    let mut future = future;
    std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx).is_pending())).await
}

#[test]
fn close_flush_fixture_records_ts_lifecycle() {
    use sha2::{Digest, Sha256};
    let mut expected = Vec::new();
    let size = 2 * 1024 * 1024;
    let hash = format!("{:x}", Sha256::digest(vec![b'x'; size]));
    for event in ["close", "eof"] {
        for outcome in ["release", "cancel"] {
            let after = if outcome == "release" {
                json!({"size":size,"opcode":1,"wireBytes":size+10+if event=="close"{4}else{0},
                    "sha256":hash,"tail":if event=="close"{"880203e8"}else{""},"finished":event=="close"})
            } else {
                json!({"destroyed":true,"serverClosed":true})
            };
            expected.push(
                json!({"event":event,"outcome":outcome,"advancedMs":3_600_000,
                "before":{"pendingBytes":true,"finished":false,"destroyed":false,
                    "serverClosed":false,"socketTimeout":0},"after":after}),
            );
        }
    }
    negative_assert_eq!(
        oracle()["resources"]["closeFlush"],
        json!(expected),
        "#749 TS close flush matrix is complete"
    );
}

#[tokio::test(start_paused = true)]
async fn accepted_output_flushes_after_close_or_eof() {
    for close in [true, false] {
        let (text, outgoing, closing) = Text::channel();
        let payload = vec![b'x'; 2 * 1024 * 1024];
        text.send_text(String::from_utf8(payload.clone()).expect("#749 flush ASCII payload"));
        if close {
            text.close(1000, "");
        } else {
            text.end();
        }
        let (writer, mut peer) = tokio::io::duplex(16);
        let writing = write_output(writer, outgoing, closing);
        tokio::pin!(writing);
        negative_assert!(
            is_pending(writing.as_mut()).await,
            "#749 accepted output survives delayed flush"
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        negative_assert!(
            is_pending(writing.as_mut()).await,
            "#749 accepted output survives delayed flush"
        );
        let reading = async {
            let mut bytes = Vec::new();
            peer.read_to_end(&mut bytes).await.map(|_| bytes)
        };
        let (written, read) = tokio::join!(writing, reading);
        crate::test_negative_receipt::expect_ok(
            written,
            "#749 accepted output survives delayed flush",
        );
        let bytes = crate::test_negative_receipt::expect_ok(
            read,
            "#749 accepted output survives delayed flush",
        );
        let mut expected = frame::server_frame(1, &payload);
        if close {
            expected.extend(frame::server_frame(8, &1000u16.to_be_bytes()));
        }
        negative_assert_eq!(
            bytes,
            expected,
            "#749 accepted output survives delayed flush"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn stalled_output_is_bounded_and_cancellable() {
    for close in [true, false] {
        for cancel in [true, false] {
            let (text, outgoing, closing) = Text::channel();
            text.send_text("held".repeat(16));
            if close {
                text.close(1000, "");
            } else {
                text.end();
            }
            let (io, _peer) = tokio::io::duplex(16);
            let (send, mut dropped) = oneshot::channel();
            let writer = DropIo {
                io,
                dropped: Some(send),
                stall_shutdown: false,
            };
            let mut writing = Box::pin(write_output(writer, outgoing, closing));
            negative_assert!(
                is_pending(writing.as_mut()).await,
                "#749 blocked flush releases resources"
            );
            tokio::time::advance(Duration::from_secs(2)).await;
            negative_assert!(
                is_pending(writing.as_mut()).await,
                "#749 blocked flush releases resources"
            );
            if !cancel {
                tokio::time::advance(Duration::from_secs(130)).await;
                // Poll once: a missing deadline is an assertion failure, never
                // an executor timeout or an unbounded await.
                let result =
                    std::future::poll_fn(|cx| std::task::Poll::Ready(writing.as_mut().poll(cx)))
                        .await;
                negative_assert!(
                    matches!(result, std::task::Poll::Ready(Err(ref e))
                    if e.kind()==std::io::ErrorKind::TimedOut),
                    "#749 blocked flush releases resources"
                );
            }
            drop(writing);
            crate::test_negative_receipt::expect_ok(
                dropped.try_recv(),
                "#749 blocked flush releases resources",
            );
            negative_assert_eq!(
                text.bytes.available_permits(),
                MAX_OUTPUT_BYTES,
                "#749 blocked flush releases resources"
            );
        }
    }
}
