//! #1344 stub-contract gold master replay: the 36 scenarios recorded by
//! `tests/fixtures/channel-slack/generate.mts` against the real TS baseline
//! (Node v22.22.0, baseline cc7c4e6) behind the committed seven-member
//! `@slack/bolt` stub, re-driven through the native port with the same
//! loopback fake — one port serving both the Socket Mode REST endpoints and
//! the WebSocket upgrade, like the generator's own mock (`startMock` in
//! `generate.mts`).
//!
//! No `maskDynamic`: the mapper uuid is the fixed seam, the ts values are
//! event literals, so all four record streams (`requests` / `ws` / `rpcs` /
//! `logs`) compare byte for byte.
//!
//! The 36 cases run strictly sequentially inside one test (the 16 replay
//! ports round-robin), so a parallel `--test-threads` run cannot collide two
//! cases onto one listener.
use super::*;
use crate::channel::protocol::ChannelStartDeps;
use crate::channel::sms::{
    ElicitationResponse, PermissionDecisionOutcome, SmsGateway, SubmitTurnRequest, TurnEventStream,
};
use base64::Engine as _;
use serde_json::{Map as JsonMap, Value, json};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;

const ORACLE: &str = include_str!("../../../tests/fixtures/channel-slack/oracle.json");
const EXPECTED_CASE_COUNT: usize = 36;
/// The fixed mapper uuid the generator injects, so `/new` session keys match.
const MAPPER_UUID: &str = "aaaa1111-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const BOT_TOKEN: &str = "xoxb-1";
const APP_TOKEN: &str = "xoxa-1";
const USER_ID: &str = "U123";
const BOT_NAME: &str = "slackbot";
/// The accept-key salt from RFC 6455, the same bytes `generate.mts` uses.
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// The generator's 20 s wait budget; every blocking step is bounded so a
/// regression fails instead of hanging the suite.
const DEADLINE: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// oracle access and JS value conversion (the sms #1339 shapes)
// ---------------------------------------------------------------------------

fn oracle() -> Value {
    crate::test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1344 oracle JSON")
}

fn cases(oracle: &Value) -> &Vec<Value> {
    let cases = oracle["cases"].as_array().expect("#1344 oracle cases");
    crate::negative_assert!(!cases.is_empty(), "#1344 oracle cases non-empty");
    crate::negative_assert_eq!(
        cases.len() as u64,
        oracle["expectedCaseCount"]
            .as_u64()
            .expect("#1344 expectedCaseCount"),
        "#1344 oracle case count matches its declaration"
    );
    // The declaration alone is not a ruler: dropping a case and lowering
    // `expectedCaseCount` together would keep the assertion above green. The
    // generator's fixed count is pinned separately (`EXPECTED_CASE_COUNT`
    // in `generate.mts`), so coverage cannot shrink silently.
    crate::negative_assert_eq!(
        cases.len() as u64,
        EXPECTED_CASE_COUNT as u64,
        "#1344 oracle holds the generator's 36 cases"
    );
    cases
}

fn to_js(value: &Value) -> JsValue {
    match value {
        Value::Null => JsValue::Null,
        Value::Bool(flag) => JsValue::Bool(*flag),
        Value::Number(number) => JsValue::Number(number.as_f64().unwrap_or(f64::NAN)),
        Value::String(text) => JsValue::String(JsString::from(text.as_str())),
        Value::Array(items) => JsValue::Array(items.iter().map(to_js).collect()),
        Value::Object(_) => JsValue::Object(to_js_object(value)),
    }
}

fn to_js_object(value: &Value) -> JsObject {
    let mut object = JsObject::new();
    for (key, item) in value.as_object().into_iter().flatten() {
        object.insert(JsString::from(key.as_str()), to_js(item));
    }
    object
}

/// The record side of `JSON.stringify`: undefined object values are omitted,
/// undefined array items become null, non-finite numbers become null.
fn js_to_json(value: &JsValue) -> Value {
    match value {
        JsValue::Undefined | JsValue::Null => Value::Null,
        JsValue::Bool(flag) => Value::Bool(*flag),
        JsValue::Number(number) => serde_json::Number::from_f64(*number)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        JsValue::String(text) => Value::String(String::from_utf16_lossy(text.units())),
        JsValue::Array(items) => Value::Array(items.iter().map(js_to_json).collect()),
        JsValue::Object(object) => {
            let mut map = JsonMap::new();
            for (key, value) in object.own_entries() {
                if matches!(value, JsValue::Undefined) {
                    continue;
                }
                map.insert(String::from_utf16_lossy(key.units()), js_to_json(value));
            }
            Value::Object(map)
        }
    }
}

// ---------------------------------------------------------------------------
// the record, gates and bounded waits
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Record {
    requests: Vec<Value>,
    ws: Vec<Value>,
    rpcs: Vec<Value>,
    logs: Vec<(String, String)>,
}

fn snapshot(record: &Arc<SyncMutex<Record>>) -> Value {
    let record = record.lock().expect("#1344 record mutex");
    json!({
        "requests": record.requests,
        "ws": record.ws,
        "rpcs": record.rpcs,
        "logs": record
            .logs
            .iter()
            .map(|(level, message)| json!([level, message]))
            .collect::<Vec<_>>(),
    })
}

type Gate = Arc<tokio::sync::watch::Sender<bool>>;
type Gates = Arc<SyncMutex<HashMap<String, Gate>>>;

fn gate(gates: &Gates, name: &str) -> Gate {
    gates
        .lock()
        .expect("#1344 gates mutex")
        .entry(name.to_owned())
        .or_insert_with(|| Arc::new(tokio::sync::watch::Sender::new(false)))
        .clone()
}

async fn released(gate: Gate) {
    let mut receiver = gate.subscribe();
    while !*receiver.borrow_and_update() {
        receiver
            .changed()
            .await
            .expect("#1344 gate sender retained");
    }
}

/// The TS mock's `gates.get(name)?.release()` — a no-op when the gate was
/// never awaited into existence.
fn release_if_exists(gates: &Gates, name: &str) {
    if let Some(gate) = gates.lock().expect("#1344 gates mutex").get(name) {
        let _ = gate.send(true);
    }
}

async fn bounded<T>(label: &str, future: impl std::future::Future<Output = T>) -> T {
    match timeout(DEADLINE, future).await {
        Ok(value) => value,
        // Route through the receipt emitter: a plain panic yields no receipt
        // and the host matrix reports INVALID instead of KILLED.
        Err(_) => crate::test_negative_receipt::expect_some(None, label),
    }
}

/// Wait for `predicate`, emitting `assertion` when the deadline passes. The
/// assertion must be one a mutation row declares (a case's cell marker), so a
/// mutant that never converges is judged KILLED, not INVALID.
async fn until(assertion: &str, detail: &str, predicate: impl Fn() -> bool) {
    let started = Instant::now();
    while !predicate() {
        if started.elapsed() >= DEADLINE {
            eprintln!("#1344 timeout waiting for {detail}");
        }
        crate::negative_assert!(started.elapsed() < DEADLINE, "{assertion}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// The generator's `ticks(n)`: n event-loop turns. Each turn also parks the
/// single-threaded test runtime on a 1 ms timer, which is what lets the mock
/// and channel tasks run their IO between the steps.
async fn ticks(count: u64) {
    for _ in 0..count {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

// ---------------------------------------------------------------------------
// the loopback mock: Socket Mode REST + WebSocket on one port
// (`startMock` / `attachFrameReader` / `stopMock` from the generator)
// ---------------------------------------------------------------------------

type MockSocket = Arc<SyncMutex<OwnedWriteHalf>>;

struct Mock {
    sockets: Arc<SyncMutex<Vec<MockSocket>>>,
    /// Set once the case teardown starts: frames recorded after it are the
    /// teardown's own, never the case's.
    closing: Arc<AtomicBool>,
    scripted: Arc<SyncMutex<VecDeque<(u16, String)>>>,
    tasks: SyncMutex<Vec<tokio::task::JoinHandle<()>>>,
}

async fn start_mock(
    port: u16,
    record: &Arc<SyncMutex<Record>>,
    scripted: VecDeque<(u16, String)>,
) -> Arc<Mock> {
    // `SO_REUSEADDR`, like the Node listener: the 16 ports round-robin over
    // the 36 cases, and the previous case's listener may still sit in
    // TIME_WAIT when the next one binds.
    let socket = tokio::net::TcpSocket::new_v4().expect("#1344 mock socket");
    socket.set_reuseaddr(true).expect("#1344 mock reuseaddr");
    socket
        .bind(std::net::SocketAddr::from(([127, 0, 0, 1], port)))
        .expect("#1344 mock bind");
    let listener = socket.listen(64).expect("#1344 mock listen");
    let mock = Arc::new(Mock {
        sockets: Arc::default(),
        closing: Arc::new(AtomicBool::new(false)),
        scripted: Arc::new(SyncMutex::new(scripted)),
        tasks: SyncMutex::new(Vec::new()),
    });
    let accept_mock = mock.clone();
    let accept_record = record.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let mock = accept_mock.clone();
            let record = accept_record.clone();
            let task = tokio::spawn(async move {
                serve_connection(stream, mock, record).await;
            });
            accept_mock
                .tasks
                .lock()
                .expect("#1344 tasks mutex")
                .push(task);
        }
    });
    mock.tasks.lock().expect("#1344 tasks mutex").push(task);
    mock
}

/// The teardown's server-side close(1000) frame.
const TEARDOWN_CLOSE: [u8; 4] = [0x88, 0x02, 0x03, 0xe8];

/// `stopMock`: frames recorded after this are ignored, every surviving
/// socket gets the server-side close(1000) (a failed start never stops its
/// own socket), and the listener plus every connection task retire.
fn stop_mock(mock: &Arc<Mock>) {
    mock.closing.store(true, Ordering::SeqCst);
    let sockets: Vec<MockSocket> = mock
        .sockets
        .lock()
        .expect("#1344 sockets mutex")
        .drain(..)
        .collect();
    for socket in sockets {
        let _ = socket
            .lock()
            .expect("#1344 socket mutex")
            .try_write(&TEARDOWN_CLOSE);
    }
    let tasks: Vec<_> = mock
        .tasks
        .lock()
        .expect("#1344 tasks mutex")
        .drain(..)
        .collect();
    for task in tasks {
        task.abort();
    }
}

/// One connection: either a single HTTP request (the client always speaks
/// `Connection: close`) or the WebSocket upgrade plus its frame reader.
async fn serve_connection(mut stream: TcpStream, mock: Arc<Mock>, record: Arc<SyncMutex<Record>>) {
    let mut buffer: Vec<u8> = Vec::new();
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        let mut chunk = [0u8; 4096];
        let read = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();
    let mut content_length = 0usize;
    let mut content_type: Option<String> = None;
    let mut websocket_key: Option<String> = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            match name.to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.trim().parse().unwrap_or(0),
                "content-type" => content_type = Some(value.trim().to_owned()),
                "sec-websocket-key" => websocket_key = Some(value.trim().to_owned()),
                _ => {}
            }
        }
    }

    if method == "GET" {
        // The WebSocket upgrade: accept with the SHA-1 of key + GUID.
        let key = websocket_key.unwrap_or_default();
        let accept = base64::engine::general_purpose::STANDARD
            .encode(Sha1::digest(format!("{key}{WEBSOCKET_GUID}")));
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        if stream.try_write(response.as_bytes()).is_err() {
            return;
        }
        record
            .lock()
            .expect("#1344 record mutex")
            .ws
            .push(json!({"event": "open"}));
        let (read_half, write_half) = stream.into_split();
        let socket: MockSocket = Arc::new(SyncMutex::new(write_half));
        mock.sockets
            .lock()
            .expect("#1344 sockets mutex")
            .push(socket.clone());
        read_frames(read_half, socket, mock, record).await;
        return;
    }

    // The request body: exactly `content-length` bytes after the head.
    let body_start = head_end + 4;
    while buffer.len() < body_start + content_length {
        let mut chunk = [0u8; 8192];
        let read = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&buffer[body_start..body_start + content_length]);
    {
        let mut record = record.lock().expect("#1344 record mutex");
        record.requests.push(json!({
            "method": method,
            "path": path,
            "contentType": content_type,
            "body": body,
        }));
    }
    let port = stream
        .local_addr()
        .map(|address| address.port())
        .unwrap_or_default();
    let (status, payload) = mock
        .scripted
        .lock()
        .expect("#1344 scripted mutex")
        .pop_front()
        .unwrap_or_else(|| fallback_response(&path, port));
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.try_write(response.as_bytes());
}

/// The generator's per-path fallback: the connections.open URL points back at
/// this same port, so a queued `ok:false` start still opened its socket.
fn fallback_response(path: &str, port: u16) -> (u16, String) {
    if path == "/api/apps.connections.open" {
        (
            200,
            json!({"ok": true, "url": format!("ws://127.0.0.1:{port}/link")}).to_string(),
        )
    } else if path == "/api/auth.test" {
        (
            200,
            json!({"ok": true, "user_id": USER_ID, "user": BOT_NAME}).to_string(),
        )
    } else {
        (200, json!({"ok": true}).to_string())
    }
}

/// `attachFrameReader`: the client close handshake records the code and is
/// echoed; pings are answered; text frames from the client are not part of
/// the contract surface. The socket leaves the list only through the close
/// handshake — an EOF alone keeps it (a `ws-send` on a dead socket is
/// dropped silently, exactly like a write to a destroyed Node socket).
async fn read_frames(
    mut stream: OwnedReadHalf,
    socket: MockSocket,
    mock: Arc<Mock>,
    record: Arc<SyncMutex<Record>>,
) {
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let mut chunk = [0u8; 4096];
        let read = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        pending.extend_from_slice(&chunk[..read]);
        while let Some((opcode, payload, consumed)) = parse_frame(&pending) {
            pending.drain(..consumed);
            if mock.closing.load(Ordering::SeqCst) {
                continue;
            }
            match opcode {
                0x8 => {
                    let code = if payload.len() >= 2 {
                        u16::from_be_bytes([payload[0], payload[1]])
                    } else {
                        1005
                    };
                    record
                        .lock()
                        .expect("#1344 record mutex")
                        .ws
                        .push(json!({"event": "close", "code": code}));
                    let echo = [0x88, 0x02, (code >> 8) as u8, (code & 0xff) as u8];
                    let _ = socket.lock().expect("#1344 socket mutex").try_write(&echo);
                    mock.sockets
                        .lock()
                        .expect("#1344 sockets mutex")
                        .retain(|entry| !Arc::ptr_eq(entry, &socket));
                }
                0x9 => {
                    let mut pong = Vec::with_capacity(2 + payload.len());
                    pong.extend_from_slice(&[0x8a, payload.len() as u8]);
                    pong.extend_from_slice(&payload);
                    let _ = socket.lock().expect("#1344 socket mutex").try_write(&pong);
                }
                _ => {}
            }
        }
    }
}

/// Parse one (possibly masked) client frame off `buffer`; `None` while the
/// frame is incomplete. The payload is returned unmasked.
fn parse_frame(buffer: &[u8]) -> Option<(u8, Vec<u8>, usize)> {
    if buffer.len() < 2 {
        return None;
    }
    let opcode = buffer[0] & 0x0f;
    let masked = (buffer[1] & 0x80) != 0;
    let mut length = (buffer[1] & 0x7f) as usize;
    let mut offset = 2;
    if length == 126 {
        if buffer.len() < 4 {
            return None;
        }
        length = u16::from_be_bytes([buffer[2], buffer[3]]) as usize;
        offset = 4;
    } else if length == 127 {
        if buffer.len() < 10 {
            return None;
        }
        length = u64::from_be_bytes(buffer[2..10].try_into().expect("#1344 u64 bytes")) as usize;
        offset = 10;
    }
    let mask_offset = offset;
    if masked {
        offset += 4;
    }
    if buffer.len() < offset + length {
        return None;
    }
    let mut payload = buffer[offset..offset + length].to_vec();
    if masked {
        for (index, unit) in payload.iter_mut().enumerate() {
            *unit ^= buffer[mask_offset + (index & 3)];
        }
    }
    Some((opcode, payload, offset + length))
}

/// The generator's `writeTextFrame`: an unmasked server text frame.
fn write_text_frame(socket: &MockSocket, text: &str) {
    let payload = text.as_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 10);
    if payload.len() < 126 {
        frame.extend_from_slice(&[0x81, payload.len() as u8]);
    } else if payload.len() < 65536 {
        frame.extend_from_slice(&[0x81, 126]);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        frame.extend_from_slice(&[0x81, 127]);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    let _ = socket.lock().expect("#1344 socket mutex").try_write(&frame);
}

// ---------------------------------------------------------------------------
// the fake A7 gateway (`makeGateway` from the generator)
// ---------------------------------------------------------------------------

/// The generator's `submitTurn` fake: the per-turn script is consumed in
/// order, the rpc record lands before any gate, and the stream stays open
/// past the elicitation/permission event until the matching answer rpc.
struct MockGateway {
    turns: Vec<Value>,
    next: SyncMutex<usize>,
    record: Arc<SyncMutex<Record>>,
    gates: Gates,
}

impl SmsGateway for MockGateway {
    fn submit_turn(
        &self,
        request: SubmitTurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let turns = self.turns.clone();
        let index = {
            let mut next = self.next.lock().expect("#1344 turn index");
            let index = *next;
            *next += 1;
            index
        };
        let record = self.record.clone();
        let gates = self.gates.clone();
        Box::pin(async move {
            {
                let mut record = record.lock().expect("#1344 record mutex");
                record.rpcs.push(json!({
                    "rpc": "submitTurn",
                    "request": js_to_json(&JsValue::Object(request.to_js())),
                }));
            }
            let turn = turns.get(index).cloned().unwrap_or(Value::Null);
            if let Some(gate_name) = turn["gate"].as_str() {
                released(gate(&gates, gate_name)).await;
            }
            if let Some(error) = turn["error"].as_str() {
                return Err(ChannelError::Error(error.to_owned()));
            }
            let events: VecDeque<JsValue> = turn["events"]
                .as_array()
                .map(|events| events.iter().map(to_js).collect())
                .unwrap_or_default();
            let hold = turn["holdAfter"].as_str().map(str::to_owned);
            Ok(Box::new(MockTurnStream {
                events,
                hold,
                gates,
            }) as Box<dyn TurnEventStream>)
        })
    }

    fn respond_elicitation(
        &self,
        response: ElicitationResponse,
    ) -> BoxFuture<Result<(), ChannelError>> {
        let record = self.record.clone();
        let gates = self.gates.clone();
        Box::pin(async move {
            {
                let mut record = record.lock().expect("#1344 record mutex");
                record.rpcs.push(json!({
                    "rpc": "respondElicitation",
                    "response": js_to_json(&JsValue::Object(response.to_js())),
                }));
            }
            release_if_exists(&gates, "elicited");
            // The released turn continuation runs before this reply's caller
            // continues, like the TS microtask order.
            tokio::task::yield_now().await;
            Ok(())
        })
    }

    fn permission_decide(
        &self,
        decision: PermissionDecision,
    ) -> BoxFuture<Result<PermissionDecisionOutcome, ChannelError>> {
        let record = self.record.clone();
        let gates = self.gates.clone();
        Box::pin(async move {
            {
                let mut record = record.lock().expect("#1344 record mutex");
                record.rpcs.push(json!({
                    "rpc": "permissionDecide",
                    "decision": js_to_json(&JsValue::Object(decision.to_js())),
                }));
            }
            // The `await setImmediate` boundary before the release, then one
            // more yield so the resumed turn lands before this reply's
            // caller sends the confirmation.
            tokio::task::yield_now().await;
            release_if_exists(&gates, "decided");
            tokio::task::yield_now().await;
            Ok(PermissionDecisionOutcome { delivered: true })
        })
    }
}

/// The generator's `submitTurn` async generator: events first, then the
/// `holdAfter` park before completion.
struct MockTurnStream {
    events: VecDeque<JsValue>,
    hold: Option<String>,
    gates: Gates,
}

impl TurnEventStream for MockTurnStream {
    fn next_event(&mut self) -> BoxFuture<Option<Result<JsValue, ChannelError>>> {
        if let Some(event) = self.events.pop_front() {
            return Box::pin(async move { Some(Ok(event)) });
        }
        let hold = self.hold.take();
        let gates = self.gates.clone();
        Box::pin(async move {
            match hold.as_deref() {
                Some("elicit") => released(gate(&gates, "elicited")).await,
                Some("decide") => released(gate(&gates, "decided")).await,
                _ => {}
            }
            None
        })
    }
}

// ---------------------------------------------------------------------------
// the channel rig
// ---------------------------------------------------------------------------

/// `loadChannel` from the generator: a fresh channel whose mapper is pinned
/// to the fixed uuid and whose REST base is this case's mock port. `without`
/// drops the named token option, leaving the env fallback empty.
fn load_channel(port: u16, without: &[String], gateway: GatewayRef) -> SlackChannel {
    let drops = |name: &str| without.iter().any(|item| item == name);
    SlackChannel::new(SlackChannelOptions {
        bot_token: (!drops("botToken")).then(|| JsValue::String(JsString::from(BOT_TOKEN))),
        app_token: (!drops("appToken")).then(|| JsValue::String(JsString::from(APP_TOKEN))),
        mapper: Some(SlackSessionMapper::new(
            SlackSessionMapperState::default(),
            Box::new(|| JsString::from(MAPPER_UUID)),
        )),
        gateway: Some(gateway),
        env: None,
        base_url: Some(JsString::from(format!("http://127.0.0.1:{port}"))),
    })
}

fn case_deps(record: &Arc<SyncMutex<Record>>) -> ChannelStartDeps {
    let logger = record.clone();
    ChannelStartDeps {
        config: None,
        report_status: None,
        log: Some(Arc::new(move |level, message| {
            logger
                .lock()
                .expect("#1344 record mutex")
                .logs
                .push((level.to_owned(), message.to_owned()));
        })),
        turn_gateway: None,
    }
}

/// The replay and the `#[ignore]` regeneration share the host-global
/// 18183–18198 port segment (the generator's per-case rotation); this lock
/// serializes the two tests within one binary. It is a tokio mutex because
/// the replay holds it across its awaits (clippy `await_holding_lock`
/// rejects a `std::sync` guard there).
static FIXTURE_PORT_SEGMENT: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// The generator's per-case port rotation is host-global; a port held by
/// anything else means the replay would measure the holder, not the channel.
async fn ensure_replay_port_free(port: u16) {
    if bounded(
        "#1344 the replay port is free for this case",
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .is_ok()
    {
        panic!("#1344 replay port {port} is already in use");
    }
}

// ---------------------------------------------------------------------------
// #2322: the host-level half of the port-segment serialization
// ---------------------------------------------------------------------------

/// `/tmp/pdrs-channel-slack.lock` in the canonical `cli-parity` form
/// (`scripts/cli-parity/README.md`「锁被保留时怎么解」）： a `.guard` flock
/// serializes the takers, the lock directory itself carries `owner.json`
/// (`pid` / `timestamp` / `token` / `cleanupPending`, published tmp+rename),
/// and a leftover directory is reclaimed only per the canonical rules — the
/// owner pid is dead (regardless of age) or the lease is over the 2400 s
/// takeover bound, never while `cleanupPending` is truthy. Contention waits,
/// never fails: two `cargo test` processes on one host serialize instead of
/// the later one tripping `ensure_replay_port_free` as a false FAIL.
const HOST_LOCK_DIR: &str = "/tmp/pdrs-channel-slack.lock";
const HOST_LOCK_GUARD: &str = "/tmp/pdrs-channel-slack.lock.guard";
/// The canonical takeover bound (`MAX_AGE`); the port replay is bounded far
/// below it, so this only fires on a wedged holder.
const HOST_LOCK_MAX_AGE: Duration = Duration::from_secs(2400);
/// The bounded retry a release spends regaining the guard before it retains
/// the lease fail-closed (the same bound the in-tree `release_guard`
/// precedent uses in `sdk_sessions_list_exit.rs`).
const HOST_LOCK_RELEASE_RETRY: Duration = Duration::from_secs(2);
/// The wait bound for a would-be taker. A lease governed by the takeover
/// rules — a live holder that releases, a dead holder (every poll reads its
/// pid dead), or an age past `HOST_LOCK_MAX_AGE` — always resolves within
/// `HOST_LOCK_MAX_AGE` of the wait's start (the lease ages as the wait
/// advances), so this deadline is only ever tripped by a lease the rules
/// cannot resolve — a future-dated `timestamp`, a truthy `cleanupPending`
/// (the rules never reclaim it), or a foreign lease no rule owns — and it
/// fails by naming the manual recovery section instead of hanging the test
/// process. Derived from the max age (`Duration::as_secs` /
/// `from_secs` are const), so the two bounds cannot drift apart; the
/// canonical gateway side raises `TimeoutError` rather than wait forever
/// (`scripts/gateway-parity/common.py`).
const HOST_LOCK_WAIT_LIMIT: Duration = Duration::from_secs(HOST_LOCK_MAX_AGE.as_secs() + 600);
/// The manual recovery section every #2322 retention printout and lock
/// failure names.
const HOST_LOCK_RECOVERY: &str = "scripts/cli-parity/README.md 「锁被保留时怎么解」";

/// The host lease. The guard flock is held only for the take (the canonical
/// short-lived-helper shape); the directory is the lease across the replay.
struct HostLease {
    dir: std::path::PathBuf,
    token: String,
}

impl Drop for HostLease {
    fn drop(&mut self) {
        // The canonical release: re-take the guard with a bounded retry
        // (the in-tree `release_guard` precedent — never stolen), verify
        // the lease is still ours (a takeover could have renamed ours
        // away), then rename the directory off the lock path and reap it —
        // the whole release inside the guard's critical section, as the
        // canonical release holds it. A release that cannot regain the
        // guard within `HOST_LOCK_RELEASE_RETRY` retains fail-closed, like
        // the canonical release's own `Err` on `release_guard`: the
        // `still_ours` read and the rename are only atomic under the
        // guard, so a guard-less rename could instead move a fresh lease
        // another taker just published onto the lock path (a stale holder
        // dropping while its own lease is being taken over), leaving the
        // path empty for a third taker — two replays on one port segment.
        // Retention loses no liveness either: the r1 worry (a short
        // taker's critical section straddling the drop) is exactly what
        // the bounded retry covers, and a lease that outlives the retry is
        // resolved the canonical way — by the takeover rules (a dead pid
        // or an age past `HOST_LOCK_MAX_AGE`) or by the manual recovery
        // section the retention printout names.
        let guard = release_host_guard();
        let still_ours = std::fs::read(self.dir.join("owner.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|owner| {
                owner["token"]
                    .as_str()
                    .is_some_and(|token| token == self.token)
                    .then_some(owner)
            });
        if still_ours.is_none() {
            return; // a takeover renamed ours away; nothing of ours to release
        }
        let Some(_held) = guard else {
            // Fail-closed retention: the lease stays put on the lock
            // path — whole and visible to the takeover rules, `owner.json`
            // intact — and the printout names the recovery section, never
            // a silent fork. Nothing is renamed or removed without the
            // guard.
            eprintln!(
                "#2322 port segment lease {HOST_LOCK_DIR} retained on the lock \
                 path: the release could not regain its guard within {} s; \
                 recover it per {HOST_LOCK_RECOVERY}",
                HOST_LOCK_RELEASE_RETRY.as_secs()
            );
            return;
        };
        let retired = self.dir.with_extension(format!("releasing.{}", self.token));
        if std::fs::rename(&self.dir, &retired).is_err() {
            return; // the takeover rules eventually reclaim what stayed put
        }
        // The guard was held across the rename, as the canonical release
        // holds it: safe to reap the retired directory.
        let _ = std::fs::remove_file(retired.join("owner.json"));
        let _ = std::fs::remove_dir(&retired);
    }
}

/// Open the guard file in the canonical hardened form: `O_NOFOLLOW`, then a
/// regular-file, single-link check on the open descriptor itself — a
/// replaced or linked `.guard` would give each taker its own inode and
/// silently drop the mutual exclusion, so damage fails closed. `Err` means
/// the path could not be opened as a lone regular file.
fn open_host_guard() -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let guard = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(HOST_LOCK_GUARD)?;
    let stat = rustix::fs::fstat(&guard)?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
        || stat.st_nlink != 1
    {
        return Err(std::io::Error::other(format!(
            "{HOST_LOCK_GUARD} is not a lone regular file"
        )));
    }
    Ok(guard)
}

/// The take-side guard acquisition: one non-blocking flock — another taker's
/// critical section is short (creation and the owner.json publish), so the
/// caller's 50 ms loop is the retry. `None` means held; a damaged guard
/// fails closed and loudly, pointing at the recovery section.
fn host_guard() -> Option<std::fs::File> {
    let guard = open_host_guard().unwrap_or_else(|error| {
        panic!(
            "#2322 port segment guard {HOST_LOCK_GUARD} is damaged ({error}); \
             recover it per {HOST_LOCK_RECOVERY}"
        );
    });
    rustix::fs::flock(&guard, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .is_ok()
        .then_some(guard)
}

/// The release-side guard acquisition: a taker may be inside its critical
/// section when the owner drops, so the release retries the non-blocking
/// take against a `HOST_LOCK_RELEASE_RETRY` deadline (the in-tree
/// `release_guard` precedent in `sdk_sessions_list_exit.rs` — bounded, so a
/// wedged guard holder cannot hang the test, and never stolen). Transient
/// failures — a held guard, or an `open` a momentary condition like EMFILE
/// answers — retry to the same deadline; `None` means the deadline passed or
/// the guard is damaged, and the caller retains fail-closed, which is why
/// damage here is a quiet `None` and not the take side's loud panic: the
/// retention printout, not this function, carries the recovery pointer.
fn release_host_guard() -> Option<std::fs::File> {
    let deadline = std::time::Instant::now() + HOST_LOCK_RELEASE_RETRY;
    loop {
        if let Ok(guard) = open_host_guard() {
            match rustix::fs::flock(&guard, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Some(guard),
                Err(rustix::io::Errno::WOULDBLOCK) => {}
                Err(_) => return None,
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The owner is dead when `kill(pid, 0)` answers ESRCH — through rustix's
/// safe wrapper, since the workspace forbids `unsafe`. Every other verdict,
/// including EPERM for another user's live process, keeps the lease. A pid
/// above i32::MAX is `kill`'s ERR_INVALID_ARG_TYPE, which is not ESRCH: the
/// `run_registry` precedent counts such an owner alive, so only the
/// takeover bound reclaims it.
fn owner_pid_is_dead(pid: u64) -> bool {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    matches!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    )
}

/// The canonical takeover rules: a lease is reclaimable when its owner pid
/// is dead (regardless of age) or it is older than the takeover bound —
/// unless it published a truthy `cleanupPending`. An unreadable owner is
/// treated as reclaimable: `owner.json` is published tmp+rename inside the
/// guard's critical section, so a live lease always has a readable one;
/// this only relaxes the canonical `damaged` retention because a test that
/// waited forever on manual recovery would be unusable.
fn host_lease_is_stale(dir: &std::path::Path) -> bool {
    let owner = std::fs::read(dir.join("owner.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let Some(owner) = owner else {
        return true;
    };
    if owner["cleanupPending"].as_bool().unwrap_or(false) {
        return false;
    }
    let Some(pid) = owner["pid"].as_u64().filter(|pid| *pid > 0) else {
        return true;
    };
    // `kill(pid, 0)`'s dead verdict, made through `owner_pid_is_dead`: a live
    // process — even another user's — keeps its lease.
    if owner_pid_is_dead(pid) {
        return true;
    }
    match owner["timestamp"].as_f64() {
        Some(timestamp) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("#2322 wall clock");
            now.as_secs_f64() - timestamp > HOST_LOCK_MAX_AGE.as_secs_f64()
        }
        None => true,
    }
}

/// One acquisition attempt: the guard's critical section, then the canonical
/// lease publish. `None` means held — the caller waits and retries.
fn try_take_host_lease() -> Option<HostLease> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt as _};
    let _guard = host_guard()?;
    let dir = std::path::PathBuf::from(HOST_LOCK_DIR);
    if dir.is_dir() {
        if !host_lease_is_stale(&dir) {
            return None;
        }
        // Reclaim per the canonical rules; a directory the reclaim cannot
        // remove (foreign files) is damage, not contention — the canonical
        // form retains it for manual recovery, so fail loudly instead of
        // waiting forever.
        std::fs::remove_dir_all(&dir).unwrap_or_else(|error| {
            panic!(
                "#2322 stale port-segment lock {HOST_LOCK_DIR} cannot be \
                 reclaimed ({error}); recover it per {HOST_LOCK_RECOVERY}"
            );
        });
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("#2322 wall clock");
    let token = format!("{:x}-{:x}", std::process::id(), timestamp.as_nanos());
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .expect("#2322 port segment lock dir");
    // Atomic publish: tmp + rename, so a reader never sees a half lease.
    let tmp = dir.join(format!("owner.json.tmp.{token}"));
    let owner = json!({
        "pid": std::process::id(),
        "timestamp": timestamp.as_secs_f64(),
        "token": token,
        "cleanupPending": false,
    });
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .expect("#2322 owner tmp");
    file.write_all((serde_json::to_string(&owner).expect("#2322 owner json") + "\n").as_bytes())
        .expect("#2322 owner write");
    drop(file);
    std::fs::rename(&tmp, dir.join("owner.json")).expect("#2322 owner publish");
    Some(HostLease { dir, token })
}

/// Wait for the host lease — the canonical "wait, not fail": the holder's
/// release (or its death — the kernel drops its flock) ends the wait, and the
/// `HOST_LOCK_WAIT_LIMIT` deadline turns a lease none of the takeover rules
/// can resolve into a failure that names the recovery section.
async fn host_port_segment_lease() -> HostLease {
    let deadline = std::time::Instant::now() + HOST_LOCK_WAIT_LIMIT;
    loop {
        if let Some(lease) = try_take_host_lease() {
            return lease;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "#2322 port segment lock {HOST_LOCK_DIR} is still held after \
                 {} s of waiting — it sits outside the takeover rules (a \
                 future-dated timestamp, a truthy cleanupPending, or a \
                 foreign lease); recover it per {HOST_LOCK_RECOVERY}",
                HOST_LOCK_WAIT_LIMIT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Both halves of the port-segment serialization for one test: the
/// in-process mutex first (cheap, process-local), then the host lease. The
/// fields are declared in the opposite — release — order: the host lease
/// drops first, so the lock path is free before the in-process mutex wakes
/// the next same-process waiter, which must never poll a lease its own
/// process still owns (drop order is declaration order).
struct PortSegmentGuard {
    _host: HostLease,
    _in_process: tokio::sync::MutexGuard<'static, ()>,
}

async fn lock_port_segment() -> PortSegmentGuard {
    let in_process = FIXTURE_PORT_SEGMENT.lock().await;
    let host = host_port_segment_lease().await;
    PortSegmentGuard {
        _in_process: in_process,
        _host: host,
    }
}

/// A failing host-lease test must not leave its retained lease blocking the
/// same process's sibling tests until the `HOST_LOCK_WAIT_LIMIT` bound —
/// the retained owner pid is this very process, alive, so no takeover rule
/// ever resolves it in-process. While unwinding, this reaps our own lease
/// off the lock path under the guard; the green path has nothing to reap
/// (the release already removed it), and a lease a takeover replaced (the
/// token check) is left to its new owner.
struct RetainedLeaseReaper {
    token: String,
}

impl Drop for RetainedLeaseReaper {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let Some(_held) = release_host_guard() else {
            return; // a taker holds the guard; the path is not ours to touch
        };
        let still_ours = std::fs::read(std::path::Path::new(HOST_LOCK_DIR).join("owner.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|owner| {
                owner["token"]
                    .as_str()
                    .is_some_and(|token| token == self.token)
            });
        if still_ours {
            let _ = std::fs::remove_file(std::path::Path::new(HOST_LOCK_DIR).join("owner.json"));
            let _ = std::fs::remove_dir(HOST_LOCK_DIR);
        }
    }
}

/// #2322: the release retries the guard past another taker's critical
/// section instead of silently keeping a live lease. The guard is held
/// across the drop on purpose — the r1 shape this fixes — and released
/// shortly after, so the bounded retry (`HOST_LOCK_RELEASE_RETRY`) must
/// outlast it, re-take the guard and reap the retired lease inside the
/// same drop. Before the fix the single non-blocking take missed the
/// guard and returned, retaining the lease (a live pid, a fresh
/// timestamp) on the lock path where no takeover rule ever reclaims it:
/// the first assertion below was red.
#[tokio::test]
async fn host_lease_release_retries_past_a_held_guard() {
    let in_process = FIXTURE_PORT_SEGMENT.lock().await;
    let lease = host_port_segment_lease().await;
    let _reaper = RetainedLeaseReaper {
        token: lease.token.clone(),
    };
    let retired = lease
        .dir
        .with_extension(format!("releasing.{}", lease.token));
    // Another taker's critical section straddling our release.
    let held = host_guard().expect("#2322 the guard is free under our own lease");
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(held);
    });
    drop(lease);
    releaser
        .join()
        .expect("#2322 the guard-holding thread finished");
    crate::negative_assert!(
        !std::path::Path::new(HOST_LOCK_DIR).exists(),
        "#2322 the release moves the lease off the lock path"
    );
    crate::negative_assert!(
        !retired.exists(),
        "#2322 the release with the guard reaps the retired lease"
    );
    drop(in_process);
}

/// #2322: a release that cannot regain the guard within
/// `HOST_LOCK_RELEASE_RETRY` retains the lease on the lock path
/// (fail-closed, the canonical release's own `Err`-on-`release_guard`
/// shape) instead of renaming it off — the rename form this fixes could
/// move a fresh lease another taker had just published onto the lock path,
/// leaving the path empty for a third taker while the second still holds
/// the segment. The pre-fix (rename) form is red here on the first
/// assertion (it empties the lock path); it also leaves a
/// `releasing.<token>` directory behind and prints no retention line,
/// which the later assertions and the worker rerun below would catch.
#[tokio::test]
async fn host_lease_release_without_the_guard_retains_on_the_lock_path() {
    let in_process = FIXTURE_PORT_SEGMENT.lock().await;
    let lease = host_port_segment_lease().await;
    let token = lease.token.clone();
    let _reaper = RetainedLeaseReaper {
        token: token.clone(),
    };
    let retired = std::path::Path::new(HOST_LOCK_DIR).with_extension(format!("releasing.{token}"));
    // The guard stays with another taker past the release's bounded retry.
    let held = host_guard().expect("#2322 the guard is free under our own lease");
    drop(lease);
    crate::negative_assert!(
        std::path::Path::new(HOST_LOCK_DIR).is_dir(),
        "#2322 a guard-less release retains the lease on the lock path"
    );
    crate::negative_assert!(
        std::path::Path::new(HOST_LOCK_DIR)
            .join("owner.json")
            .is_file(),
        "#2322 the retained lease keeps its owner.json for the takeover rules"
    );
    crate::negative_assert!(
        !retired.exists(),
        "#2322 a guard-less release renames nothing off the lock path"
    );
    // Reap the retained lease by hand under the still-held guard: this
    // test is its only owner, and the rerun below needs the path free.
    std::fs::remove_file(std::path::Path::new(HOST_LOCK_DIR).join("owner.json"))
        .expect("#2322 the retained owner.json is removable");
    std::fs::remove_dir(HOST_LOCK_DIR).expect("#2322 the retained lease is removable");
    drop(held);
    // The retention must also be observable, not a silent fork: the
    // printout names the manual recovery section. It goes to this
    // process's stderr, which the test harness captures, so an owned
    // rerun of the same shape in a worker child provides the line.
    let rerun = crate::test_process::rerun_exact(
        "channel::slack::tests::host_lease_retention_output_worker",
        &[],
        Duration::from_secs(30),
    );
    // #2322 fix-r4 (#1723 sibling/simplify/boundary): the same "was not
    // stopped by this harness" judgment the audits use — one predicate
    // instead of a third literal spelling of the disjunction, so a new stop
    // reason added to the predicate cannot leave this copy behind reading a
    // stopped rerun as a pass.
    crate::negative_assert!(
        rerun.ended_its_own_or_unstopped(),
        "#2322 the retention worker rerun was stopped (deadline={} output-cap={} group-kill={}):\n{}\n{}",
        rerun.timed_out,
        rerun.overflow,
        rerun.stopped,
        rerun.stdout,
        rerun.stderr
    );
    crate::negative_assert!(
        rerun.status.success() && rerun.stderr.contains(HOST_LOCK_RECOVERY),
        "#2322 the guard-less retention prints the manual recovery section:\n{}\n{}",
        rerun.stdout,
        rerun.stderr
    );
    drop(in_process);
}

/// #2322 retention-output worker only: rerun by
/// `host_lease_release_without_the_guard_retains_on_the_lock_path` — the
/// parent cannot read its own harness-captured stderr — to expose the
/// retention printout through a child the parent owns.
#[tokio::test]
#[ignore = "#2322 retention-output worker only (rerun by its parent test)"]
async fn host_lease_retention_output_worker() {
    let lease = host_port_segment_lease().await;
    let dir = lease.dir.clone();
    let held = host_guard().expect("#2322 the guard is free under the worker's own lease");
    drop(lease); // the guard-less retention prints the recovery pointer
    // Reap the retained lease by hand under the still-held guard: this
    // worker is the lease's only owner.
    std::fs::remove_file(dir.join("owner.json")).expect("#2322 the worker reaps its owner.json");
    std::fs::remove_dir(&dir).expect("#2322 the worker reaps its retained lease");
    drop(held);
}

/// #2322: the stale-takeover interleaving the fail-closed retention fixes.
/// P drops a lease the takeover rules already consider stale (its age is
/// past `HOST_LOCK_MAX_AGE`, backdated through the owner.json itself — no
/// clock hook in the production code) while Q holds the guard past the
/// release's bounded retry. The release retains: the lock path still
/// carries P's complete lease, which the takeover side (the
/// `host_lease_is_stale` read Q does inside its critical section) still
/// sees whole, and Q's takeover then reclaims it and publishes a fresh
/// lease. The pre-fix (rename) form was red on the first assertion — it
/// moved the stale lease off the lock path (and would have moved Q's
/// fresh lease instead, had Q already published it, leaving the path
/// empty for a third taker).
#[tokio::test]
async fn host_lease_release_retains_a_stale_lease_a_takeover_still_sees() {
    let in_process = FIXTURE_PORT_SEGMENT.lock().await;
    let lease = host_port_segment_lease().await;
    let token = lease.token.clone();
    let _reaper = RetainedLeaseReaper {
        token: token.clone(),
    };
    // Backdate the owner past the takeover bound (tmp+rename, the
    // canonical publish form): the lease is stale by age while our pid
    // stays alive — the interleaving's precondition.
    let mut owner: Value = serde_json::from_slice(
        &std::fs::read(std::path::Path::new(HOST_LOCK_DIR).join("owner.json"))
            .expect("#2322 the fresh lease publishes its owner.json"),
    )
    .expect("#2322 the owner.json is valid");
    let backdated = std::time::SystemTime::now()
        .checked_sub(HOST_LOCK_MAX_AGE + Duration::from_secs(60))
        .expect("#2322 the wall clock is past the takeover bound")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("#2322 the wall clock is past the epoch")
        .as_secs_f64();
    owner["timestamp"] = json!(backdated);
    let tmp = std::path::Path::new(HOST_LOCK_DIR).join("owner.json.tmp.backdate");
    std::fs::write(
        &tmp,
        serde_json::to_string(&owner).expect("#2322 the backdated owner json"),
    )
    .expect("#2322 the backdate tmp write");
    std::fs::rename(&tmp, std::path::Path::new(HOST_LOCK_DIR).join("owner.json"))
        .expect("#2322 the backdate publish");
    // Q holds the guard while P's release exhausts its bounded retry; the
    // dropping happens on its own thread so the interleaving (a taker
    // holding the guard across P's whole release) is deterministic.
    let held = host_guard().expect("#2322 the guard is free under our own lease");
    let dropper = std::thread::spawn(move || drop(lease));
    dropper.join().expect("#2322 the dropping thread finished");
    // P retained: the lock path still carries our complete stale lease...
    crate::negative_assert!(
        std::path::Path::new(HOST_LOCK_DIR).is_dir(),
        "#2322 the guard-less release retains the stale lease on the lock path"
    );
    let retained_owner: Value = serde_json::from_slice(
        &std::fs::read(std::path::Path::new(HOST_LOCK_DIR).join("owner.json"))
            .expect("#2322 the retained stale lease keeps a readable owner.json"),
    )
    .expect("#2322 the retained owner.json is valid");
    crate::negative_assert!(
        retained_owner["token"].as_str() == Some(token.as_str()),
        "#2322 the retained stale lease is still the dropping process's own"
    );
    // ...which the takeover side still sees whole and reclaimable.
    crate::negative_assert!(
        host_lease_is_stale(std::path::Path::new(HOST_LOCK_DIR)),
        "#2322 the takeover rules still see the retained stale lease whole"
    );
    // Q's takeover, under its guard: reclaims ours and publishes a fresh
    // lease — the interleaving ends with one live lease on the path, not
    // a moved-away path two processes could each believe they hold.
    drop(held);
    let reclaimed =
        try_take_host_lease().expect("#2322 the takeover reclaims the retained stale lease");
    crate::negative_assert!(
        reclaimed.token != token,
        "#2322 the takeover publishes a fresh lease over the retained one"
    );
    drop(reclaimed); // the guard is free again: the clean release reaps it
    crate::negative_assert!(
        !std::path::Path::new(HOST_LOCK_DIR).exists(),
        "#2322 the taken-over lease releases cleanly off the lock path"
    );
    drop(in_process);
}

// ---------------------------------------------------------------------------
// replay
// ---------------------------------------------------------------------------

async fn replay(case: &Value) -> Value {
    let name = case["name"].as_str().expect("#1344 case name").to_owned();
    let context = format!("case {name}");
    // Every blocking step names its cell, so a mutant that never converges
    // fails on the same marker as the cell comparison it stands in for.
    let cell = format!("#1344 case {name} matches the TS oracle");
    let port = case["port"].as_u64().expect("#1344 case port") as u16;
    ensure_replay_port_free(port).await;

    let record: Arc<SyncMutex<Record>> = Arc::new(SyncMutex::new(Record::default()));
    let gates: Gates = Arc::new(SyncMutex::new(HashMap::new()));
    let scripted: VecDeque<(u16, String)> = case["scripted"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["status"].as_u64().expect("#1344 scripted status") as u16,
                        entry["json"].to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let turns: Vec<Value> = case["turns"].as_array().cloned().unwrap_or_default();
    let gateway: GatewayRef = Arc::new(MockGateway {
        turns,
        next: SyncMutex::new(0),
        record: record.clone(),
        gates: gates.clone(),
    });
    let mock = start_mock(port, &record, scripted).await;

    let mut channel: Option<SlackChannel> = None;
    let mut handle: Option<Arc<dyn ChannelHandle>> = None;

    for step in case["steps"].as_array().expect("#1344 case steps") {
        match step["op"].as_str().expect("#1344 step op") {
            "start" | "start-fresh" => {
                let without: Vec<String> = step["without"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                channel = Some(load_channel(port, &without, gateway.clone()));
                let channel = channel.as_ref().expect("#1344 channel built");
                handle = Some(
                    bounded(&cell, channel.start(case_deps(&record)))
                        .await
                        .expect("#1344 start"),
                );
            }
            "restart" => {
                let channel = channel.as_ref().expect("#1344 restart needs a channel");
                handle = Some(
                    bounded(&cell, channel.start(case_deps(&record)))
                        .await
                        .expect("#1344 start"),
                );
            }
            "stop" => {
                let reason = step["reason"].as_str().expect("#1344 stop reason");
                let handle = handle.as_ref().expect("#1344 started");
                bounded(&cell, handle.stop(reason))
                    .await
                    .expect("#1344 stop result");
            }
            "ws-send" => {
                let socket = mock
                    .sockets
                    .lock()
                    .expect("#1344 sockets mutex")
                    .last()
                    .cloned();
                match socket {
                    Some(socket) => {
                        let envelope =
                            json!({"type": "message", "payload": {"event": step["event"].clone()}});
                        write_text_frame(&socket, &envelope.to_string());
                    }
                    None => {
                        record
                            .lock()
                            .expect("#1344 record mutex")
                            .ws
                            .push(json!({"event": "dropped"}));
                    }
                }
            }
            "release" => {
                release_if_exists(&gates, step["gate"].as_str().expect("#1344 gate name"));
            }
            "wait" => {
                // Every named condition is waited in turn, so a step can gate
                // on several streams at once (the turn landing *and* its
                // reply), like the generator's `wait`.
                let mut any = false;
                if let Some(count) = step["requests"].as_u64() {
                    any = true;
                    let record = record.clone();
                    until(&cell, &format!("{context} requests {count}"), move || {
                        record.lock().expect("#1344 record mutex").requests.len() >= count as usize
                    })
                    .await;
                }
                if let Some(count) = step["ws"].as_u64() {
                    any = true;
                    let record = record.clone();
                    until(&cell, &format!("{context} ws {count}"), move || {
                        record.lock().expect("#1344 record mutex").ws.len() >= count as usize
                    })
                    .await;
                }
                if let Some(count) = step["rpcs"].as_u64() {
                    any = true;
                    let record = record.clone();
                    until(&cell, &format!("{context} rpcs {count}"), move || {
                        record.lock().expect("#1344 record mutex").rpcs.len() >= count as usize
                    })
                    .await;
                }
                if let Some(count) = step["logs"].as_u64() {
                    any = true;
                    let record = record.clone();
                    until(&cell, &format!("{context} logs {count}"), move || {
                        record.lock().expect("#1344 record mutex").logs.len() >= count as usize
                    })
                    .await;
                }
                if let Some(count) = step["ticks"].as_u64() {
                    any = true;
                    ticks(count).await;
                }
                assert!(any, "#1344 wait without condition");
            }
            other => panic!("#1344 unknown op {other}"),
        }
    }

    // Every scripted response must have been served: a dry queue means the
    // case consumed the mock default and its cells are garbage.
    let unserved = mock.scripted.lock().expect("#1344 scripted mutex").len();
    crate::negative_assert_eq!(
        unserved,
        0,
        "#1344 case {name} served every scripted response"
    );
    // Quiescence: nothing may arrive after the case's steps end, and the
    // record must be identical one more window later.
    ticks(50).await;
    let settled = snapshot(&record);
    ticks(50).await;
    let after = snapshot(&record);
    crate::negative_assert_eq!(
        after,
        settled,
        "#1344 case {name} records stop arriving after the case ends"
    );
    stop_mock(&mock);
    settled
}

#[tokio::test]
async fn channel_matches_real_ts() {
    // The generator's per-case port rotation is host-global, and the
    // `#[ignore]` regeneration test runs the same generator: serialize the
    // two so one binary can run both (`--include-ignored`) without either
    // side measuring the other's listener — in-process (#1344) and across
    // the host's processes (#2322).
    let _port_segment = lock_port_segment().await;
    let oracle = oracle();
    for case in cases(&oracle) {
        let name = case["name"].as_str().expect("#1344 case name").to_owned();
        let actual = replay(case).await;
        crate::negative_assert_eq!(
            actual,
            case["expected"],
            "#1344 case {} matches the TS oracle",
            name
        );
    }
}

#[tokio::test]
#[ignore = "regenerates from the real TS baseline; set PDRS_BASELINE and the pinned Node v22.22.0 runtime"]
async fn channel_slack_fixture_regenerates_identically() {
    let _port_segment = lock_port_segment().await;
    crate::test_baseline::regenerates_identically("channel-slack");
}

// ---------------------------------------------------------------------------
// #2321 / #2312: the default REST base, the abnormal close, the failed start
// ---------------------------------------------------------------------------

/// #2321: with the default base, each stub-contract path must join to its
/// production URL exactly once — the old default carried its own `/api`, so
/// every call site joined `https://slack.com/api/api/...` in production (the
/// loopback mock hid it: its base has no path).
#[test]
fn default_rest_base_joins_the_contract_paths_once() {
    let channel = SlackChannel::new(SlackChannelOptions {
        bot_token: Some(JsValue::String(JsString::from(BOT_TOKEN))),
        app_token: Some(JsValue::String(JsString::from(APP_TOKEN))),
        mapper: None,
        gateway: None,
        env: None,
        base_url: None,
    });
    for path in [
        "/api/apps.connections.open",
        "/api/auth.test",
        "/api/chat.postMessage",
    ] {
        crate::negative_assert_eq!(
            rest_url(&channel.shared.base_url, path),
            format!("https://slack.com{path}"),
            "#2321 the default REST base joins the contract paths once"
        );
    }
}

/// #2321: a non-1000 close frame is delivered as `Ok(Message::Close)` —
/// measured: `Some(Ok(Close(Some(CloseFrame { code: Error, reason: "" }))))`,
/// then `None` — so the reader reads `CloseFrame.code` and reports the stub's
/// `bolt error` (`Error: websocket error`) for every code outside
/// {1000, 1005} (1005 is the empty close the SDK treats as transport noise).
#[tokio::test]
async fn abnormal_close_logs_the_stub_bolt_error() {
    let _port_segment = lock_port_segment().await;
    let port = 18184;
    ensure_replay_port_free(port).await;
    let record: Arc<SyncMutex<Record>> = Arc::new(SyncMutex::new(Record::default()));
    let gates: Gates = Arc::new(SyncMutex::new(HashMap::new()));
    let gateway: GatewayRef = Arc::new(MockGateway {
        turns: vec![],
        next: SyncMutex::new(0),
        record: record.clone(),
        gates,
    });
    let mock = start_mock(port, &record, VecDeque::new()).await;
    let channel = load_channel(port, &[], gateway);
    let handle = bounded(
        "#2321 an abnormal server close logs the stub bolt error",
        channel.start(case_deps(&record)),
    )
    .await
    .expect("#2321 start");
    // A server close(1011): raw frame `0x88 0x02 0x03 0xf3` (FIN + close,
    // payload = the 2-byte big-endian code).
    let socket = mock
        .sockets
        .lock()
        .expect("#1344 sockets mutex")
        .last()
        .cloned()
        .expect("#2321 the socket opened");
    let _ = socket
        .lock()
        .expect("#1344 socket mutex")
        .try_write(&[0x88, 0x02, 0x03, 0xf3]);
    let waited = record.clone();
    until(
        "#2321 an abnormal server close logs the stub bolt error",
        "case abnormal-close bolt error log",
        move || {
            waited
                .lock()
                .expect("#1344 record mutex")
                .logs
                .iter()
                .any(|(level, message)| {
                    level == "error" && message == "slack: bolt error: Error: websocket error"
                })
        },
    )
    .await;
    stop_mock(&mock);
    drop(channel);
    drop(handle);
}

/// #2312: a failed `auth.test` must retire the session it already opened —
/// sink dropped (socket closed), reader aborted (task gone), `Arc<SlackShared>`
/// releasable once the channel itself is dropped — and wire-invisibly: the
/// `start-auth-test-failed` oracle cell pins `ws == [open]`, so the teardown
/// sends no close frame (TS keeps `this.app` until process teardown — the
/// registered R2 divergence).
#[tokio::test]
async fn auth_test_failure_releases_the_session() {
    let _port_segment = lock_port_segment().await;
    let port = 18185;
    ensure_replay_port_free(port).await;
    let record: Arc<SyncMutex<Record>> = Arc::new(SyncMutex::new(Record::default()));
    let scripted = VecDeque::from([
        (
            200,
            json!({"ok": true, "url": format!("ws://127.0.0.1:{port}/link")}).to_string(),
        ),
        (
            200,
            json!({"ok": false, "error": "invalid_bot_token"}).to_string(),
        ),
    ]);
    let gates: Gates = Arc::new(SyncMutex::new(HashMap::new()));
    let gateway: GatewayRef = Arc::new(MockGateway {
        turns: vec![],
        next: SyncMutex::new(0),
        record: record.clone(),
        gates,
    });
    let mock = start_mock(port, &record, scripted).await;
    let channel = load_channel(port, &[], gateway);
    let weak = Arc::downgrade(&channel.shared);
    let handle = bounded(
        "#2312 a failed auth.test releases the session",
        channel.start(case_deps(&record)),
    )
    .await
    .expect("#2312 start returns the silent handle");
    let waited = record.clone();
    until(
        "#2312 a failed auth.test releases the session",
        "case auth-test-failure start failed log",
        move || {
            waited
                .lock()
                .expect("#1344 record mutex")
                .logs
                .iter()
                .any(|(level, message)| {
                    level == "error" && message == "slack: start failed: Error: invalid_bot_token"
                })
        },
    )
    .await;
    // Wire-invisible teardown: the connections.open + auth.test requests, one
    // socket open, and no close frame or stop traffic after the failure.
    crate::negative_assert_eq!(
        snapshot(&record)["ws"],
        json!([{"event": "open"}]),
        "#2312 a failed auth.test releases the session"
    );
    // The channel is now the last owner of the shared state: the reader is
    // aborted and the failure handle is the silent one, so dropping the
    // channel releases the `Arc`.
    drop(channel);
    drop(handle);
    until(
        "#2312 a failed auth.test releases the session",
        "case auth-test-failure the shared state is released",
        || weak.upgrade().is_none(),
    )
    .await;
    stop_mock(&mock);
}

// ---------------------------------------------------------------------------
// component pins: the mapper, the render fragments, mrkdwn and chunking
// ---------------------------------------------------------------------------

fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

fn fixed_mapper() -> SlackSessionMapper {
    SlackSessionMapper::new(
        SlackSessionMapperState::default(),
        Box::new(|| JsString::from(MAPPER_UUID)),
    )
}

#[test]
fn mapper_defaults_to_the_general_session_and_trims() {
    let mut mapper = fixed_mapper();
    let mapped = mapper.resolve(&JsString::from("C1"), &JsString::from("  你好 slack  "));
    crate::negative_assert_eq!(
        text(&mapped.session_key),
        "slack:chat=C1:general",
        "#1344 the first message lands on the chat's general session"
    );
    assert_eq!(text(&mapped.message), "你好 slack");
    assert!(!mapped.command_new);
}

#[test]
fn mapper_new_command_switches_to_a_fresh_session() {
    let mut mapper = fixed_mapper();
    let mapped = mapper.resolve(&JsString::from("C1"), &JsString::from("/new"));
    crate::negative_assert_eq!(
        text(&mapped.session_key),
        format!("slack:chat=C1:s_{MAPPER_UUID}"),
        "#1344 the /new command switches the chat to a fresh s_ session"
    );
    assert!(mapped.command_new);
    assert_eq!(text(&mapped.message), "");
    // The new session persists for the chat's next message, while other
    // chats keep their general key.
    let next = mapper.resolve(&JsString::from("C1"), &JsString::from("继续"));
    assert_eq!(
        text(&next.session_key),
        format!("slack:chat=C1:s_{MAPPER_UUID}")
    );
    assert!(!next.command_new);
    let other = mapper.resolve(&JsString::from("C2"), &JsString::from("hi"));
    assert_eq!(text(&other.session_key), "slack:chat=C2:general");
    // `/new` needs the end of the command or a space: `/newer` is text.
    let similar = mapper.resolve(&JsString::from("C3"), &JsString::from("/newer"));
    assert_eq!(text(&similar.session_key), "slack:chat=C3:general");
    assert!(!similar.command_new);
}

#[test]
fn mapper_new_with_body_carries_the_payload() {
    let mut mapper = fixed_mapper();
    let mapped = mapper.resolve(&JsString::from("C1"), &JsString::from("/new 直接处理"));
    assert_eq!(
        text(&mapped.session_key),
        format!("slack:chat=C1:s_{MAPPER_UUID}")
    );
    assert!(mapped.command_new);
    assert_eq!(text(&mapped.message), "直接处理");
}

#[test]
fn mapper_snapshot_carries_the_active_session_state() {
    let mut mapper = fixed_mapper();
    mapper.resolve(&JsString::from("C1"), &JsString::from("/new"));
    let state = mapper.snapshot();
    let mut restored = SlackSessionMapper::new(state, Box::new(|| JsString::from("unused")));
    let next = restored.resolve(&JsString::from("C1"), &JsString::from("继续"));
    assert_eq!(
        text(&next.session_key),
        format!("slack:chat=C1:s_{MAPPER_UUID}")
    );
}

#[test]
fn render_maps_turn_events_to_slack_fragments() {
    let delta = json!({"type": "assistant_text_delta", "text": "你好"});
    assert_eq!(
        text(&render::render_slack_event(&to_js(&delta)).expect("#1344 delta fragment")),
        "你好"
    );
    let thinking = json!({"type": "assistant_thinking_delta"});
    assert_eq!(
        text(&render::render_slack_event(&to_js(&thinking)).expect("#1344 thinking fragment")),
        ""
    );
    let started = json!({"type": "tool_call_started"});
    assert_eq!(
        text(&render::render_slack_event(&to_js(&started)).expect("#1344 started fragment")),
        ""
    );
    let finished_ok = json!({"type": "tool_call_finished", "ok": true});
    assert_eq!(
        text(&render::render_slack_event(&to_js(&finished_ok)).expect("#1344 ok fragment")),
        ""
    );
    let error = json!({"type": "error", "message": "坏了"});
    assert_eq!(
        text(&render::render_slack_event(&to_js(&error)).expect("#1344 error fragment")),
        "\n❌ 坏了\n"
    );
    let unknown = json!({"type": "something_else"});
    assert!(render::render_slack_event(&to_js(&unknown)).is_none());
    let no_type = json!({"text": "没有类型"});
    assert!(render::render_slack_event(&to_js(&no_type)).is_none());
}

#[test]
fn render_reports_a_failed_tool_call_by_name() {
    let failed = json!({"type": "tool_call_finished", "ok": false, "toolName": "Bash"});
    let fragment = render::render_slack_event(&to_js(&failed))
        .expect("#1344 a failed tool call renders its warning line");
    crate::negative_assert_eq!(
        text(&fragment),
        "\n⚠️ Bash failed\n",
        "#1344 a failed tool call renders its warning line"
    );
    let by_id = json!({"type": "tool_call_finished", "ok": false, "toolCallId": "call-7"});
    let fragment = render::render_slack_event(&to_js(&by_id)).expect("#1344 id fragment");
    assert_eq!(text(&fragment), "\n⚠️ call-7 failed\n");
    let nameless = json!({"type": "tool_call_finished", "ok": false});
    let fragment = render::render_slack_event(&to_js(&nameless)).expect("#1344 nameless");
    assert_eq!(text(&fragment), "\n⚠️ undefined failed\n");
}

#[test]
fn mrkdwn_conversion_bold_then_link() {
    let source = JsString::from("**加粗** 和 [链接](https://example.com)");
    crate::negative_assert_eq!(
        text(&format_slack_mrkdwn(&source)),
        "*加粗* 和 <https://example.com|链接>",
        "#1344 case mrkdwn-and-link-formats matches the TS oracle"
    );
    // Bold runs first, so a bolded link converts inside the bold.
    let both = JsString::from("**[a](u)**");
    assert_eq!(text(&format_slack_mrkdwn(&both)), "*<u|a>*");
    // A bold middle may not contain a line terminator (no `s` flag).
    let split = JsString::from("**a\nb**");
    assert_eq!(text(&format_slack_mrkdwn(&split)), "**a\nb**");
    // Unclosed runs stay literal.
    assert_eq!(text(&format_slack_mrkdwn(&JsString::from("**a"))), "**a");
    assert_eq!(
        text(&format_slack_mrkdwn(&JsString::from("[a](u"))),
        "[a](u"
    );
    assert_eq!(text(&format_slack_mrkdwn(&JsString::from(""))), "");
}

#[test]
fn chunk_text_splits_at_the_last_newline_past_halfway() {
    // The oracle's long-text cell: 38500 x's, a newline, 1500 y's, split at
    // the newline with the next chunk's leading newline stripped.
    let units: Vec<u16> = format!("{}{}{}", "x".repeat(38500), '\n', "y".repeat(1500))
        .encode_utf16()
        .collect();
    let chunks = chunk_text(&units, 39_000);
    crate::negative_assert_eq!(
        chunks.len(),
        2,
        "#1344 case long-text-chunked matches the TS oracle"
    );
    assert_eq!(chunks[0].len(), 38500);
    assert!(chunks[0].iter().all(|&unit| unit == u16::from(b'x')));
    assert_eq!(chunks[1].len(), 1500);
    assert!(chunks[1].iter().all(|&unit| unit == u16::from(b'y')));
    // No separator past the halfway mark: the hard split at the limit.
    let hard: Vec<u16> = "a".repeat(40_000).encode_utf16().collect();
    let chunks = chunk_text(&hard, 39_000);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), 39_000);
    assert_eq!(chunks[1].len(), 1_000);
    // Short text is never split.
    let short: Vec<u16> = "abc".encode_utf16().collect();
    assert_eq!(chunk_text(&short, 39_000).len(), 1);
}
