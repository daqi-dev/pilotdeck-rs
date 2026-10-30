use super::*;
use crate::{
    negative_assert, negative_assert_eq,
    runtime::{
        connection::{Call, RequestError},
        turn::gateway::{GatewayRequests, tests::native_ws_fixture},
    },
    test_native_ws_sample::RFC6455_SAMPLE_NONCE,
    test_negative_receipt::{expect_ok, expect_some},
};
use pilotdeck_protocol::{JsString, JsValue};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::{sync::Notify, time::timeout};

const DEADLINE: Duration = Duration::from_secs(5);
const CAP_ASSERT: &str = "#1079 connection budget bounds half handshakes";
const INPUT_ASSERT: &str = "#1079 aggregate pressure closes only contender";
const CLEANUP_ASSERT: &str = "#1079 shutdown and cancellation release all resources";
const ORACLE: &str = include_str!("../../tests/fixtures/native-ws-caps/oracle.json");

#[derive(Default)]
struct Requests {
    hold: AtomicBool,
    entered: Notify,
    release: Notify,
    active: AtomicUsize,
}
impl GatewayRequests for Requests {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            if self.hold.load(Ordering::SeqCst) {
                let released = self.release.notified();
                tokio::pin!(released);
                released.as_mut().enable();
                self.active.fetch_add(1, Ordering::SeqCst);
                self.entered.notify_one();
                released.await;
                self.active.fetch_sub(1, Ordering::SeqCst);
            }
            Ok(crate::metadata::object([("name", "caps".into())]))
        })
    }
    fn request(&self, _: JsString, _: JsValue) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async { Err(RequestError::new("#1079 unexpected request")) })
    }
}
struct Running {
    address: SocketAddr,
    connections: Arc<Semaphore>,
    input: Arc<Semaphore>,
    task_high_watermark: Arc<std::sync::atomic::AtomicUsize>,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), ProxyError>>,
}
async fn start(count: usize, bytes: usize, requests: Arc<Requests>) -> Running {
    let host = native_ws_fixture(requests);
    let mut server = NativeWsServer::bind(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#1079 bind isolated listener");
    server.resources = Resources::new(count, bytes);
    let address = server.local_addr().expect("#1079 allocated address");
    let connections = server.resources.connections.clone();
    let input = server.resources.input.clone();
    let task_high_watermark = server.resources.task_high_watermark.clone();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        Duration::from_millis(30),
    ));
    Running {
        address,
        connections,
        input,
        task_high_watermark,
        stop,
        task,
    }
}
async fn remaining(budget: &Semaphore, value: usize, assertion: &str) {
    expect_ok(
        timeout(DEADLINE, async {
            while budget.available_permits() != value {
                tokio::task::yield_now().await;
            }
        })
        .await,
        assertion,
    );
}
async fn tcp(address: SocketAddr) -> TcpStream {
    expect_ok(
        timeout(DEADLINE, TcpStream::connect(address)).await,
        "#1079 TCP connect deadline",
    )
    .expect("#1079 TCP connect")
}
async fn upgrade(address: SocketAddr) -> TcpStream {
    let mut socket = tcp(address).await;
    let request = format!(
        "GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {RFC6455_SAMPLE_NONCE}\r\n\r\n"
    );
    write(&mut socket, request.as_bytes()).await;
    let header = expect_ok(
        timeout(DEADLINE, async {
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.expect("#1079 upgrade response byte"));
                assert!(header.len() < 4096, "#1079 bounded handshake response");
            }
            header
        })
        .await,
        "#1079 upgrade response deadline",
    );
    assert!(header.starts_with(b"HTTP/1.1 101"), "#1079 upgrade status");
    socket
}
async fn write(socket: &mut TcpStream, bytes: &[u8]) {
    expect_ok(
        timeout(DEADLINE, socket.write_all(bytes)).await,
        "#1079 write deadline",
    )
    .expect("#1079 socket write");
}
async fn read_frame(socket: &mut TcpStream, assertion: &str) -> (u8, Vec<u8>) {
    expect_ok(
        timeout(DEADLINE, async {
            let mut decoder = Decoder::default();
            loop {
                if let Some(frame) = decoder.next_server().expect("#1079 server decoder") {
                    return (frame.opcode, frame.payload);
                }
                let count = socket
                    .read_buf(&mut decoder.buffer)
                    .await
                    .expect("#1079 frame read");
                negative_assert!(count > 0, "#1079 frame before EOF");
            }
        })
        .await,
        assertion,
    )
}
fn small_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() < 126, "#1079 small fixture payload");
    let mask = [1, 2, 3, 4];
    let mut wire = vec![0x80 | opcode, 0x80 | payload.len() as u8];
    wire.extend_from_slice(&mask);
    wire.extend(
        payload
            .iter()
            .enumerate()
            .map(|(i, byte)| byte ^ mask[i % 4]),
    );
    wire
}
fn partial() -> Vec<u8> {
    let mut header = vec![0x82, 0xff];
    header.extend_from_slice(&1000u64.to_be_bytes());
    header.extend_from_slice(&[1, 2, 3, 4]);
    header
}
fn hello() -> Value {
    json!({"type":"hello","protocolVersion":"1.2","clientName":"ui","clientVersion":"p","token":"fixture-token"})
}
fn large_text(mut value: Value, length: usize) -> Vec<u8> {
    value["padding"] = json!("");
    let overhead = value.to_string().len();
    assert!(length >= overhead, "#1079 large text has padding room");
    value["padding"] = json!("x".repeat(length - overhead));
    let payload = value.to_string();
    assert_eq!(payload.len(), length, "#1079 exact complete payload length");
    let mut wire = vec![0x81, 0xff];
    wire.extend_from_slice(&(length as u64).to_be_bytes());
    wire.extend_from_slice(&[0; 4]);
    wire.extend_from_slice(payload.as_bytes());
    wire
}
async fn active(requests: &Requests, count: usize) {
    expect_ok(
        timeout(DEADLINE, async {
            while requests.active.load(Ordering::SeqCst) != count {
                tokio::task::yield_now().await;
            }
        })
        .await,
        "#1079 exact business admission barrier",
    );
}
async fn finish(running: Running) {
    let _ = running.stop.send(());
    expect_ok(
        timeout(DEADLINE, running.task).await,
        "#1079 server completion deadline",
    )
    .expect("#1079 server join")
    .expect("#1079 clean server completion");
}

#[test]
fn baseline_provenance() {
    crate::test_baseline::oracle(ORACLE);
}

#[test]
fn baseline_observations() {
    let mut oracle: Value = serde_json::from_str(ORACLE).expect("#1079 oracle JSON");
    if crate::mutants::on("ws-caps-oracle-version") {
        oracle["node"] = Value::Null;
    }
    if crate::mutants::on("ws-caps-oracle-connections") {
        oracle["observations"]["connections"] = Value::Null;
    }
    if crate::mutants::on("ws-caps-oracle-aggregate") {
        oracle["observations"]["aggregate"] = Value::Null;
    }
    if crate::mutants::on("ws-caps-oracle-consumed") {
        oracle["observations"]["consumed"] = Value::Null;
    }
    if crate::mutants::on("ws-caps-oracle-cancelled") {
        oracle["observations"]["cancelled"] = Value::Null;
    }
    if crate::mutants::on("ws-caps-oracle-replacement") {
        oracle["observations"]["replacement"] = Value::Null;
    }
    if crate::mutants::on("ws-caps-oracle-pending-stop") {
        oracle["observations"]["pendingStop"] = Value::Null;
    }
    if crate::mutants::on("ws-caps-oracle-cancelled-stop") {
        oracle["observations"]["cancelledStop"] = Value::Null;
    }
    if crate::mutants::on("ws-caps-oracle-pending-requests") {
        oracle["observations"]["pendingRequests"] = Value::Null;
    }
    let observed = &oracle["observations"];
    negative_assert_eq!(oracle["node"], "v22.22.0", "#1079 TS version");
    negative_assert_eq!(
        observed["connections"],
        json!({
            "upgraded":65,"accepted":66,"incompleteOpen":true,"closedTransports":0
        }),
        "#1079 TS accepts 65 upgrades beside half handshake"
    );
    negative_assert_eq!(
        observed["aggregate"],
        json!({
            "connections":3,"declaredPayload":104857600,"buffered":283115520,
            "closed":[false,false,false],"timeouts":[0,0,0]
        }),
        "#1079 TS retains 270 MiB without aggregate close"
    );
    negative_assert_eq!(
        observed["consumed"],
        json!({
            "firstBuffered":0,"remainingBuffered":188743680,"closed":false
        }),
        "#1079 TS consumes completed frame"
    );
    negative_assert_eq!(
        observed["cancelled"],
        json!({
            "destroyed":true,"closeNotified":true,"closedTransports":1
        }),
        "#1079 TS observes cancellation"
    );
    negative_assert_eq!(
        observed["replacement"],
        json!({"upgraded":66,"closedTransports":1}),
        "#1079 TS accepts replacement"
    );
    negative_assert_eq!(
        observed["pendingStop"],
        json!({"closed":false,"pendingBuffered":94371840}),
        "#1079 TS stop waits for predecessor"
    );
    negative_assert_eq!(
        observed["cancelledStop"],
        json!({"closed":true}),
        "#1079 TS stop after cancellation"
    );
    negative_assert_eq!(
        json!([
            observed["pendingRequests"],
            observed["completedRequest"],
            observed["closedPendingRequests"],
            observed["completedAfterClose"]
        ]),
        json!([
            {"entered":4,"completed":0,"deliveredBytes":4194304,"buffered":0,"closed":false},
            {"completed":1,"pending":3}, {"closed":true,"completed":1,"pending":3},
            {"completed":4}
        ]),
        "#1079 TS complete requests survive socket close until business completion"
    );
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn regenerates_identically() {
    crate::test_baseline::regenerates_identically("native-ws-caps");
}

#[tokio::test]
async fn connection_cap_covers_handshakes_and_recovers() {
    let running = start(2, 128, Arc::default()).await;
    let mut first = tcp(running.address).await;
    write(&mut first, b"GET /ws HTTP/1.1\r\n").await;
    let second = upgrade(running.address).await;
    remaining(&running.connections, 0, CAP_ASSERT).await;
    let mut excess = tcp(running.address).await;
    let outcome = timeout(DEADLINE, excess.read_u8()).await;
    negative_assert!(
        matches!(outcome, Ok(Err(_))),
        "#1079 excess TCP rejected without handshake"
    );
    drop(first);
    remaining(
        &running.connections,
        1,
        "#1079 EOF releases half handshake slot",
    )
    .await;
    let mut invalid = tcp(running.address).await;
    write(&mut invalid, b"invalid\r\n\r\n").await;
    let mut response = Vec::new();
    expect_ok(
        timeout(DEADLINE, invalid.read_to_end(&mut response)).await,
        "#1079 invalid handshake closes",
    )
    .expect("#1079 invalid response read");
    remaining(
        &running.connections,
        1,
        "#1079 invalid handshake releases slot",
    )
    .await;
    let replacement = upgrade(running.address).await;
    remaining(&running.connections, 0, CAP_ASSERT).await;
    drop(second);
    drop(replacement);
    drop(invalid);
    drop(excess);
    remaining(
        &running.connections,
        2,
        "#1079 closed WS releases connection slots",
    )
    .await;
    finish(running).await;
}

#[test]
fn growth_charges_old_and_new_backing() {
    let budget = Arc::new(Semaphore::new(48));
    let mut first = InputBuffer::new(budget.clone());
    let mut second = InputBuffer::new(budget.clone());
    assert!(first.append(&partial()), "#1079 first header admitted");
    assert!(second.append(&partial()), "#1079 second header admitted");
    negative_assert_eq!(
        budget.available_permits(),
        16,
        "#1079 backing charges span connections"
    );
    // Each old allocation is 16; a 32-byte replacement needs total 64, not 48.
    negative_assert!(
        !first.append(&[0; 3]),
        "#1079 growth counts old and new allocations"
    );
    drop(second);
    negative_assert!(
        first.append(&[0; 3]),
        "#1079 exact allocation budget is admitted"
    );
    negative_assert_eq!(
        budget.available_permits(),
        16,
        "#1079 old allocation refunded after replacement"
    );
    drop(first);
    negative_assert_eq!(
        budget.available_permits(),
        48,
        "#1079 decoder drop refunds backing"
    );
}

#[test]
fn consumed_frames_release_backing_after_tail() {
    let budget = Arc::new(Semaphore::new(16));
    let mut input = InputBuffer::new(budget.clone());
    let wire = small_frame(9, b"a");
    assert!(
        input.append(&[wire.clone(), wire].concat()),
        "#1079 batch allocation"
    );
    let frame = expect_some(
        input
            .decoder
            .next_limited(100)
            .expect("#1079 first frame decode"),
        "#1079 first frame present",
    );
    drop(frame);
    input.reclaim_empty();
    negative_assert_eq!(
        budget.available_permits(),
        0,
        "#1079 tail retains backing charge"
    );
    let frame = expect_some(
        input.decoder.next_limited(100).expect("#1079 tail decode"),
        "#1079 tail frame present",
    );
    drop(frame);
    input.reclaim_empty();
    negative_assert_eq!(
        (budget.available_permits(), input.decoder.buffer.capacity()),
        (16, 0),
        "#1079 empty decoder releases allocation and charge"
    );
    let mut successor = InputBuffer::new(budget.clone());
    negative_assert!(
        successor.append(&partial()),
        "#1079 consumed input budget is reusable"
    );
}

#[test]
fn split_backing_stays_charged_during_growth() {
    for limit in [48, 64] {
        let budget = Arc::new(Semaphore::new(limit));
        let mut input = InputBuffer::new(budget.clone());
        let wire = [small_frame(9, b"x"), partial()].concat();
        assert!(input.append(&wire), "#1079 split fixture allocation");
        let frame = expect_some(
            input
                .decoder
                .next_limited(1000)
                .expect("#1079 split fixture decode"),
            "#1079 split fixture first frame",
        );
        drop(frame);
        input.reclaim_empty();
        negative_assert_eq!(
            budget.available_permits(),
            limit - 32,
            "#1079 split retains full original allocation"
        );
        let admitted = input.append(&[0; 12]);
        negative_assert_eq!(
            admitted,
            limit == 64,
            "#1079 split growth includes original backing"
        );
        negative_assert_eq!(
            budget.available_permits(),
            limit - 32,
            "#1079 split growth refunds only old backing"
        );
        negative_assert_eq!(
            input.decoder.buffer.len(),
            if admitted { 26 } else { 14 },
            "#1079 failed growth preserves tail"
        );
    }
}

#[tokio::test]
async fn aggregate_overload_isolated_from_existing_peers() {
    let running = start(4, 32, Arc::default()).await;
    let mut first = upgrade(running.address).await;
    let mut second = upgrade(running.address).await;
    let mut excess = upgrade(running.address).await;
    write(&mut first, &partial()).await;
    write(&mut second, &partial()).await;
    remaining(&running.input, 0, INPUT_ASSERT).await;
    write(&mut excess, &partial()).await;
    let (opcode, payload) = read_frame(&mut excess, INPUT_ASSERT).await;
    negative_assert_eq!(
        (opcode, payload),
        (
            8,
            [
                1009u16.to_be_bytes().as_slice(),
                b"aggregate input limit exceeded"
            ]
            .concat()
        ),
        "#1079 aggregate rejection uses close1009"
    );
    // The never-completing predecessors still own their original allocations.
    negative_assert_eq!(
        running.input.available_permits(),
        0,
        "#1079 rejection leaves predecessors intact"
    );
    drop(first);
    remaining(&running.input, 16, "#1079 peer EOF returns input budget").await;
    let mut replacement = upgrade(running.address).await;
    write(&mut replacement, &small_frame(9, b"ok")).await;
    negative_assert_eq!(
        read_frame(&mut replacement, "#1079 replacement ping completes").await,
        (10, b"ok".to_vec()),
        "#1079 released aggregate budget serves successor"
    );
    drop(second);
    drop(excess);
    drop(replacement);
    remaining(&running.connections, 4, "#1079 all clients reaped").await;
    remaining(&running.input, 32, "#1079 all input refunded").await;
    finish(running).await;
}

#[tokio::test]
async fn eof_releases_input_before_pending_business() {
    let requests = Arc::new(Requests::default());
    requests.hold.store(true, Ordering::SeqCst);
    let running = start(2, 256, requests.clone()).await;
    let mut client = upgrade(running.address).await;
    let hello = json!({"type":"hello","protocolVersion":"1.2","clientName":"ui","clientVersion":"p","token":"fixture-token"});
    let hello = hello.to_string();
    write(&mut client, &small_frame(1, hello.as_bytes())).await;
    expect_ok(
        timeout(DEADLINE, requests.entered.notified()).await,
        "#1079 business predecessor entered",
    );
    write(&mut client, &partial()).await;
    remaining(
        &running.input,
        240 - hello.len(),
        "#1079 pending business still reads input",
    )
    .await;
    drop(client);
    remaining(
        &running.input,
        256 - hello.len(),
        "#1079 EOF frees input before business drain",
    )
    .await;
    negative_assert_eq!(
        running.connections.available_permits(),
        1,
        "#1079 pending business retains connection slot"
    );
    let mut successor = upgrade(running.address).await;
    write(&mut successor, &small_frame(9, b"x")).await;
    negative_assert_eq!(
        read_frame(&mut successor, "#1079 successor independent of predecessor").await,
        (10, b"x".to_vec()),
        "#1079 pending predecessor does not block successor"
    );
    requests.release.notify_one();
    remaining(
        &running.input,
        256,
        "#1079 completed business returns input",
    )
    .await;
    drop(successor);
    remaining(
        &running.connections,
        2,
        "#1079 released business completes cleanup",
    )
    .await;
    finish(running).await;
}

#[tokio::test]
async fn complete_large_requests_share_aggregate_budget() {
    // Leave room for the contender's old 512 KiB + new 1 MiB decoder
    // backing, but not for its additional ~1 MiB queued String.
    const LIMIT: usize = 9 * 512 * 1024;
    const PAYLOAD: usize = 1024 * 1024 - 14;
    let requests = Arc::new(Requests::default());
    requests.hold.store(true, Ordering::SeqCst);
    let running = start(3, LIMIT, requests.clone()).await;
    let mut first = upgrade(running.address).await;
    write(&mut first, &large_text(hello(), PAYLOAD)).await;
    active(&requests, 1).await;
    for index in 1..3 {
        write(
            &mut first,
            &large_text(
                json!({"type":"request","id":index.to_string(),
            "method":"describe_server","params":{}}),
                PAYLOAD,
            ),
        )
        .await;
        active(&requests, index + 1).await;
    }
    // Ping is a decode-loop barrier: all three complete frames have left the
    // decoder, yet their business jobs deliberately never finish before release.
    write(&mut first, &small_frame(9, b"held")).await;
    negative_assert_eq!(
        read_frame(&mut first, "#1079 held requests allow ping").await,
        (10, b"held".to_vec()),
        "#1079 business suspension does not stop input"
    );
    negative_assert_eq!(
        running.input.available_permits(),
        LIMIT - 3 * PAYLOAD,
        "#1079 complete requests retain aggregate charge"
    );
    let mut contender = upgrade(running.address).await;
    write(&mut contender, &large_text(hello(), PAYLOAD)).await;
    negative_assert_eq!(
        read_frame(&mut contender, "#1079 pending aggregate rejects contender").await,
        (
            8,
            [
                1009u16.to_be_bytes().as_slice(),
                b"aggregate input limit exceeded"
            ]
            .concat()
        ),
        "#1079 pending aggregate rejection uses close1009"
    );
    negative_assert_eq!(
        requests.active.load(Ordering::SeqCst),
        3,
        "#1079 over-budget request never enters host"
    );
    remaining(
        &running.input,
        LIMIT - 3 * PAYLOAD,
        "#1079 request rejection leaves predecessor leases",
    )
    .await;
    requests.hold.store(false, Ordering::SeqCst);
    requests.release.notify_waiters();
    active(&requests, 0).await;
    remaining(
        &running.input,
        LIMIT,
        "#1079 completed requests refund aggregate budget",
    )
    .await;
    drop(first);
    drop(contender);
    finish(running).await;
}

#[tokio::test]
async fn completed_request_refunds_before_waiter_reap() {
    let requests = Arc::new(Requests::default());
    requests.hold.store(true, Ordering::SeqCst);
    let host = native_ws_fixture(requests.clone());
    let (text, _outgoing, _closing) = Text::channel();
    let connection = host.connect(text, "fixture-token".into(), "probe".into());
    let budget = Arc::new(Semaphore::new(512));
    let (message, permit) = expect_some(
        request_text(hello().to_string().as_bytes(), &budget),
        "#1079 request text admitted",
    );
    let length = message.len();
    let completion = expect_ok(
        expect_ok(
            timeout(
                DEADLINE,
                connection.enqueue_with_input_permit(message, Some(permit)),
            )
            .await,
            "#1079 completion fixture admission deadline",
        ),
        "#1079 completion fixture admission",
    );
    active(&requests, 1).await;
    negative_assert_eq!(
        budget.available_permits(),
        512 - length,
        "#1079 host owns pending request lease"
    );
    requests.release.notify_one();
    // Never poll completion before checking the host itself has refunded.
    remaining(
        &budget,
        512,
        "#1079 host completion refunds without waiter poll",
    )
    .await;
    expect_ok(
        expect_ok(
            timeout(DEADLINE, completion).await,
            "#1079 completed waiter deadline",
        ),
        "#1079 completed waiter succeeds",
    );
    expect_ok(
        expect_ok(
            timeout(DEADLINE, host.shutdown(DEADLINE)).await,
            "#1079 completed host shutdown deadline",
        ),
        "#1079 completed host shuts down",
    );
}

#[tokio::test]
async fn pending_requests_release_on_shutdown_and_abort() {
    for abort in [false, true] {
        let requests = Arc::new(Requests::default());
        requests.hold.store(true, Ordering::SeqCst);
        let running = start(1, 512, requests.clone()).await;
        let mut client = upgrade(running.address).await;
        let message = hello().to_string();
        write(&mut client, &small_frame(1, message.as_bytes())).await;
        active(&requests, 1).await;
        remaining(
            &running.input,
            512 - message.len(),
            "#1079 cancellation fixture charged",
        )
        .await;
        if abort {
            running.task.abort();
        } else {
            let _ = running.stop.send(());
        }
        let result = expect_ok(
            timeout(DEADLINE, running.task).await,
            "#1079 pending server shutdown deadline",
        );
        if abort {
            negative_assert!(
                result.is_err_and(|error| error.is_cancelled()),
                "#1079 pending run abort reported"
            );
        } else {
            negative_assert!(
                expect_ok(result, "#1079 pending shutdown join").is_err(),
                "#1079 never completing business has bounded drain"
            );
        }
        remaining(
            &running.input,
            512,
            "#1079 cancelled host returns request input",
        )
        .await;
        drop(client);
    }
}

#[test]
fn lossy_request_reservation_matches_node_bytes() {
    let mut oracle: Value = serde_json::from_str(ORACLE).expect("#1079 lossy oracle JSON");
    if crate::mutants::on("ws-caps-lossy-empty-samples") {
        oracle["observations"]["lossyText"] = json!([]);
    }
    let samples = oracle["observations"]["lossyText"]
        .as_array()
        .expect("#1079 lossy samples");
    negative_assert_eq!(
        samples
            .iter()
            .map(|sample| sample["bytes"].clone())
            .collect::<Vec<_>>(),
        vec![
            json!([97, 255, 255, 98]),
            json!([240, 159, 146]),
            json!([226, 130, 172]),
            json!([237, 160, 128])
        ],
        "#1079 lossy oracle covers fixed valid and invalid sequences"
    );
    for sample in samples {
        let payload: Vec<u8> = sample["bytes"]
            .as_array()
            .expect("#1079 lossy bytes")
            .iter()
            .map(|byte| byte.as_u64().expect("#1079 lossy byte integer") as u8)
            .collect();
        let expected = sample["text"].as_str().expect("#1079 lossy text");
        let length = sample["utf8Bytes"].as_u64().expect("#1079 lossy length") as usize;
        let budget = Arc::new(Semaphore::new(length));
        let (text, permit) = expect_some(
            request_text(&payload, &budget),
            "#1079 exact lossy reservation admitted",
        );
        negative_assert_eq!(text, expected, "#1079 lossy conversion matches Node");
        negative_assert_eq!(
            budget.available_permits(),
            0,
            "#1079 lossy bytes charged after replacement"
        );
        drop(text);
        drop(permit);
        negative_assert!(
            request_text(&payload, &Arc::new(Semaphore::new(length - 1))).is_none(),
            "#1079 lossy over-budget rejected before allocation"
        );
    }
}

#[tokio::test]
async fn protocol_error_releases_input() {
    let running = start(1, 32, Arc::default()).await;
    let mut client = upgrade(running.address).await;
    write(&mut client, &partial()[..1]).await;
    remaining(&running.input, 31, "#1079 partial header charged").await;
    write(&mut client, &[0]).await; // unmasked frame completes the invalid header
    remaining(&running.input, 32, "#1079 decoder error refunds input").await;
    remaining(
        &running.connections,
        1,
        "#1079 decoder error releases connection",
    )
    .await;
    drop(client);
    finish(running).await;
}

#[tokio::test]
async fn shutdown_and_abort_release_resources() {
    for abort in [false, true] {
        let running = start(2, 32, Arc::default()).await;
        let half = tcp(running.address).await;
        let mut client = upgrade(running.address).await;
        write(&mut client, &partial()).await;
        remaining(&running.input, 16, "#1079 shutdown fixture holds input").await;
        remaining(
            &running.connections,
            0,
            "#1079 shutdown fixture holds connections",
        )
        .await;
        if abort {
            running.task.abort();
        } else {
            let _ = running.stop.send(());
        }
        let result = expect_ok(timeout(DEADLINE, running.task).await, CLEANUP_ASSERT);
        if abort {
            negative_assert!(
                result.is_err_and(|error| error.is_cancelled()),
                "#1079 run cancellation reported"
            );
        } else {
            negative_assert!(
                result.expect("#1079 drain task joins").is_err(),
                "#1079 drain timeout reported"
            );
        }
        remaining(&running.input, 32, CLEANUP_ASSERT).await;
        remaining(&running.connections, 2, CLEANUP_ASSERT).await;
        drop(half);
        drop(client);
    }
}

#[tokio::test]
async fn completed_records_reaped_before_next_accept() {
    let running = start(1, 32, Arc::default()).await;
    for _ in 0..4 {
        let client = upgrade(running.address).await;
        remaining(&running.connections, 0, "#1079 churn connection accepted").await;
        drop(client);
        remaining(&running.connections, 1, "#1079 churn connection finished").await;
    }
    negative_assert_eq!(
        running.task_high_watermark.load(Ordering::SeqCst),
        1,
        "#1079 completed connection records stay within cap"
    );
    finish(running).await;
}

#[tokio::test]
async fn defaults_are_applied_to_bound_server() {
    let host = native_ws_fixture(Arc::new(Requests::default()));
    let server = NativeWsServer::bind(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        host.clone(),
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#1079 default bind");
    negative_assert_eq!(
        (
            server.resources.connections.available_permits(),
            server.resources.input.available_permits()
        ),
        (64, 256 * 1024 * 1024),
        "#1079 default bind applies both resource limits"
    );
    drop(server);
    expect_ok(
        timeout(DEADLINE, host.shutdown(DEADLINE)).await,
        "#1079 default fixture shutdown deadline",
    )
    .expect("#1079 default fixture host shutdown");
}
