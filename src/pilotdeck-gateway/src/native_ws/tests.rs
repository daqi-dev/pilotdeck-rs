use super::*;
use crate::{
    negative_assert, negative_assert_eq,
    runtime::{
        connection::{Call, RequestError},
        turn::gateway::{
            GatewayRequests,
            tests::{native_ws_fixture, native_ws_fixture_with_cleanup},
        },
    },
    test_native_ws_sample::RFC6455_SAMPLE_NONCE,
};
use pilotdeck_protocol::{JsString, JsValue};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::{sync::Notify, time::timeout};
/// #1891: the server's drain budget — the only clock granted to the scripted
/// in-process `GatewayRequests` fixture, and the behavior several drain tests
/// pin, so it keeps the 5 s form.
const DEADLINE: Duration = Duration::from_secs(5);
/// #1891 fix-r1: the client-side reads wait for response *bytes* from the
/// local listener's serving task. That task shares this worker's scheduler
/// with everything else in the suite, so the #978 starvation mechanism (a
/// loaded host delaying a responder past a short deadline) applies here too —
/// "no real sidecar" is not an exemption from scheduling. These are pure
/// anti-hang bounds: no assertion or reply shape changes.
const RESPONSE_LIVENESS: Duration = Duration::from_secs(60);
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
    /// #2391: park the next non-submit `request` body at `release`, publishing
    /// `request_entered` before and `request_done` after — the probe the
    /// disconnect experiment needs (the request body's own completion,
    /// observable without a socket to deliver the response on).
    hold_request: AtomicBool,
    request_entered: Notify,
    request_done: Notify,
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
        Box::pin(async move {
            if self.hold_request.swap(false, Ordering::SeqCst) {
                // Register before publishing entered: notify_waiters keeps no
                // permit for a Notified created after the release broadcast.
                // #2391 fix-r1: this local is `gate`, not `released` — the
                // native-ws-notify `skip-release-wait` anchor pins
                // `released.await;` to exactly the describe_server branch.
                let gate = self.release.notified();
                tokio::pin!(gate);
                gate.as_mut().enable();
                self.request_entered.notify_one();
                gate.await;
                self.request_done.notify_waiters();
            }
            Err(RequestError::new("#749 unexpected method"))
        })
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
        // #1891 fix-r1: first-response read from the listener's serving task
        // (see RESPONSE_LIVENESS) — liveness bound, not a reply oracle.
        timeout(RESPONSE_LIVENESS, async {
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
        // #1891 fix-r1: response bytes from the listener's serving task
        // (see RESPONSE_LIVENESS) — liveness bound, not a reply oracle.
        let result = timeout(RESPONSE_LIVENESS, async {
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
    // #1891 fix-r1: strictly looser than the drain budget granted above, so
    // scheduling decides the join, not this watchdog.
    timeout(RESPONSE_LIVENESS, task)
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
        // #1891 fix-r1: response bytes (see RESPONSE_LIVENESS).
        let reply = crate::test_negative_receipt::expect_ok(
            timeout(RESPONSE_LIVENESS, socket.read_to_end(&mut bytes)).await,
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
/// #1292: Node's TrackHeader span budget applies to upgrade heads exactly
/// like non-upgrade heads (probe `b4-U1.out`: a 17 020-byte `/ws` upgrade
/// head is the same 431 + close as `b4-U2.out`'s `/health` head), so the
/// native listener checks the budget before the upgrade diversion. The
/// boundary cell on the same head shape (tracked 16383) stays a 101, which
/// is what the diversion alone would answer.
#[tokio::test]
async fn upgrade_head_respects_span_budget() {
    let (address, stop, task) = start(Arc::default(), DEADLINE).await;
    // tracked span = 3 ("/ws") + 13 (Host: localhost) + 17 (Connection:
    // Upgrade) + 16 (Upgrade: websocket) + 41 (Sec-WebSocket-Key: nonce)
    // + 3 + n (x-p) = 93 + n; the inclusive 16384 boundary (probe
    // `probe-fixr3-c.out` C1) puts the 431 at exactly n = 16291.
    for (n, upgraded) in [(16291, false), (16290, true)] {
        let mut socket = TcpStream::connect(address)
            .await
            .expect("#1292 budget upgrade connect");
        let head = format!(
            "GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {RFC6455_SAMPLE_NONCE}\r\nx-p: {}\r\n\r\n",
            "a".repeat(n)
        );
        socket
            .write_all(head.as_bytes())
            .await
            .expect("#1292 budget upgrade send");
        let mut header = vec![];
        // #1891 fix-r1: response bytes (see RESPONSE_LIVENESS).
        timeout(RESPONSE_LIVENESS, async {
            while !header.ends_with(b"\r\n\r\n") {
                header.push(
                    socket
                        .read_u8()
                        .await
                        .expect("#1292 budget upgrade response byte"),
                );
            }
        })
        .await
        .expect("#1292 budget upgrade response deadline");
        let line = String::from_utf8_lossy(&header).to_string();
        if upgraded {
            negative_assert!(
                line.starts_with("HTTP/1.1 101"),
                "#1292 the boundary upgrade head (tracked 16383) stays a 101 like TS"
            );
        } else {
            negative_assert!(
                line.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
                "#1292 the over-budget upgrade head is the same 431 as the non-upgrade lane"
            );
            negative_assert!(
                line.contains("Connection: close\r\n"),
                "#1292 the over-budget upgrade head is told to close like the non-upgrade lane"
            );
            let mut rest = vec![];
            let closed = crate::test_negative_receipt::expect_ok(
                timeout(RESPONSE_LIVENESS, socket.read_to_end(&mut rest)).await,
                "#1292 the over-budget upgrade head closes the connection like the non-upgrade lane",
            );
            crate::test_negative_receipt::expect_ok(
                closed,
                "#1292 the over-budget upgrade head closes the connection like the non-upgrade lane",
            );
        }
    }
    finish(stop, task).await;
}
/// #1351: the boundary golden of the 16 KiB span budget, cell-replayed
/// against the native listener from the committed `native-ws-upgrade-budget`
/// fixture (recorded on the TS baseline with Node v22.22.0): upgrade-lane
/// and non-upgrade-lane heads at tracked 16383 (still served) and 16384
/// (Node's canned 431 + close), plus the 16385 and 20 KiB (20480) far-over
/// cells of the 2026-11-06 round's raw-socket probe
/// (`_tmp/20261106-issue-1351/probe.out`: both lanes share the inclusive
/// 16384 boundary and answer the identical canned 431 + close on every
/// over-budget cell), plus `b4-U1.out`'s far-over shape. The
/// replay writes the recorded request bytes and captures with the same
/// 250 ms quiet window the TS recorder used, so the whole head — status
/// line, every header, and the close decision — is pinned, not just the
/// 431's first line.
const BUDGET_ORACLE: &str =
    include_str!("../../tests/fixtures/native-ws-upgrade-budget/oracle.json");
/// The quiet window the TS recorder used: a response is complete after a
/// quarter second of silence while the connection stays open.
const QUIET: Duration = Duration::from_millis(250);
/// #1891: liveness watchdog for the first response byte, not an assertion
/// about the response. This capture loop is the verbatim sibling of the one
/// the #978 fix relaxed in `module_host::tests`, where a host running eight
/// concurrent lib suites starved the serving task past the old deadline
/// (8/48 loaded copies reddened "produced no bytes and stayed open"); the
/// byte oracle and the closed flag are unchanged, and a connection that truly
/// never answers still reds after this margin.
const FIRST_BYTE: Duration = Duration::from_secs(60);

fn budget_oracle() -> Value {
    crate::test_baseline::oracle(BUDGET_ORACLE);
    let mut observed = serde_json::from_str::<Value>(BUDGET_ORACLE).expect("#1351 oracle JSON");
    // Mutate the parsed real fixture, before the named test inspects it —
    // the corruption style of `native-ws-large-oracle-content`: a recorded
    // 431 cell is replaced in memory, so the replay must fail on its own
    // byte comparison.
    if crate::mutants::on("native-ws-upgrade-oracle-431") {
        for cell in observed["cells"]
            .as_array_mut()
            .expect("#1351 oracle cells")
            .iter_mut()
        {
            if cell["id"].as_str() == Some("upgrade-16384") {
                cell["response"] = json!("HTTP/1.1 101 Switching Protocols\r\n\r\n");
            }
        }
    }
    observed
}

/// The same quiet-window capture the TS recorder used, bounded by the test
/// deadline: 250 ms of silence with the connection open ends the capture;
/// EOF or a reset is a close.
async fn budget_capture(socket: &mut TcpStream) -> (String, bool) {
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 8192];
    let start = std::time::Instant::now();
    let mut closed = false;
    while start.elapsed() < FIRST_BYTE {
        let wait = if out.is_empty() {
            // Saturating: the loop condition above re-reads `elapsed`, so on a
            // loaded host it can cross the cap between the two reads and the
            // plain subtraction would panic on the very stall this watcher
            // must tolerate (#978 boundary review).
            FIRST_BYTE.saturating_sub(start.elapsed())
        } else {
            QUIET
        };
        match timeout(wait, socket.read(&mut buffer)).await {
            Err(_elapsed) if !out.is_empty() => break,
            Err(_elapsed) => continue,
            Ok(Ok(0)) => {
                closed = true;
                break;
            }
            Ok(Ok(read)) => out.extend_from_slice(&buffer[..read]),
            // The TS recorder ignored socket errors and kept the bytes that
            // had arrived, so a reset while the response drains is a close.
            Ok(Err(_reset)) => {
                closed = true;
                break;
            }
        }
    }
    crate::negative_assert!(
        !out.is_empty() || closed,
        "#1351 every boundary cell produced bytes or closed"
    );
    (out.iter().map(|byte| *byte as char).collect(), closed)
}

/// The bytes both sides are compared in, applying the registered transport
/// diffs the same way `native_http::tests::normalize` does for its golden:
/// the Date value is masked (the recording is masked already, Rust renders
/// the live clock), header names are case-folded (hyper renders Title-Case,
/// TS writes lowercase and literal-cased names), hyper's implicit
/// `content-length: 0` line is dropped on the registered 431 cells only
/// (#2153, the same scoping native_http's `parser_admission` cells carry),
/// and the chunk-size line is case-folded (hyper writes `B`, Node writes
/// `b`). The cells this fixture records are one response each, with either
/// an empty body (101, 431) or one small chunk plus terminator (200), so
/// the folding only walks that shape.
fn budget_normalize(raw: &str, parser_admission: bool) -> String {
    let Some((head, body)) = raw.split_once("\r\n\r\n") else {
        return raw.to_owned();
    };
    let mut out = String::with_capacity(raw.len());
    let mut chunked = false;
    for (index, line) in head.split("\r\n").enumerate() {
        if index == 0 {
            out.push_str(line);
            out.push_str("\r\n");
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            out.push_str(line);
            out.push_str("\r\n");
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
        if name.eq_ignore_ascii_case("date") {
            out.push_str("date: <masked>\r\n");
        } else if (parser_admission || crate::mutants::on("native-ws-budget-cl0-mask-always"))
            && name.eq_ignore_ascii_case("content-length")
            && value.trim() == "0"
        {
            // hyper's implicit empty-body length on the registered 431s
            // only.
        } else {
            out.push_str(&name.to_ascii_lowercase());
            out.push(':');
            out.push_str(value);
            out.push_str("\r\n");
        }
    }
    out.push_str("\r\n");
    let mut rest = body;
    while chunked && !rest.is_empty() {
        let Some(line_end) = rest.find("\r\n") else {
            out.push_str(rest);
            break;
        };
        let size_text = rest[..line_end].split(';').next().unwrap_or("");
        let size = usize::from_str_radix(size_text.trim(), 16)
            .unwrap_or_else(|_| panic!("#1351 chunk size {size_text:?} is not hex"));
        out.push_str(&size_text.to_ascii_lowercase());
        out.push_str("\r\n");
        rest = &rest[line_end + 2..];
        if rest.len() < size + 2 {
            out.push_str(rest);
            break;
        }
        out.push_str(&rest[..size]);
        out.push_str("\r\n");
        rest = &rest[size + 2..];
        if size == 0 {
            break;
        }
    }
    if !chunked {
        out.push_str(body);
    }
    out
}

/// #2153: the cells whose replay needs the `content-length: 0` exemption —
/// exactly the fixture's 431 cells, the canned span-budget rejection
/// (`native_http::request_header_fields_too_large`) whose typed empty body
/// hyper adds a `content-length: 0` line to while the TS golden records none.
/// Like native_http's `PARSER_ADMISSION_CELLS` the list is explicit, so a
/// cell that grows a length line anywhere else replays red instead of
/// normalizing away; `parser_admission_cells_cover_exactly_the_oracle_431s`
/// holds the list against the oracle, and
/// `budget_content_length_zero_stays_scoped_to_the_registered_431s` pins the
/// raw bytes behind the mask in both directions.
const PARSER_ADMISSION_CELLS: &[&str] = &[
    "plain-16384",
    "plain-16385",
    "plain-20480",
    "upgrade-16384",
    "upgrade-16385",
    "upgrade-17020",
    "upgrade-20480",
];

/// #2153 fix-r1 (review r1): the predicate that grants the
/// `content-length: 0` mask. Every consumer — the exemption in the boundary
/// replay and the coverage guard — goes through this one function, so
/// widening the predicate anywhere fails the guard instead of silently
/// re-widening the mask (the webhook fix-r3 shape).
fn parser_admission(id: &str) -> bool {
    if crate::mutants::on("native-ws-budget-cl0-off") {
        return false;
    }
    if crate::mutants::on("native-ws-budget-cl0-wide") {
        return true;
    }
    PARSER_ADMISSION_CELLS.contains(&id)
}

/// #1351 (2026-11-06 review): the TrackHeader arithmetic the TS recorder
/// applies at record time (`generate.mts` `trackedSpan`), recomputed from the
/// *recorded* request bytes — target length plus every header name/value
/// length, the colon and the leading space after it not counted. The replay
/// asserts it against the recorded `tracked`, so copying one cell's request
/// into another cell can no longer stay green while silently losing the
/// boundary a cell is supposed to pin.
fn budget_tracked_span(request: &str) -> usize {
    let head = request.split("\r\n\r\n").next().unwrap_or_default();
    let mut lines = head.split("\r\n");
    let target = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .expect("#1351 recorded request line");
    let mut span = target.len();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .expect("#1351 recorded request header line");
        span += name.len() + value.trim_start_matches(' ').len();
    }
    span
}

#[tokio::test]
async fn upgrade_budget_boundary_matches_ts() {
    let expected = budget_oracle();
    let cells = expected["cells"].as_array().expect("#1351 oracle cells");
    let mut ids: Vec<&str> = cells
        .iter()
        .map(|cell| cell["id"].as_str().expect("#1351 oracle cell id"))
        .collect();
    ids.sort_unstable();
    // #1351 (2026-11-06 review): compare the cell *set*, not just its length —
    // a duplicated id plus a dropped cell would keep `len() == 9` while
    // silently losing a covered cell, and `plain-16385` / `plain-20480` are
    // covered only by this replay (the standalone cell tests are upgrade-only).
    negative_assert_eq!(
        ids,
        vec![
            "plain-16383",
            "plain-16384",
            "plain-16385",
            "plain-20480",
            "upgrade-16383",
            "upgrade-16384",
            "upgrade-16385",
            "upgrade-17020",
            "upgrade-20480",
        ],
        "#1351 the fixture records exactly the nine expected boundary cells"
    );
    let (address, stop, task) = start(Arc::default(), DEADLINE).await;
    for cell in cells {
        let id = cell["id"].as_str().expect("#1351 oracle cell id");
        let request = cell["request"].as_str().expect("#1351 oracle cell request");
        let recorded = cell["response"]
            .as_str()
            .expect("#1351 oracle cell response");
        let closed = cell["closed"]
            .as_bool()
            .expect("#1351 oracle cell closed flag");
        let tracked = cell["tracked"].as_u64().expect("#1351 oracle cell tracked") as usize;
        negative_assert_eq!(
            budget_tracked_span(request),
            tracked,
            "#1351 recorded cell {id} tracked span matches its own request bytes"
        );
        eprintln!("#1351 replaying boundary cell {id} (tracked {tracked})");
        let mut socket = TcpStream::connect(address)
            .await
            .expect("#1351 boundary cell connect");
        socket
            .write_all(request.as_bytes())
            .await
            .expect("#1351 boundary cell send");
        let (raw, observed) = budget_capture(&mut socket).await;
        let _ = socket.shutdown().await;
        // #2153: the mask is granted per cell through `fn parser_admission`,
        // so every other cell's length lines compare verbatim.
        let admitted = parser_admission(id);
        negative_assert_eq!(
            budget_normalize(&raw, admitted),
            budget_normalize(recorded, admitted),
            "#1351 every boundary cell's response matches the recorded TS bytes"
        );
        negative_assert_eq!(
            observed,
            closed,
            "#1351 every boundary cell closes exactly when the TS recorder saw a close"
        );
    }
    finish(stop, task).await;
}

/// #2153 fix-r1 (review r1): the exemption list and the oracle describe the
/// same cell set, so neither can drift alone — the admitted ids, read
/// through the same `fn parser_admission` the replay's exemption uses, are
/// exactly the oracle's 431 cells (the canned rejection of the span budget;
/// the served cells answer 101 or a chunked 200). A cell that gains or loses
/// the 431 shape fails here with its id, and a stale list entry points at
/// the cell to re-check — widening or dropping the predicate fails here
/// instead of re-widening the replay's mask.
#[test]
fn parser_admission_cells_cover_exactly_the_oracle_431s() {
    let recorded = budget_oracle();
    let cells = recorded["cells"].as_array().expect("#1351 oracle cells");
    let mut admitted: Vec<&str> = cells
        .iter()
        .map(|cell| cell["id"].as_str().expect("#1351 oracle cell id"))
        .filter(|id| parser_admission(id))
        .collect();
    admitted.sort_unstable();
    let mut oracle_431s: Vec<&str> = cells
        .iter()
        .filter(|cell| {
            cell["response"]
                .as_str()
                .expect("#1351 oracle cell response")
                .starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n")
        })
        .map(|cell| cell["id"].as_str().expect("#1351 oracle cell id"))
        .collect();
    oracle_431s.sort_unstable();
    negative_assert_eq!(
        admitted,
        oracle_431s,
        "#2153 the content-length exemption covers exactly the oracle's 431 cells"
    );
}

/// #2153: the raw-pin anchors, standalone literals the native-ws-entry
/// negative-control table's audit can quote and the receipt protocol
/// records verbatim (`negative_receipt.rs` matches the recorded assertion
/// by exact equality, so an interpolated message never matches the table's
/// literal anchor — fix-r2, review r2).
const PIN_REGISTERED_431: &str = "#2153 pin: the registered 431 carries content-length: 0";
const PIN_SERVED_NO_LENGTH: &str = "#2153 pin: the served cell carries no content-length at all";

/// #2153 fix-r2 (review r2): record the anchor constant itself as the
/// receipt's assertion — the same record-literal-then-panic-with-detail
/// shape as `negative_receipt.rs`'s `expect_*` helpers — and keep the cell
/// id and raw bytes in the panic text for the human reader. Both directions
/// fail here on the wire bytes (the 101 cell's inserted `content-length: 0`
/// is on the wire under the pinned hyper), so a verbatim receipt makes the
/// table's two product rows KILLED without touching their bindings.
fn pin_failure(assertion: &str, id: &str, raw: &str) -> ! {
    crate::test_negative_receipt::record_failure(assertion);
    panic!("{assertion} ({id}): {raw:?}");
}

/// #2153: the `content-length: 0` exemption `fn parser_admission` grants the
/// 431 cells is the only one, so the raw bytes have to be pinned in both
/// directions or the mask could hide a drift: a 431 that stopped carrying
/// the line (the rejection disappearing into a bare close) and a served
/// cell that grew one (the extra length line the narrowed replay must
/// catch) both compare green through the mask alone. Every exchange writes
/// the cell's own recorded request bytes, like the replay.
#[tokio::test]
async fn budget_content_length_zero_stays_scoped_to_the_registered_431s() {
    let recorded = budget_oracle();
    let cells = recorded["cells"].as_array().expect("#1351 oracle cells");
    let (address, stop, task) = start(Arc::default(), DEADLINE).await;
    for cell in cells {
        let id = cell["id"].as_str().expect("#1351 oracle cell id");
        let request = cell["request"]
            .as_str()
            .expect("#1351 oracle cell request")
            .as_bytes();
        let served_431 = cell["response"]
            .as_str()
            .expect("#1351 oracle cell response")
            .starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n");
        let mut socket = TcpStream::connect(address)
            .await
            .expect("#2153 content-length pin connect");
        socket
            .write_all(request)
            .await
            .expect("#2153 content-length pin send");
        let (raw, _) = budget_capture(&mut socket).await;
        let _ = socket.shutdown().await;
        let lower = raw.to_ascii_lowercase();
        if served_431 {
            if !lower.contains("\r\ncontent-length: 0\r\n") {
                pin_failure(PIN_REGISTERED_431, id, &raw);
            }
        } else if lower.contains("\r\ncontent-length:") {
            pin_failure(PIN_SERVED_NO_LENGTH, id, &raw);
        }
    }
    finish(stop, task).await;
}

/// #2153: the exemption `fn parser_admission` grants is the only masking
/// left — an injected `content-length: 0` on a served cell must survive
/// normalization (an extra hyper length line replays red), while the
/// registered 431's line stays masked.
#[test]
fn only_the_registered_cells_mask_a_content_length_zero() {
    let recorded = budget_oracle();
    let response = |id: &str| {
        recorded["cells"]
            .as_array()
            .expect("#1351 oracle cells")
            .iter()
            .find(|cell| cell["id"].as_str() == Some(id))
            .and_then(|cell| cell["response"].as_str())
            .unwrap_or_else(|| panic!("#2153 the {id} cell is missing from the oracle"))
            .to_owned()
    };
    let served = response("plain-16383");
    let injected = served.replacen("\r\n\r\n", "\r\ncontent-length: 0\r\n\r\n", 1);
    negative_assert!(
        budget_normalize(&injected, false) != budget_normalize(&served, false),
        "#2153 an injected content-length: 0 on a served cell must not normalize away"
    );
    // The registered direction stays masked: hyper's line on the 431 is the
    // one exemption left, so the TS-shaped bytes still compare equal.
    let bare = response("plain-16384");
    let hyper_shaped = bare.replacen("\r\n\r\n", "\r\ncontent-length: 0\r\n\r\n", 1);
    negative_assert_eq!(
        budget_normalize(&hyper_shaped, true),
        budget_normalize(&bare, true),
        "#2153 the registered 431 exemption still masks hyper's content-length: 0"
    );
}

/// `upgrade_head_respects_span_budget` (tracked 93 + n): writes the head,
/// asserts the canned 431 and its `Connection: close`, then requires the
/// connection to close (the read reaching EOF — the same two-step
/// `expect_ok` the boundary test uses).
async fn upgrade_budget_cell(address: std::net::SocketAddr, n: usize, what: &str) {
    let mut socket = TcpStream::connect(address)
        .await
        .unwrap_or_else(|error| panic!("#1351 {what} connect: {error}"));
    let head = format!(
        "GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {RFC6455_SAMPLE_NONCE}\r\nx-p: {}\r\n\r\n",
        "a".repeat(n)
    );
    socket
        .write_all(head.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("#1351 {what} send: {error}"));
    let mut header = vec![];
    // #1891 fix-r1: response bytes (see RESPONSE_LIVENESS).
    timeout(RESPONSE_LIVENESS, async {
        while !header.ends_with(b"\r\n\r\n") {
            header.push(
                socket
                    .read_u8()
                    .await
                    .unwrap_or_else(|error| panic!("#1351 {what} response byte: {error}")),
            );
        }
    })
    .await
    .unwrap_or_else(|elapsed| panic!("#1351 {what} response deadline: {elapsed}"));
    let response = String::from_utf8_lossy(&header).to_string();
    negative_assert!(
        response.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
        "#1351 {what} is the same 431 as the non-upgrade lane"
    );
    negative_assert!(
        response.contains("Connection: close\r\n"),
        "#1351 {what} is told to close like the non-upgrade lane"
    );
    let close = format!("#1351 {what} closes the connection like the non-upgrade lane");
    let mut rest = vec![];
    // #1891 fix-r1: response bytes (see RESPONSE_LIVENESS).
    crate::test_negative_receipt::expect_ok(
        crate::test_negative_receipt::expect_ok(
            timeout(RESPONSE_LIVENESS, socket.read_to_end(&mut rest)).await,
            &close,
        ),
        &close,
    );
}

/// #1351 (2026-11-06 round): the one-past-the-boundary upgrade cell — tracked
/// 16385 (n = 16292) is the same canned 431 + close as the 16384 boundary
/// cell, recorded on the TS baseline by this round's raw-socket probe
/// (`_tmp/20261106-issue-1351/probe.out`, Node v22.22.0) and golden-replayed
/// byte-exactly by `upgrade_budget_boundary_matches_ts`. Kept in its own test
/// like #1209's `header_budget_deep_oversize_431s_like_ts` so that a panic in
/// the boundary replay cannot mask this cell, and so a hand-run
/// `PDRS_MUTANT=native-http-upgrade-budget-off … --exact` pins this cell on its
/// own (raw output in `_tmp/20261106-issue-1351/negctl-newcells/`). It has no
/// `native-ws-entry/negative_controls.py` row yet: that table binds one switch
/// name to one test, and a per-cell switch needs its own site in the upgrade
/// guard (#1757).
#[tokio::test]
async fn upgrade_budget_16385_cell_431s_like_ts() {
    let (address, stop, task) = start(Arc::default(), DEADLINE).await;
    upgrade_budget_cell(address, 16292, "the 16385 upgrade cell").await;
    finish(stop, task).await;
}

/// #1351 (2026-11-06 round): the 20 KiB far-over upgrade cell — tracked 20480
/// (n = 20387) is the same canned 431 + close, recorded on the TS baseline by
/// this round's raw-socket probe (`_tmp/20261106-issue-1351/probe.out`, Node
/// v22.22.0) and golden-replayed byte-exactly by
/// `upgrade_budget_boundary_matches_ts`. A test of its own so the 16385 cell's
/// panic cannot mask this cell, and so a hand-run
/// `PDRS_MUTANT=native-http-upgrade-budget-off … --exact` pins this cell on its
/// own (raw output in `_tmp/20261106-issue-1351/negctl-newcells/`). Like the
/// 16385 cell it has no negative-control row yet (#1757).
#[tokio::test]
async fn upgrade_budget_20kib_cell_431s_like_ts() {
    let (address, stop, task) = start(Arc::default(), DEADLINE).await;
    upgrade_budget_cell(address, 20387, "the 20 KiB upgrade cell").await;
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

/// #2391: settle the two conflicting statements about what the WS layer does
/// to an in-flight request when its client disconnects — `runtime/settings/
/// gateway.rs` claimed "the WS layer drops in-flight requests on disconnect",
/// while the connection contract (`runtime/connection/mod.rs`: drive the
/// returned future to completion, retain and drain request tasks) and this
/// transport's retain+drain loops say the job survives. The request body
/// parks at the fixture gate, the client disconnects, and the body must still
/// finish: the job is host-owned, not socket-owned, so its transcript-write
/// reservation (or any other body-held state) is released by the body's own
/// exit, not by the socket's.
#[tokio::test]
async fn disconnect_leaves_in_flight_request_running_to_completion() {
    let requests = Arc::new(Requests {
        hold_request: AtomicBool::new(true),
        ..Requests::default()
    });
    let (address, stop, task) = start(requests.clone(), DEADLINE).await;
    let mut client = Client::connect(address).await;
    client.hello().await;
    client.json().await;
    client
        .send(json!({"type":"request","id":"held","method":"probe","params":{}}))
        .await;
    timeout(DEADLINE, requests.request_entered.notified())
        .await
        .expect("#2391 request body entered its await");
    // Disconnect the client mid-request: close the write side and drop the
    // socket, so the transport sees EOF rather than a graceful WS close.
    client.socket.shutdown().await.expect("#2391 client FIN");
    drop(client);
    // The job is parked inside the request body. Register the done-waiter
    // before opening the gate — notify_waiters keeps no permit for a future
    // registered after the broadcast.
    let done = requests.request_done.notified();
    tokio::pin!(done);
    done.as_mut().enable();
    requests.release.notify_waiters();
    timeout(DEADLINE, &mut done)
        .await
        .expect("#2391 the disconnected request still runs to completion");
    finish(stop, task).await;
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
    // #1891 fix-r1: response bytes (see RESPONSE_LIVENESS).
    crate::test_negative_receipt::expect_ok(
        timeout(RESPONSE_LIVENESS, socket.read_to_end(&mut tail)).await,
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
    // #1131 intentionally replaced the A4 stderr `proxy listening` line with
    // the TS stdout address lines (actual bound port); re-pinned for that diff.
    // #1354 grew the opt-in into the entry's routing region: the explicit
    // `--native` argument plus the `PILOTDECK_GATEWAY_MODE` config item. Both
    // the region and its argv plumbing helper are removed before hashing, so
    // the residual hash still pins every byte outside them to the A4
    // executable; the executable-level routing matrix is tests/mode_routing.rs.
    const ROUTING: &str = "    let mut args = std::env::args_os().skip(1);\n    let first = args.next();\n    let explicit_native = pilotdeck_gateway::native_process::native_requested(first.as_deref());\n    // #1354 A8.5-pr3: the main entry's routing. The explicit `--native`\n    // argument (#1071) wins over the `PILOTDECK_GATEWAY_MODE` config item —\n    // the per-invocation flag outranks the deployment default, like the TS\n    // baseline's `readStringFlag(argv, \"--host\") ?? env.PILOTDECK_GATEWAY_HOST`\n    // (src/cli/pilotdeck.ts:213). The config item is still judged *before*\n    // the flag short-circuits anything, so an invalid value fails closed at\n    // startup even when `--native` is present (#1369 shape). The config\n    // item's default is proxy mode (#1303 option A), so without either\n    // explicit switch the entry below is the unchanged A4 proxy: the\n    // rollback target stage A keeps.\n    let mode_native = pilotdeck_gateway::native_process::gateway_mode(\n        std::env::var_os(\"PILOTDECK_GATEWAY_MODE\").as_deref(),\n    )?;\n    if explicit_native || mode_native {\n        return pilotdeck_gateway::native_process::run(\n            pilotdeck_gateway::native_process::Options::parse(native_arguments(\n                explicit_native,\n                first,\n                args,\n            ))?,\n        )\n        .await;\n    }\n";
    const ARGV_PLUMBING: &str = "/// #1354: `--native` is consumed by the router itself, so only the remaining\n/// arguments reach the native options parser; the config item's routing\n/// leaves every argument — `first` included — to that parser.\nfn native_arguments(\n    explicit: bool,\n    first: Option<std::ffi::OsString>,\n    rest: impl Iterator<Item = std::ffi::OsString>,\n) -> impl Iterator<Item = std::ffi::OsString> {\n    if explicit { None } else { first }.into_iter().chain(rest)\n}\n\n";
    // Source installation adds a separate exact opt-in; all legacy bytes
    // remain pinned after removing only these two approved entry sequences.
    const INSTALLATION_OWNER: &str = "    if let Some(code) =\n        pilotdeck_gateway::native_installation_owner_helper(std::env::args_os().skip(1))\n    {\n        std::process::exit(code);\n    }\n";
    let source = include_str!("../main.rs");
    negative_assert_eq!(
        source.matches(ROUTING).count(),
        1,
        "#1354 exact entry routing region"
    );
    negative_assert_eq!(
        source.matches(ARGV_PLUMBING).count(),
        1,
        "#1354 exact argv plumbing helper"
    );
    negative_assert_eq!(
        source.matches(INSTALLATION_OWNER).count(),
        1,
        "exact installation owner opt-in"
    );
    negative_assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(
                source
                    .replacen(ROUTING, "", 1)
                    .replacen(ARGV_PLUMBING, "", 1)
                    .replacen(INSTALLATION_OWNER, "", 1)
            )
        ),
        "02613cf4a756f4e9cfdb59c5356961798244feb209bf53a5485491c8b7dd4622",
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
        // #1891 fix-r1: response bytes (see RESPONSE_LIVENESS).
        timeout(RESPONSE_LIVENESS, client.read_to_end(&mut replies))
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
/// #1351: the boundary golden regenerates byte-identically from the TS
/// baseline (Node v22.22.0 first on PATH, `PDRS_BASELINE` set).
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn regenerates_budget_fixture_identically() {
    crate::test_baseline::regenerates_identically("native-ws-upgrade-budget");
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
    // #1891 fix-r1: response bytes (see RESPONSE_LIVENESS).
    timeout(RESPONSE_LIVENESS, client.read_to_end(&mut replies))
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
    // #1891 fix-r1: response bytes (see RESPONSE_LIVENESS).
    timeout(RESPONSE_LIVENESS, client.read_to_end(&mut output))
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
    // The non-loopback addresses must be refused before any Config derives
    // `loopback: address.ip().is_loopback()` from them (#1209 fix-r2), and
    // 127.0.0.2 pins the dependency the other way: only the literal
    // 127.0.0.1 can bind, so the derivation's 127.0.0.0/8 width (wider than
    // the TS `127.0.0.1`/`localhost` literals) is unreachable in practice
    // (#1209 fix-r3 review, opus concurrency-4 / DeepSeek P3-2).
    for (address, token) in [
        ("0.0.0.0:0", "token"),
        ("192.0.2.1:0", "token"),
        ("127.0.0.2:0", "token"),
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

/// #1292: the token route's loopback derivation is the TS literal
/// (`host === "127.0.0.1" || host === "localhost"`), not the wide
/// `is_loopback()` class — probes `b4-L1.out` (127.0.0.2 with
/// allowRemoteHost still gets a 404), `b4-L2.out`/`b4-L3.out` (localhost
/// and 127.0.0.1 serve the token). The bind policy keeps 127.0.0.2 out of
/// `bind` (the `native_configuration` refusal cases above), so the boundary
/// is pinned on the derivation itself: widening the bind policy without the
/// literal would leak `/auth/local-token` to the rest of 127.0.0.0/8 and
/// ::1. Binds the negative control `native-ws-loopback-literal` (the
/// relaxed `is_loopback()` derivation).
#[test]
fn loopback_literal_matches_ts() {
    let address = |text: &str| {
        text.parse::<std::net::IpAddr>()
            .expect("#1292 literal address")
    };
    negative_assert!(
        ts_loopback_literal(address("127.0.0.1"))
            && !ts_loopback_literal(address("127.0.0.2"))
            && !ts_loopback_literal(address("::1"))
            && !ts_loopback_literal(address("192.0.2.1")),
        "#1292 the loopback derivation is the TS 127.0.0.1/localhost literal, not the is_loopback() class"
    );
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
        // #1891 fix-r1: response bytes (see RESPONSE_LIVENESS).
        let received = timeout(RESPONSE_LIVENESS, async {
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

// --------------------------------------------------------------- #2434 stderr

/// The re-exec probe's env: `run:<token>`; the child also requires the token
/// as an argv positional, so a stray env value alone can never branch a
/// normal test run into the child path (the #2301 probe-token shape).
const WS_STDERR_PROBE_ENV: &str = "PDRS_2434_WS_STDERR_PROBE";
/// Where the child's panic hook leaves its evidence.
const WS_STDERR_MARKER_ENV: &str = "PDRS_2434_WS_STDERR_MARKER";

fn ws_stderr_probe_token() -> Option<String> {
    let value = std::env::var_os(WS_STDERR_PROBE_ENV)?.to_str()?.to_owned();
    let (_, token) = value.split_once(':')?;
    if token.is_empty() {
        return None;
    }
    std::env::args()
        .any(|argument| argument == token)
        .then(|| token.to_owned())
}

/// #2434: the accept loop's error lines were `eprintln!`, which panics when
/// the stderr write fails (std behavior). For the connection-error branch
/// the panic is contained by the JoinSet (the accept loop's `join_next`
/// branch drops the JoinError), so the blast radius is the dead connection
/// task and the lost diagnostic — TS's `console.error` never loses either.
/// The accept-error branch sits in the accept-loop task itself, where the
/// same panic would take the whole loop down. The fix is the shared
/// `console_log_line`, which drops the write error. This control drives the
/// reachable line — the connection error after an invalid upgrade — in a
/// re-exec'd child whose stderr is a pipe with the read end already dropped
/// (every write fails with EPIPE); the child installs a panic hook first, so
/// a panicking `eprintln!` leaves a marker file and exits 33 instead of
/// passing. A read-only fd is not a failing sink for this purpose:
/// `std::io::Stderr` redirects EBADF to a fresh `/dev/null` and reports
/// success, so only EPIPE reaches the `eprintln!` panic. The accept-error
/// line at the same call site got the same shape, but an `accept()` error
/// that is neither InvalidInput, PermissionDenied nor Unsupported has no
/// portable trigger, so it is covered by the shared helper's own contract,
/// not a second child.
#[tokio::test]
async fn connection_error_lines_survive_a_failed_stderr() {
    if let Some(token) = ws_stderr_probe_token() {
        ws_stderr_probe_child(&token).await;
        return;
    }
    let token = format!("pdrs-2434-{}", uuid::Uuid::new_v4().simple());
    let marker = std::env::temp_dir().join(format!("{token}.marker"));
    let mut child =
        tokio::process::Command::new(std::env::current_exe().expect("#2434 the probe binary path"))
            .args([
                "--exact",
                "native_ws::tests::connection_error_lines_survive_a_failed_stderr",
                "--nocapture",
                "--test-threads=1",
                &token,
            ])
            .env(WS_STDERR_PROBE_ENV, format!("run:{token}"))
            .env(WS_STDERR_MARKER_ENV, &marker)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("#2434 the failed-stderr probe child spawns");
    // The failing sink: a pipe whose read end is dropped before the child
    // writes, so every stderr write fails with EPIPE. (A read-only fd is not
    // enough: `std::io::Stderr` redirects EBADF to a fresh `/dev/null` and
    // reports success — only EPIPE reaches the `eprintln!` panic.)
    drop(child.stderr.take());
    // The stdout pipe is drained concurrently with the wait, never after it
    // (the #2301 probe's drain-before-wait shape): a full pipe — a failing
    // child prints a whole backtrace — would block the child before it could
    // exit, and the wait below would then report its 30 s deadline as a
    // false failure of the probe itself.
    let stdout = child.stdout.take();
    let console_task = tokio::spawn(async move {
        let mut out = Vec::new();
        if let Some(mut pipe) = stdout {
            let _ = pipe.read_to_end(&mut out).await;
        }
        String::from_utf8_lossy(&out).into_owned()
    });
    // Every wait states its termination condition: 30 seconds on the child,
    // then kill + reap before the named failure (the #2301 probe shape).
    let status = match tokio::time::timeout(DEADLINE * 6, child.wait()).await {
        Ok(status) => status.expect("#2434 the failed-stderr probe child is waitable"),
        Err(_) => {
            child
                .kill()
                .await
                .expect("#2434 the timed-out probe child is killed");
            let _ = child.wait().await;
            negative_assert!(
                false,
                "#2434 the failed-stderr probe child did not finish within 30s"
            );
            unreachable!("negative_assert! panics");
        }
    };
    let console = console_task
        .await
        .expect("#2434 the probe's stdout drain finishes");
    negative_assert!(
        status.success(),
        "#2434 the child with a write-failed stderr exits cleanly (exit {status})\n{console}"
    );
    // Positive receipt — success alone is not evidence the probe case ran: a
    // renamed test makes the `--exact` filter match nothing and libtest still
    // exits 0 with no marker (the vacuous-pass shape `skills_migrate` records
    // for its own rerun), so a loose contains-check would hold on a child that
    // executed nothing. The shared exact-receipt criterion — one `running 1
    // test` banner, the whole per-test line, and the one-test summary — is
    // what proves the case ran (the child body writes only stderr and the
    // reached file, so the single-threaded layout cannot splice the line).
    negative_assert!(
        crate::test_exact_receipt::single_pass(
            &status,
            "native_ws::tests::connection_error_lines_survive_a_failed_stderr",
            &console,
        ),
        "#2434 the child's libtest receipt proves the probe case ran\n{console}"
    );
    // The child leaves a reached marker once the invalid upgrade has been
    // answered — the proof the connection-error line was actually driven, not
    // merely that one test passed.
    let reached = std::env::temp_dir().join(format!("{token}.reached"));
    negative_assert!(
        reached.exists(),
        "#2434 the child never reached the invalid-upgrade answer"
    );
    let _ = std::fs::remove_file(&reached);
    negative_assert!(
        !marker.exists(),
        "#2434 no panic marker after the connection-error line ({}): {:?}",
        marker.display(),
        std::fs::read_to_string(&marker).unwrap_or_default()
    );
}

/// The child half of [`connection_error_lines_survive_a_failed_stderr`]:
/// panic hook first (marker + exit 33), then a real listener and the invalid
/// upgrade that makes `serve_socket` fail, then a clean drain.
async fn ws_stderr_probe_child(token: &str) {
    let marker = std::env::var(WS_STDERR_MARKER_ENV)
        .unwrap_or_else(|_| panic!("#2434 {token} is missing the marker env"));
    std::panic::set_hook(Box::new(move |info| {
        // The default hook would print to the failed stderr; the marker is
        // the parent's evidence instead.
        let _ = std::fs::write(&marker, format!("{info}"));
        std::process::exit(33);
    }));
    let host = native_ws_fixture(Arc::new(Requests::default()));
    let server = NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#2434 listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#2434 bind native");
    let address = server.local_addr().expect("#2434 bound address");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        DEADLINE,
    ));
    let mut socket = TcpStream::connect(address)
        .await
        .expect("#2434 invalid upgrade connect");
    socket
        .write_all(
            b"GET /wrong HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: key\r\n\r\n",
        )
        .await
        .expect("#2434 invalid upgrade send");
    let mut reply = vec![];
    let read = timeout(DEADLINE, socket.read_to_end(&mut reply)).await;
    crate::test_negative_receipt::expect_ok(
        crate::test_negative_receipt::expect_ok(read, "#2434 the invalid upgrade is answered"),
        "#2434 the invalid upgrade is answered",
    );
    // The parent's positive receipt that this half actually drove the
    // connection-error limb (an `--exact` filter matching nothing still runs
    // a passing, marker-free child).
    let _ = std::fs::write(
        std::env::temp_dir().join(format!("{token}.reached")),
        b"reached",
    );
    stop.send(()).expect("#2434 stop server");
    timeout(DEADLINE, task)
        .await
        .expect("#2434 server finish deadline")
        .expect("#2434 server task")
        .expect("#2434 successful drain");
}

// #2351: exercise the real runner with stop futures that complete, panic, or
// never complete. No production panic hook or process-global mutation is needed.
async fn phased_drain_fixture(stop: impl Future<Output = ()> + Send + 'static) -> PhasedRun {
    let host = native_ws_fixture(Arc::new(Requests::default()));
    NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#2351 fixture listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#2351 fixture bind")
    .run_phased(stop, Duration::from_millis(100))
}

async fn phased_runner_exited(phased: &PhasedRun) {
    timeout(DEADLINE, async {
        while !phased.runner.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#2351 runner exit is bounded");
}

#[tokio::test]
async fn phased_drain_waiter_cancellation_preserves_success() {
    let (stop, stopped) = oneshot::channel();
    let phased = phased_drain_fixture(async {
        stopped.await.expect("#2351 normal stop is delivered");
    })
    .await;
    let mut predecessor = Box::pin(phased.drained());
    negative_assert!(
        poll_request(predecessor.as_mut()).is_pending(),
        "#2351 predecessor waits for stop"
    );
    drop(predecessor);
    let mut first = Box::pin(phased.drained());
    let mut second = Box::pin(phased.drained());
    negative_assert!(
        poll_request(first.as_mut()).is_pending() && poll_request(second.as_mut()).is_pending(),
        "#2351 successors wait independently before stop"
    );
    stop.send(()).expect("#2351 normal runner still owns stop");
    let verdicts = timeout(DEADLINE, async { tokio::join!(first, second) })
        .await
        .expect("#2351 normal waiters finish within deadline");
    negative_assert_eq!(
        verdicts,
        (true, true),
        "#2351 normal waiters agree on success"
    );
    negative_assert!(
        timeout(DEADLINE, phased.drained())
            .await
            .expect("#2351 repeated successful drain is bounded"),
        "#2351 late waiter preserves success after runner exits"
    );
    timeout(DEADLINE, phased.finish())
        .await
        .expect("#2351 successful finish is bounded")
        .expect("#2351 successful drain finishes cleanly");
}

#[tokio::test]
async fn phased_drain_runner_panic_wakes_all_waiters() {
    let (stop, stopped) = oneshot::channel();
    let phased = phased_drain_fixture(async {
        stopped.await.expect("#2351 panic trigger is delivered");
        panic!("#2351 injected runner panic");
    })
    .await;
    let mut first = Box::pin(phased.drained());
    let mut second = Box::pin(phased.drained());
    negative_assert!(
        poll_request(first.as_mut()).is_pending() && poll_request(second.as_mut()).is_pending(),
        "#2351 both waiters are pending before runner panic"
    );
    stop.send(())
        .expect("#2351 runner still owns panic trigger");
    phased_runner_exited(&phased).await;
    // A failed runner must not require this lock to publish its failure. A
    // different caller may hold it through a slow finish after the panic.
    let held = timeout(DEADLINE, phased.inner.lock())
        .await
        .expect("#2351 panic releases the inner mutex");
    let verdicts = crate::test_negative_receipt::expect_ok(
        timeout(DEADLINE, async { tokio::join!(first, second) }).await,
        "#2351 panic wakes every drained waiter within deadline",
    );
    negative_assert_eq!(
        verdicts,
        (false, false),
        "#2351 panic is a failed drain for every waiter"
    );
    negative_assert!(
        !timeout(DEADLINE, phased.drained())
            .await
            .expect("#2351 late panic waiter is bounded without the inner lock"),
        "#2351 late panic waiter observes the same failure"
    );
    drop(held);
    negative_assert!(
        timeout(DEADLINE, phased.finish())
            .await
            .expect("#2351 finish after panic is bounded")
            .is_err(),
        "#2351 finish after panic reports failure"
    );
}

#[tokio::test]
async fn phased_drain_runner_abort_is_observable() {
    for started in [false, true] {
        let (entered, entry) = oneshot::channel();
        let phased = phased_drain_fixture(async move {
            entered.send(()).expect("#2351 runner entry is observed");
            std::future::pending::<()>().await;
        })
        .await;
        if started {
            timeout(DEADLINE, entry)
                .await
                .expect("#2351 runner starts within deadline")
                .expect("#2351 runner entered its never-completing stop");
        }
        phased.runner.abort();
        phased_runner_exited(&phased).await;
        for _ in 0..2 {
            negative_assert!(
                !timeout(DEADLINE, phased.drained())
                    .await
                    .expect("#2351 aborted runner cannot strand a drain waiter"),
                "#2351 abort is consistently a failed drain"
            );
        }
        negative_assert!(
            timeout(DEADLINE, phased.finish())
                .await
                .expect("#2351 finish after abort is bounded")
                .is_err(),
            "#2351 finish after abort reports failure"
        );
    }
}

#[tokio::test]
async fn phased_drain_drop_aborts_runner() {
    let (held, released) = oneshot::channel::<()>();
    let phased = phased_drain_fixture(async move {
        let _held = held;
        std::future::pending::<()>().await;
    })
    .await;
    drop(phased);
    negative_assert!(
        timeout(DEADLINE, released)
            .await
            .expect("#2351 dropping PhasedRun releases its runner stop future")
            .is_err(),
        "#2351 runner stop future is dropped without completing"
    );
}

/// #2557: a partial-head HTTP client — the connection that keeps the
/// close-notice window's join set non-empty. It never upgrades, so the stop
/// is consumed without closing it (`served_heads == 0` is never idle) and
/// the close-notice watch the WS transport alone observes does not apply;
/// its headers deadline is disarmed by the same stop (the #749 half
/// handshake shape), it never submits a host job, and nothing but the strong
/// abort reclaims it.
async fn partial_head_survivor(address: SocketAddr) -> TcpStream {
    let mut socket = TcpStream::connect(address)
        .await
        .expect("#2557 survivor connect");
    socket
        .write_all(b"GET /ws HTTP/1.1\r\n")
        .await
        .expect("#2557 survivor partial head write");
    socket
}

/// #2557: the two merged `request_shutdown` re-issue sites in the S4 half —
/// the revisit branch's (the interrupted marker) and the first run's (ahead
/// of the abort join) — each need a deletion that turns this test red. The
/// gated cleanup runs inside the host teardown drain, so its `entered`
/// signal is exactly "the teardown request went out and the dedicated host
/// thread acted on it"; nothing here awaits `stopped()`, so a request that
/// never fires is a signal that never comes. Real clock like the interrupted
/// revisit's: the host thread's cross-thread wake is what is awaited.
#[tokio::test]
async fn a_cancelled_owner_finish_still_requests_the_host_teardown() {
    // Scenario A — the drop inside the close-notice window: the revisit's
    // own re-issue is the only request that can follow.
    {
        let (entered, entered_rx) = oneshot::channel::<()>();
        let (release, release_rx) = oneshot::channel::<()>();
        let host = native_ws_fixture_with_cleanup(Arc::new(Requests::default()), async move {
            let _ = entered.send(());
            let _ = release_rx.await;
            Ok(())
        });
        let server = NativeWsServer::bind(
            "127.0.0.1:0"
                .parse()
                .expect("#2557 scenario A listen address"),
            host,
            "fixture-token".into(),
            "probe".into(),
        )
        .await
        .expect("#2557 scenario A fixture bind");
        let address = server.local_addr().expect("#2557 scenario A address");
        let (stop, stop_rx) = oneshot::channel::<()>();
        let phased = server.run_phased(
            async move {
                let _ = stop_rx.await;
            },
            Duration::from_millis(100),
        );
        let survivor = partial_head_survivor(address).await;
        // A completed handshake behind the survivor is the accept-loop
        // barrier (the #749 shape): the FIFO accept guarantees the
        // survivor's task is already in the connections set when the stop
        // below fires, so the drain is really held open by it, not racing
        // a backlog entry the loop never accepted.
        let mut barrier = Client::connect(address).await;
        barrier.close().await;
        stop.send(()).expect("#2557 scenario A stop fires");
        negative_assert!(
            !timeout(DEADLINE, phased.drained())
                .await
                .expect("#2557 scenario A drain bounded"),
            "#2557 the survivor holds the entry drain open"
        );
        // One poll parks the first finish at the window join — the only
        // await point before the request: the marker is set, the notice is
        // out, and no request has been sent.
        let mut first = Box::pin(phased.finish());
        negative_assert!(
            poll_request(first.as_mut()).is_pending(),
            "#2557 the first finish parks in the close-notice window"
        );
        drop(first);
        let verdict = timeout(DEADLINE, phased.finish())
            .await
            .expect("#2557 scenario A revisit is bounded");
        negative_assert!(
            matches!(&verdict, Err(error) if error.to_string()
                .contains("native WS owner finish was interrupted")),
            "#2557 the revisit records the interrupted verdict"
        );
        // The #2557 direct-deletion witness (the revisit branch's re-issue
        // deleted — no optional switch here, per the R6 correction entry
        // (A8.5-SWITCH-MACHINE-001-R6) these two direct-deletion
        // witnesses carry no fixture row): with it
        // gone, no request ever goes out — the first run died inside the
        // window and the unawaited `StopHost` drop request is all that is
        // left while the phased run is alive.
        negative_assert!(
            matches!(timeout(DEADLINE, entered_rx).await, Ok(Ok(()))),
            "#2557 the revisit re-issue requests the host teardown"
        );
        release.send(()).expect("#2557 scenario A cleanup release");
        drop(survivor);
    }
    // Scenario B — the drop past the window, inside the abort join: the
    // first run's own re-issue is the only request that has gone out, and it
    // is asserted before any revisit could send one.
    {
        let (entered, entered_rx) = oneshot::channel::<()>();
        let (release, release_rx) = oneshot::channel::<()>();
        let host = native_ws_fixture_with_cleanup(Arc::new(Requests::default()), async move {
            let _ = entered.send(());
            let _ = release_rx.await;
            Ok(())
        });
        let server = NativeWsServer::bind(
            "127.0.0.1:0"
                .parse()
                .expect("#2557 scenario B listen address"),
            host,
            "fixture-token".into(),
            "probe".into(),
        )
        .await
        .expect("#2557 scenario B fixture bind");
        let address = server.local_addr().expect("#2557 scenario B address");
        let (stop, stop_rx) = oneshot::channel::<()>();
        let phased = server.run_phased(
            async move {
                let _ = stop_rx.await;
            },
            Duration::from_millis(100),
        );
        let survivor = partial_head_survivor(address).await;
        // The same accept-loop barrier as scenario A's.
        let mut barrier = Client::connect(address).await;
        barrier.close().await;
        stop.send(()).expect("#2557 scenario B stop fires");
        negative_assert!(
            !timeout(DEADLINE, phased.drained())
                .await
                .expect("#2557 scenario B drain bounded"),
            "#2557 the survivor again holds the entry drain open"
        );
        let mut first = Box::pin(phased.finish());
        negative_assert!(
            poll_request(first.as_mut()).is_pending(),
            "#2557 the first finish parks in the window again"
        );
        // Let the window expire in real time: a paused clock would race
        // its auto-advance against the host thread's cross-thread `entered`
        // wake, which nothing virtual would deliver in time.
        tokio::time::sleep(CLOSE_FRAME_WINDOW + Duration::from_millis(50)).await;
        // The second poll runs the window's expiry through: the request
        // ahead of the abort join fires synchronously, and the half parks in
        // the join's await (the aborted survivor's reaping needs the
        // scheduler, so this manual poll stays Pending).
        negative_assert!(
            poll_request(first.as_mut()).is_pending(),
            "#2557 the first finish parks in the abort join past the request"
        );
        // The #2557 direct-deletion witness (the first run's re-issue
        // deleted — no optional switch here, per the R6 correction entry
        // (A8.5-SWITCH-MACHINE-001-R6) these two direct-deletion
        // witnesses carry no fixture row): with it
        // gone, no request has gone out — the half was cut before
        // `host.shutdown`'s own request was reached, and the revisit below
        // is what would have sent one. Anchored before that.
        negative_assert!(
            matches!(timeout(DEADLINE, entered_rx).await, Ok(Ok(()))),
            "#2557 the pre-join re-issue requests the host teardown"
        );
        drop(first);
        let verdict = timeout(DEADLINE, phased.finish())
            .await
            .expect("#2557 scenario B revisit is bounded");
        negative_assert!(
            matches!(&verdict, Err(error) if error.to_string()
                .contains("native WS owner finish was interrupted")),
            "#2557 the revisit still records the interrupted verdict"
        );
        release.send(()).expect("#2557 scenario B cleanup release");
        drop(survivor);
    }
}
