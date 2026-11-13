//! #1329: replay of the `channel-feishu` golden fixture — the same step
//! scripts the TS generator ran against the real baseline channel (Node
//! v22.22.0, baseline cc7c4e6), driven through the native hyper listener
//! (`native_http::serve_with_body`) with the same loopback Lark fake and a
//! scripted A7 gateway — plus the component pins the golden oracle cannot
//! carry: the top-level `null` body divergence (diff 007 /
//! A8.3-CH-FEISHU-007), the 2000-event dedupe-ring eviction and the
//! tenant-token invalidation on Lark's 99991663.
//!
//! Registered diffs this replay is shaped by (gateway-parity ledger):
//! 002 the generator's attachment store is a no-op fake, so nothing under
//!     ~/.pilotdeck is ever touched;
//! 003 the system command registry is a fake that reports "not handled";
//! 004 `/new` does not abort the previous run, so `abortTurn` is never
//!     recorded;
//! 005 the live-reply controller's intermediate sends are not ported — the
//!     generator pins `liveReplyOptions` to the same "one final post per
//!     chunk" semantics through the baseline's own test seam, so the cells
//!     compare without masking;
//! 006 the gateway record keeps only the three TS `submitTurn` inputs;
//! 007 the top-level `null` body pin below (TS throws an uncaught TypeError
//!     reading `raw.token` on `null`; the port answers `{"ok":true}`).
use super::super::protocol::{
    BoxFuture, ChannelError, ChannelHandle, ChannelStartDeps, TurnEventStream, TurnGateway,
    TurnRequest,
};
use super::*;
use crate::test_baseline;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::service::service_fn;
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const ORACLE: &str = include_str!("../../../tests/fixtures/channel-feishu/oracle.json");
/// The generator's constants; every id the channel emits is one of these.
const APP_ID: &str = "app-1";
const APP_SECRET: &str = "sec-1";
const VERIFY_TOKEN: &str = "vt-1";
const FIXED_UUID: &str = "aaaa1111-bbbb-4ccc-8ddd-eeeeeeeeeeee";
/// The Lark Open API loopback the generator ran on (generate.mts
/// `LARK_PORT`); the replay's channel points at its own copy per test so the
/// parallel pins never race for one listener.
const LARK_PORT: u16 = 18990;
/// The component pins take their own ports inside the 18960-18999 feishu
/// block the task reserved (wecom owns 18997-18998, weixin 18800-18921).
const NULL_PIN_ROUTER_PORT: u16 = 18973;
const NULL_PIN_LARK_PORT: u16 = 18991;
const DEDUP_PIN_LARK_PORT: u16 = 18992;
const TOKEN_PIN_LARK_PORT: u16 = 18993;
/// Every blocking step is bounded so a regression fails instead of hanging
/// the suite (the whole lib suite runs in parallel; the parity cells are
/// compared exactly, never by time).
const DEADLINE: Duration = Duration::from_secs(20);
/// The generator's post-case quiescence window (two `ticks(50)` rounds).
const SETTLE: Duration = Duration::from_millis(50);

fn oracle() -> Value {
    test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1329 oracle JSON")
}

fn cases(oracle: &Value) -> &Vec<Value> {
    let cases = oracle["cases"].as_array().expect("#1329 oracle cases");
    crate::negative_assert!(!cases.is_empty(), "#1329 oracle cases non-empty");
    crate::negative_assert_eq!(
        cases.len() as u64,
        oracle["expectedCaseCount"]
            .as_u64()
            .expect("#1329 expectedCaseCount"),
        "#1329 oracle case count matches its declaration"
    );
    // The declaration alone is not a ruler: dropping a case and lowering
    // `expectedCaseCount` together would keep both assertions above green.
    // The generator's fixed count is pinned separately
    // (`EXPECTED_CASE_COUNT` in `generate.mts`), so coverage cannot shrink
    // silently.
    crate::negative_assert_eq!(
        cases.len() as u64,
        13,
        "#1329 oracle holds the generator's 13 cases"
    );
    cases
}

async fn bounded<T>(context: &str, future: impl std::future::Future<Output = T>) -> T {
    match timeout(DEADLINE, future).await {
        Ok(value) => value,
        // Route through the receipt emitter: a plain panic yields no receipt
        // and the host matrix reports INVALID instead of KILLED.
        Err(_) => crate::test_negative_receipt::expect_some(
            None,
            &format!("#1329 {context} matches the TS oracle"),
        ),
    }
}

/// Wait for `predicate`, emitting the receipt of the cell this wait stands in
/// for when the deadline passes. `assertion` must be one a mutation row
/// declares — a timeout that invented its own name (`case x wait lark 5`)
/// would be judged INVALID instead of KILLED even though the mutation was
/// caught. `detail` therefore only reaches stderr.
async fn until(assertion: &str, detail: &str, predicate: impl Fn() -> bool) {
    let started = Instant::now();
    while !predicate() {
        if started.elapsed() >= DEADLINE {
            eprintln!("#1329 timeout waiting for {detail}");
        }
        crate::negative_assert!(started.elapsed() < DEADLINE, "{assertion}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// The generator's `maskDynamic`, rule for rule and order for order: a
/// 13-digit run at word boundaries becomes `<now>`, then a UUID-shaped hex
/// run (no boundaries — it must match inside `s_<uuid>`) becomes `<uuid>`.
/// The `s_` prefix of a session key is exactly the load-bearing case.
fn mask_dynamic(text: &str) -> String {
    let bytes = text.as_bytes();
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let hex = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);

    // `[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}` starting
    // at `at`, with the same leftmost scan the regex performs (a longer hex
    // run still matches one position in).
    let uuid_at = |at: usize| -> Option<usize> {
        let group = |from: usize, len: usize| {
            from + len <= bytes.len() && (from..from + len).all(|p| hex(bytes[p]))
        };
        let mut p = at;
        for len in [8usize, 4, 4, 4] {
            if !group(p, len) {
                return None;
            }
            p += len;
            if p >= bytes.len() || bytes[p] != b'-' {
                return None;
            }
            p += 1;
        }
        if !group(p, 12) {
            return None;
        }
        Some(p + 12)
    };

    let mut out = String::new();
    let mut last = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit()
            && (i == 0 || !word(bytes[i - 1]))
            && i + 13 <= bytes.len()
            && bytes[i..i + 13].iter().all(|byte| byte.is_ascii_digit())
            && (i + 13 == bytes.len() || !word(bytes[i + 13]))
        {
            out.push_str(&text[last..i]);
            out.push_str("<now>");
            last = i + 13;
            i = last;
            continue;
        }
        if let Some(end) = uuid_at(i) {
            out.push_str(&text[last..i]);
            out.push_str("<uuid>");
            last = end;
            i = end;
            continue;
        }
        i += 1;
    }
    out.push_str(&text[last..]);
    out
}

// ---------------------------------------------------------------- recording

#[derive(Default)]
struct Record {
    http: Vec<Value>,
    lark: Vec<Value>,
    gateway: Vec<Value>,
    logs: Vec<Value>,
}

impl Record {
    fn snapshot(&self) -> Value {
        json!({
            "http": self.http,
            "lark": self.lark,
            "gateway": self.gateway,
            "logs": self.logs,
        })
    }
}

type Gate = Arc<tokio::sync::watch::Sender<bool>>;
type Gates = Arc<Mutex<HashMap<String, Gate>>>;

fn gate(gates: &Gates, name: &str) -> Gate {
    gates
        .lock()
        .expect("#1329 gate mutex")
        .entry(name.to_owned())
        .or_insert_with(|| Arc::new(tokio::sync::watch::Sender::new(false)))
        .clone()
}

async fn released(gate: Gate) {
    let mut rx = gate.subscribe();
    while !*rx.borrow_and_update() {
        rx.changed().await.expect("#1329 gate sender retained");
    }
}

#[derive(Clone, Default)]
struct TurnScript {
    gate_before: Option<String>,
    /// Per-event staging: event `i` is only sent after the gate
    /// `<name>-<i>` flips true — the fine-grained control the gated-turn pin
    /// needs (an event consumed while the turn is still current, then the
    /// stream parked until after the superseding `/new`).
    gate_each: Option<String>,
    events: Vec<Value>,
    error: Option<String>,
}

/// The A7 `submitTurn` fake: records the three TS inputs (diff 006), then
/// streams the scripted events (optionally behind a gate) and finally the
/// scripted error.
struct MockInbound {
    turns: Vec<TurnScript>,
    next: Mutex<usize>,
    record: Arc<Mutex<Record>>,
    gates: Gates,
}

/// Adapts the scripted `mpsc` receiver to the shared `TurnEventStream`.
struct MockStream(tokio::sync::mpsc::UnboundedReceiver<Result<JsValue, ChannelError>>);

impl TurnEventStream for MockStream {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move {
            match self.0.recv().await {
                Some(Ok(event)) => Ok(Some(event)),
                Some(Err(error)) => Err(error),
                None => Ok(None),
            }
        })
    }
}

impl MockInbound {
    fn new(turns: Vec<TurnScript>, record: Arc<Mutex<Record>>, gates: Gates) -> Arc<Self> {
        Arc::new(Self {
            turns,
            next: Mutex::new(0),
            record,
            gates,
        })
    }
}

impl TurnGateway for MockInbound {
    fn submit(
        &self,
        input: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let turn = {
            let mut next = self.next.lock().expect("#1329 turn index");
            let index = *next;
            *next += 1;
            self.turns.get(index).cloned().unwrap_or_default()
        };
        self.record
            .lock()
            .expect("#1329 record mutex")
            .gateway
            .push(json!({
                "sessionKey": mask_dynamic(&String::from_utf16_lossy(input.session_key.units())),
                "channelKey": input.channel_key.to_string(),
                "message": String::from_utf16_lossy(input.message.units()),
            }));
        let gates = self.gates.clone();
        let (events, stream) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            if let Some(name) = turn.gate_before {
                released(gate(&gates, &name)).await;
            }
            let gate_each = turn.gate_each;
            for (index, event) in turn.events.into_iter().enumerate() {
                if let Some(name) = &gate_each {
                    released(gate(&gates, &format!("{name}-{index}"))).await;
                }
                let _ = events.send(Ok(parse_json(&event.to_string()).expect("#1329 event JSON")));
            }
            if let Some(error) = turn.error {
                let _ = events.send(Err(ChannelError::error(error)));
            }
        });
        Box::pin(async move { Ok(Box::new(MockStream(stream)) as Box<dyn TurnEventStream>) })
    }
}

// ---------------------------------------------------------------- lark fake

/// The loopback Lark Open API fake: one server per test, a scripted response
/// queue popped per request in arrival order (the generator's mock has no
/// per-path routing — a mid-sequence DELETE needs its own scripted slot),
/// the `{code:0}` default when dry, and a per-case record slot with stray
/// tracking for anything that arrives outside an armed case.
struct Lark {
    script: Mutex<VecDeque<(u16, String)>>,
    record: Mutex<Option<Arc<Mutex<Record>>>>,
    stray: Mutex<Vec<Value>>,
}

impl Lark {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(VecDeque::new()),
            record: Mutex::new(None),
            stray: Mutex::new(Vec::new()),
        })
    }

    fn arm(&self, case: &Value, record: Arc<Mutex<Record>>) {
        let script = case["scripted"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        (
                            entry["status"].as_u64().expect("#1329 scripted status") as u16,
                            entry["json"].to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.arm_script(script, record);
    }

    fn arm_script(&self, script: Vec<(u16, String)>, record: Arc<Mutex<Record>>) {
        let mut queued = self.script.lock().expect("#1329 lark script");
        queued.clear();
        queued.extend(script);
        *self.record.lock().expect("#1329 lark record slot") = Some(record);
    }

    fn disarm(&self) {
        *self.record.lock().expect("#1329 lark record slot") = None;
    }
}

async fn start_lark_on(lark: Arc<Lark>, port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("#1329 lark bind");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let lark = lark.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let lark = lark.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let bytes = match body.collect().await {
                            Ok(collected) => collected.to_bytes(),
                            Err(_) => Bytes::new(),
                        };
                        let entry = json!({
                            "method": parts.method.as_str(),
                            "path": parts.uri.path(),
                            "query": parts.uri.path_and_query()
                                .and_then(|pq| pq.query())
                                .map(str::to_owned),
                            "auth": parts.headers
                                .get(hyper::header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned),
                            "contentType": parts.headers
                                .get(hyper::header::CONTENT_TYPE)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned),
                            "body": mask_dynamic(&String::from_utf8_lossy(&bytes)),
                        });
                        let record = lark.record.lock().expect("#1329 lark record slot").clone();
                        match record {
                            Some(record) => {
                                record.lock().expect("#1329 record mutex").lark.push(entry)
                            }
                            None => lark.stray.lock().expect("#1329 stray mutex").push(entry),
                        }
                        let (status, payload) = lark
                            .script
                            .lock()
                            .expect("#1329 lark script")
                            .pop_front()
                            .unwrap_or((200, "{\"code\":0}".to_owned()));
                        let response = hyper::Response::builder()
                            .status(hyper::StatusCode::from_u16(status).expect("#1329 lark status"))
                            .header(hyper::header::CONTENT_TYPE, "application/json")
                            .body(Full::new(Bytes::from(payload)))
                            .expect("#1329 lark response");
                        Ok::<_, std::io::Error>(response)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
            });
        }
    });
}

// ---------------------------------------------------------------- raw client

fn io_code(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => "ECONNREFUSED".to_owned(),
        std::io::ErrorKind::AddrInUse => "EADDRINUSE".to_owned(),
        _ => format!("Error: {error}"),
    }
}

/// The TS generator's `fetch` hands the oracle the decoded body, so a
/// chunked reply must be de-framed before it can be compared with the golden
/// cells: `9\r\nnot found\r\n0\r\n\r\n` is the wire form of `not found`.
/// Malformed framing degrades to the partial text and fails on the case
/// marker instead of a helper expect (host matrix: KILLED, not INVALID).
fn decode_chunked(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some(line_end) = rest.find("\r\n") {
        let size_token = rest[..line_end].split(';').next().unwrap_or("0").trim();
        let size = usize::from_str_radix(size_token, 16).unwrap_or(0);
        rest = &rest[line_end + 2..];
        if size == 0 {
            break;
        }
        let take = size.min(rest.len());
        out.push_str(&rest[..take]);
        rest = &rest[take..];
        if rest.starts_with("\r\n") {
            rest = &rest[2..];
        }
    }
    out
}

fn parse_raw_response(context: &str, raw: &[u8]) -> Value {
    // A mutation that stops a response from arriving at all must fail on the
    // case marker, not on an unnamed helper expect (host matrix: INVALID).
    let header_end = crate::test_negative_receipt::expect_some(
        raw.windows(4).position(|window| window == b"\r\n\r\n"),
        &format!("#1329 {context} matches the TS oracle"),
    );
    let head = String::from_utf8_lossy(&raw[..header_end]).into_owned();
    let body = String::from_utf8_lossy(&raw[header_end + 4..]).into_owned();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("#1329 status line");
    let status: u16 = status_line
        .split(' ')
        .nth(1)
        .expect("#1329 status token")
        .parse()
        .expect("#1329 status number");
    let mut content_type: Option<String> = None;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-type") {
                content_type = Some(value.trim().to_owned());
            }
            if name.eq_ignore_ascii_case("transfer-encoding")
                && value.trim().eq_ignore_ascii_case("chunked")
            {
                chunked = true;
            }
        }
    }
    let body = if chunked { decode_chunked(&body) } else { body };
    json!({"status": status, "contentType": content_type, "body": body})
}

async fn http_request(
    context: &str,
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Value {
    let mut stream = match TcpStream::connect(("127.0.0.1", port)).await {
        Ok(stream) => stream,
        Err(error) => return json!({"error": io_code(&error)}),
    };
    let body = body.unwrap_or("");
    let request = if body.is_empty() {
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
    } else {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    let _ = stream.write_all(request.as_bytes()).await;
    let mut raw = Vec::new();
    // Bounded like every other blocking step: a regression that stops writing
    // the response (or stops honouring `Connection: close`) must fail on the
    // case marker, not hang the suite (`DEADLINE` contract above).
    let _ = bounded(context, stream.read_to_end(&mut raw)).await;
    parse_raw_response(context, &raw)
}

// ---------------------------------------------------------------- channel rig

/// `Date.now()` frozen inside the token-expiry window: the scripted `expire:
/// 7200` keeps one cached tenant token across a case's turns, exactly what
/// the generator's real clock produced over a run's few milliseconds.
struct FixedClock;
impl Clock for FixedClock {
    fn now_ms(&self) -> f64 {
        1000.0
    }
}

/// A fresh channel per case, like the generator's `new FeishuChannel(...)`:
/// the slot is published at construction (TS `activeFeishu = options.feishu`
/// before `start`), the clock and session uuid are the parity seams, and
/// `base_url` points the Lark client at the test's loopback fake.
fn case_channel(lark_port: u16) -> (FeishuChannel, Arc<FeishuWebhookSlot>) {
    let slot = Arc::new(FeishuWebhookSlot::new());
    let channel = FeishuChannel::with_seams(
        FeishuOptions {
            app_id: APP_ID.to_owned(),
            app_secret: APP_SECRET.to_owned(),
            encrypt_key: None,
            verify_token: Some(VERIFY_TOKEN.to_owned()),
            connection_mode: "webhook".to_owned(),
            domain_name: "feishu".to_owned(),
            permission_mode: None,
            base_url: format!("http://127.0.0.1:{lark_port}"),
        },
        no_interaction(),
        Some(slot.clone()),
        Arc::new(FixedClock),
        Arc::new(|| JsString::from(FIXED_UUID)),
    );
    (channel, slot)
}

/// The transcribed router: `native_http::serve_with_body` with only the
/// webhook entry mounted, whose own fall-through is the TS `handleHttpRequest`
/// 404. Returns the accept task so a case can retire its listener.
async fn start_router(slot: Arc<FeishuWebhookSlot>, port: u16) -> tokio::task::JoinHandle<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("#1329 webhook bind");
    tokio::spawn(async move {
        let config = crate::native_http::Config {
            token: String::new(),
            loopback: false,
            static_root: None,
            webhook: Some(FeishuWebhookSlot::resolver(&slot)),
            api_web: None,
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let config = config.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let config = config.clone();
                    async move { crate::native_http::serve_with_body(request, &config).await }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
            });
        }
    })
}

fn turn_scripts(case: &Value) -> Vec<TurnScript> {
    case["turns"]
        .as_array()
        .map(|turns| {
            turns
                .iter()
                .map(|turn| TurnScript {
                    gate_before: turn["gateBefore"].as_str().map(str::to_owned),
                    gate_each: turn["gateEach"].as_str().map(str::to_owned),
                    events: turn["events"].as_array().cloned().unwrap_or_default(),
                    error: turn["error"].as_str().map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A v2 receive envelope with the matching verify token, the shape every
/// golden turn case posts.
fn v2_event(event_id: &str, chat_id: &str, text: &str, message_id: Option<&str>) -> String {
    let mut message = json!({
        "chat_id": chat_id,
        "message_type": "text",
        "content": json!({ "text": text }).to_string(),
    });
    if let Some(message_id) = message_id {
        message["message_id"] = json!(message_id);
    }
    json!({
        "header": {
            "event_id": event_id,
            "event_type": "im.message.receive_v1",
            "token": VERIFY_TOKEN,
        },
        "event": { "message": message },
    })
    .to_string()
}

fn case_deps(record: &Arc<Mutex<Record>>, gateway: Arc<dyn TurnGateway>) -> ChannelStartDeps {
    let logger = record.clone();
    ChannelStartDeps {
        config: None,
        report_status: None,
        log: Some(Arc::new(move |level, message| {
            logger
                .lock()
                .expect("#1329 record mutex")
                .logs
                .push(json!([level, mask_dynamic(message)]));
        })),
        turn_gateway: Some(gateway),
    }
}

// ---------------------------------------------------------------- replay

async fn replay(case: &Value, lark: &Arc<Lark>) -> Value {
    let record = Arc::new(Mutex::new(Record::default()));
    lark.arm(case, record.clone());
    let gates = Gates::default();
    // Every blocking step names its case, so a mutation that never converges
    // fails on the same marker as the cell comparison it stands in for.
    let name = case["name"].as_str().expect("#1329 case name");
    let context = format!("case {name}");

    let gateway = MockInbound::new(turn_scripts(case), record.clone(), gates.clone());
    let (channel, slot) = case_channel(LARK_PORT);
    let deps = case_deps(&record, gateway);
    let port = case["port"]
        .as_u64()
        .expect("#1329 case port")
        .try_into()
        .expect("#1329 case port fits u16");
    let router = start_router(slot, port).await;

    let mut handle: Option<Arc<dyn ChannelHandle>> = None;

    for step in case["steps"].as_array().expect("#1329 case steps") {
        match step["op"].as_str().expect("#1329 step op") {
            "start" => {
                handle = Some(
                    bounded(&context, channel.start(deps.clone()))
                        .await
                        .expect("#1329 start"),
                );
            }
            "http" => {
                let result = http_request(
                    &context,
                    port,
                    step["method"].as_str().expect("#1329 http method"),
                    step["path"].as_str().expect("#1329 http path"),
                    step["body"].as_str(),
                )
                .await;
                record.lock().expect("#1329 record mutex").http.push(result);
            }
            "stop" => {
                let reason = step["reason"].as_str().expect("#1329 stop reason");
                bounded(
                    &context,
                    handle.as_ref().expect("#1329 started").stop(reason),
                )
                .await
                .expect("#1329 stop result");
            }
            "release" => {
                gate(&gates, step["gate"].as_str().expect("#1329 gate name")).send_replace(true);
            }
            "wait" => {
                // The wait stands in for this case's cell comparison, so a
                // timeout must emit exactly that cell's receipt.
                let cell = format!("#1329 case {name} matches the TS oracle");
                if let Some(count) = step["lark"].as_u64() {
                    let record = record.clone();
                    until(&cell, &format!("{context} lark {count}"), move || {
                        record.lock().expect("#1329 record mutex").lark.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["gateway"].as_u64() {
                    let record = record.clone();
                    until(&cell, &format!("{context} gateway {count}"), move || {
                        record.lock().expect("#1329 record mutex").gateway.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["logs"].as_u64() {
                    let record = record.clone();
                    until(&cell, &format!("{context} logs {count}"), move || {
                        record.lock().expect("#1329 record mutex").logs.len() >= count as usize
                    })
                    .await;
                } else if let Some(ticks) = step["ticks"].as_u64() {
                    for _ in 0..ticks {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                } else {
                    panic!("#1329 wait without condition");
                }
            }
            other => panic!("#1329 unknown op {other}"),
        }
    }
    router.abort();
    // Quiescence, mirroring the generator's post-case settle: a late retry
    // must land in this case's cells (a mismatch), never in the next case's.
    tokio::time::sleep(SETTLE).await;
    {
        let settled = record.lock().expect("#1329 record mutex").lark.len();
        tokio::time::sleep(SETTLE).await;
        let record = record.lock().expect("#1329 record mutex");
        crate::negative_assert_eq!(
            record.lark.len(),
            settled,
            "#1329 case {name} lark calls stop arriving after the case ends"
        );
    }
    lark.disarm();
    record.lock().expect("#1329 record mutex").snapshot()
}

#[tokio::test]
async fn channel_matches_real_ts() {
    let oracle = oracle();
    let lark = Lark::new();
    start_lark_on(lark.clone(), LARK_PORT).await;
    for case in cases(&oracle) {
        let name = case["name"].as_str().expect("#1329 case name");
        let actual = replay(case, &lark).await;
        crate::negative_assert_eq!(
            actual,
            case["expected"],
            "#1329 case {} matches the TS oracle",
            name
        );
    }
    let strays = lark.stray.lock().expect("#1329 stray mutex");
    crate::negative_assert_eq!(
        strays.len(),
        0,
        "#1329 no Lark request arrives outside an armed case"
    );
}

// ---------------------------------------------------------------- pins

/// Diff 007 / A8.3-CH-FEISHU-007: a top-level `null` body (the four literal
/// characters `null`) throws an uncaught TypeError in the TS channel reading
/// `raw.token`, so the golden fixture cannot carry a cell for it. The port
/// treats it as an ignored event: the full POST path answers
/// `{"ok":true}` and nothing ever enters the pipeline.
#[tokio::test]
async fn top_level_null_body_is_answered_ok_and_never_dispatches() {
    let record = Arc::new(Mutex::new(Record::default()));
    let lark = Lark::new();
    lark.arm_script(Vec::new(), record.clone());
    start_lark_on(lark.clone(), NULL_PIN_LARK_PORT).await;
    let gateway = MockInbound::new(Vec::new(), record.clone(), Gates::default());
    let (channel, slot) = case_channel(NULL_PIN_LARK_PORT);
    let deps = case_deps(&record, gateway);
    let router = start_router(slot, NULL_PIN_ROUTER_PORT).await;
    bounded("null body start", channel.start(deps))
        .await
        .expect("#1329 start");

    let reply = http_request(
        "null body POST",
        NULL_PIN_ROUTER_PORT,
        "POST",
        "/feishu/webhook",
        Some("null"),
    )
    .await;
    crate::negative_assert_eq!(
        reply,
        json!({"status": 200, "contentType": "application/json", "body": "{\"ok\":true}"}),
        "#1329 a top-level null body answers ok without dispatching"
    );
    tokio::time::sleep(SETTLE).await;
    let record = record.lock().expect("#1329 record mutex");
    crate::negative_assert_eq!(
        record.gateway.len(),
        0,
        "#1329 a top-level null body never reaches submitTurn"
    );
    crate::negative_assert_eq!(
        record.lark.len(),
        0,
        "#1329 a top-level null body triggers no outbound Lark call"
    );
    router.abort();
}

/// The 2000-event dedupe ring evicts the oldest id, so a re-delivery of the
/// evicted id is processed again while a still-resident sibling stays
/// deduped. The whitespace text keeps every processed event from reaching
/// the gateway, so the pin isolates the ring alone.
#[tokio::test]
async fn seen_events_ring_evicts_the_oldest_id_after_2000() {
    let record = Arc::new(Mutex::new(Record::default()));
    let lark = Lark::new();
    lark.arm_script(Vec::new(), record.clone());
    start_lark_on(lark.clone(), DEDUP_PIN_LARK_PORT).await;
    let gateway = MockInbound::new(Vec::new(), record.clone(), Gates::default());
    let (channel, slot) = case_channel(DEDUP_PIN_LARK_PORT);
    let deps = case_deps(&record, gateway);
    let handle = bounded("dedup ring start", channel.start(deps))
        .await
        .expect("#1329 start");
    let handler = slot.handler().expect("#1329 webhook handler");

    for index in 0..2000u32 {
        let (status, _) = bounded(
            "dedup ring fill",
            handler(v2_event(
                &format!("ev-{index}"),
                "c1",
                " ",
                Some("om-dedup"),
            )),
        )
        .await;
        crate::negative_assert_eq!(
            status,
            200,
            "#1329 dedup ring fill event {index} is acknowledged"
        );
    }
    // The 2001st distinct id evicts `ev-0`, the oldest.
    let (status, _) = bounded(
        "dedup ring evict",
        handler(v2_event("ev-new", "c1", " ", None)),
    )
    .await;
    crate::negative_assert_eq!(status, 200, "#1329 the evicting event is acknowledged");

    // The still-resident sibling must be checked first: re-posting the
    // evicted id re-inserts it, which would evict `ev-1` in turn and turn
    // the second probe stale.
    let (status, body) = bounded(
        "dedup ring resident",
        handler(v2_event("ev-1", "c1", " ", None)),
    )
    .await;
    crate::negative_assert_eq!(status, 200, "#1329 the resident id is acknowledged");
    crate::negative_assert_eq!(
        body,
        "{\"ok\":true,\"deduped\":true}",
        "#1329 a still-resident id keeps its dedupe reply"
    );
    let (status, body) = bounded(
        "dedup ring replay",
        handler(v2_event("ev-0", "c1", " ", None)),
    )
    .await;
    crate::negative_assert_eq!(status, 200, "#1329 the evicted id is reprocessed");
    crate::negative_assert_eq!(
        body,
        "{\"ok\":true}",
        "#1329 the evicted id is no longer reported deduped"
    );

    tokio::time::sleep(SETTLE).await;
    // The guard must not live across the stop await: `stop` logs through the
    // same `record` mutex, and a sync lock inside a future's poll freezes
    // the current-thread runtime so even the 20s `bounded` deadline cannot
    // fire.
    {
        let record = record.lock().expect("#1329 record mutex");
        crate::negative_assert_eq!(
            record.gateway.len(),
            0,
            "#1329 whitespace-only events never reach submitTurn"
        );
        crate::negative_assert_eq!(
            record.lark.len(),
            0,
            "#1329 whitespace-only events trigger no outbound Lark call"
        );
    }
    let _ = bounded("dedup ring stop", handle.stop("done")).await;
}

/// A `99991663` from the messages POST drops the cached tenant token, so the
/// text fallback re-fetches a fresh one: the recorded sequence shows two
/// token POSTs and the second message send carries the new bearer. The
/// token POST itself is the only unauthenticated Lark call.
#[tokio::test]
async fn token_invalid_code_drops_the_cached_token() {
    let record = Arc::new(Mutex::new(Record::default()));
    let lark = Lark::new();
    lark.arm_script(
        vec![
            (
                200,
                "{\"code\":0,\"tenant_access_token\":\"t-1\",\"expire\":7200}".to_owned(),
            ),
            (
                200,
                "{\"code\":0,\"data\":{\"reaction_id\":\"re-1\"}}".to_owned(),
            ),
            (200, "{\"code\":99991663,\"msg\":\"bad token\"}".to_owned()),
            (
                200,
                "{\"code\":0,\"tenant_access_token\":\"t-2\",\"expire\":7200}".to_owned(),
            ),
            (
                200,
                "{\"code\":0,\"data\":{\"message_id\":\"om-9\"}}".to_owned(),
            ),
        ],
        record.clone(),
    );
    start_lark_on(lark.clone(), TOKEN_PIN_LARK_PORT).await;
    let gateway = MockInbound::new(
        vec![TurnScript {
            gate_before: None,
            gate_each: None,
            events: vec![json!({"type": "assistant_text_delta", "text": "回复"})],
            error: None,
        }],
        record.clone(),
        Gates::default(),
    );
    let (channel, slot) = case_channel(TOKEN_PIN_LARK_PORT);
    let deps = case_deps(&record, gateway);
    let handle = bounded("token pin start", channel.start(deps))
        .await
        .expect("#1329 start");
    let handler = slot.handler().expect("#1329 webhook handler");

    let (status, body) = bounded(
        "token pin event",
        handler(v2_event("ev-tok", "c9", "hi", Some("om-in-9"))),
    )
    .await;
    crate::negative_assert_eq!(status, 200, "#1329 the token pin event is acknowledged");
    crate::negative_assert_eq!(
        body,
        "{\"ok\":true}",
        "#1329 the token pin event is fresh, not deduped"
    );
    let record_reader = record.clone();
    until(
        "#1329 the invalid token costs exactly one extra token POST",
        "token pin outbound",
        move || record_reader.lock().expect("#1329 record mutex").lark.len() >= 6,
    )
    .await;

    let entries = record.lock().expect("#1329 record mutex").lark.clone();
    crate::negative_assert_eq!(
        entries.len(),
        6,
        "#1329 the invalid token costs exactly one extra token POST"
    );
    const TOKEN_PATH: &str = "/open-apis/auth/v3/tenant_access_token/internal";
    crate::negative_assert_eq!(
        entries[0]["path"],
        json!(TOKEN_PATH),
        "#1329 the first Lark call fetches the tenant token"
    );
    crate::negative_assert_eq!(
        entries[0]["auth"],
        Value::Null,
        "#1329 the token POST carries no bearer"
    );
    crate::negative_assert_eq!(
        entries[2]["auth"],
        json!("Bearer t-1"),
        "#1329 the failed post carried the first token"
    );
    crate::negative_assert_eq!(
        entries[3]["path"],
        json!(TOKEN_PATH),
        "#1329 the cache drop forces a second token POST"
    );
    crate::negative_assert_eq!(
        entries[4]["auth"],
        json!("Bearer t-2"),
        "#1329 the text fallback carried the refreshed token"
    );
    crate::negative_assert_eq!(
        entries[5]["method"],
        json!("DELETE"),
        "#1329 the turn closes with the reaction DELETE"
    );
    crate::negative_assert_eq!(
        entries[5]["auth"],
        json!("Bearer t-2"),
        "#1329 the reaction DELETE also uses the refreshed token"
    );
    let logs = record.lock().expect("#1329 record mutex").logs.clone();
    crate::negative_assert!(
        logs.iter().any(|entry| entry
            == &json!([
                "warn",
                "feishu: send post failed code=99991663 msg=bad token"
            ])),
        "#1329 the failed post is logged with its Lark code and msg"
    );
    let _ = bounded("token pin stop", handle.stop("done")).await;
}

/// Losing the `ActiveChat` admission race must not drop the turn: the running
/// drain already holds the chat slot, so `run_queued_turns` has to append to
/// the chat queue and wait for that drain (there is no separate "is the chat
/// active?" pre-check above it). The real window (admission checked, guard
/// taken, drain already running) is not stably reproducible, so the pin
/// pre-takes the slot and calls the drain entry directly: the turn must still
/// be in the queue and nothing may run.
#[tokio::test]
async fn losing_the_admission_race_requeues_the_turn() {
    let (channel, _slot) = case_channel(LARK_PORT);
    let chat_id = "race-chat";
    assert!(
        channel
            .runtime
            .active_chats
            .lock()
            .expect("#1329 active chats mutex")
            .insert(chat_id.to_owned()),
        "#1329 the pin takes the admission slot itself"
    );

    let turn = QueuedTurn {
        session_key: format!("feishu:chat={chat_id}:general"),
        message: "queued after the race".to_owned(),
        project_key: None,
        message_id: None,
        generation: 0,
    };
    channel.runtime.run_queued_turns(chat_id, turn).await;

    let state = channel.runtime.lock_state();
    let entry = state.chats.get(chat_id).expect("#1329 chat entry");
    crate::negative_assert_eq!(
        entry.queue.len(),
        1,
        "#1329 the admission race loser is queued, never dropped"
    );
    crate::negative_assert_eq!(
        entry.queue.front().expect("#1329 queued turn").message,
        "queued after the race",
        "#1329 the queued turn is the one that lost the race"
    );
    crate::negative_assert!(
        entry.active_run.is_none(),
        "#1329 the race loser does not start a run"
    );
}

/// The per-chat turn queue is not decoration: a turn that had to wait for the
/// running one must be picked up by the drainer when it finishes, in the same
/// order the messages were accepted. The live window (two inbound batches,
/// `/new` replacing the batch under a running drain) is not stably
/// reproducible, so the pin builds the state directly: two turns queue while
/// the chat is busy, the busy turn releases, and the next drainer must run the
/// live turn and then both queued ones.
#[tokio::test]
async fn queued_turns_drain_after_the_active_turn() {
    let record = Arc::new(Mutex::new(Record::default()));
    let gateway = MockInbound::new(
        vec![
            TurnScript::default(),
            TurnScript::default(),
            TurnScript::default(),
        ],
        record.clone(),
        Gates::default(),
    );
    let (channel, _slot) = case_channel(LARK_PORT);
    let deps = case_deps(&record, gateway);
    let handle = bounded("queue drain start", channel.start(deps))
        .await
        .expect("#1329 start");
    let chat_id = "queue-chat";

    let turn = |message: &str| QueuedTurn {
        session_key: format!("feishu:chat={chat_id}:general"),
        message: message.to_owned(),
        project_key: None,
        message_id: None,
        generation: 0,
    };

    // The busy turn holds the admission slot; both arrivals lose it.
    channel
        .runtime
        .active_chats
        .lock()
        .expect("#1329 active chats mutex")
        .insert(chat_id.to_owned());
    channel
        .runtime
        .run_queued_turns(chat_id, turn("queued-1"))
        .await;
    channel
        .runtime
        .run_queued_turns(chat_id, turn("queued-2"))
        .await;
    {
        let state = channel.runtime.lock_state();
        crate::negative_assert_eq!(
            state.chats.get(chat_id).map(|entry| entry.queue.len()),
            Some(2),
            "#1329 both arrivals wait in the chat queue"
        );
    }

    // The running turn ends (its drainer gives the slot back while looking for
    // the next turn), and the next message becomes the drainer.
    channel
        .runtime
        .active_chats
        .lock()
        .expect("#1329 active chats mutex")
        .remove(chat_id);
    channel
        .runtime
        .run_queued_turns(chat_id, turn("live"))
        .await;

    let messages: Vec<String> = record
        .lock()
        .expect("#1329 record mutex")
        .gateway
        .iter()
        .map(|entry| entry["message"].as_str().unwrap_or_default().to_owned())
        .collect();
    crate::negative_assert_eq!(
        messages,
        vec![
            "live".to_owned(),
            "queued-1".to_owned(),
            "queued-2".to_owned()
        ],
        "#1329 the drainer runs the queued turns after the live one"
    );
    {
        let state = channel.runtime.lock_state();
        crate::negative_assert!(
            state
                .chats
                .get(chat_id)
                .is_none_or(|entry| entry.queue.is_empty()),
            "#1329 the drained queue is empty"
        );
    }
    crate::negative_assert!(
        !channel
            .runtime
            .active_chats
            .lock()
            .expect("#1329 active chats mutex")
            .contains(chat_id),
        "#1329 the drainer gives the admission back"
    );
    let _ = bounded("queue drain stop", handle.stop("done")).await;
}

/// A batch timer superseded by a newer message must return without taking the
/// messages and without claiming `draining`: the live timer drains them. This
/// is the pin for the reason `clearTimeout` is a sequence bump here instead of
/// `JoinHandle::abort` — abort only lands at the task's next yield point, and
/// for a timer that already woke up and is blocked on the state lock that
/// point is inside this drain, after `mem::take` would have dropped the
/// messages and left `draining` set with no drainer to clear it.
#[tokio::test]
async fn superseded_batch_timer_does_not_drain() {
    const CHAT: &str = "c-timer";
    let (channel, _slot) = case_channel(0);
    {
        let mut state = channel.runtime.lock_state();
        let batch = Batch {
            id: 7,
            timer_seq: 3,
            ..Batch::default()
        };
        state.batches.insert(CHAT.to_owned(), batch);
        let batch = state.batches.get_mut(CHAT).expect("#1329 batch entry");
        batch.messages.push(InboundMessage {
            chat_id: CHAT.to_owned(),
            text: "first".to_owned(),
            event_id: "ev-timer-1".to_owned(),
            message_id: Some("om_timer_1".to_owned()),
            message_type: Some("text".to_owned()),
            content: None,
        });
    }
    // Stale sequence (the batch is on 3): nothing is taken, nothing is claimed.
    channel.runtime.drain_inbound_batch(CHAT, 7, 2).await;
    {
        let state = channel.runtime.lock_state();
        let batch = state.batches.get(CHAT).expect("#1329 batch kept");
        assert_eq!(
            batch.messages.len(),
            1,
            "a superseded timer must not take the queued messages"
        );
        assert!(
            !batch.draining,
            "a superseded timer must not claim the drain"
        );
    }
    // Stale batch id (a `/new` replacement): the same no-op.
    channel.runtime.drain_inbound_batch(CHAT, 6, 3).await;
    assert!(
        channel
            .runtime
            .lock_state()
            .batches
            .get(CHAT)
            .is_some_and(|batch| batch.messages.len() == 1 && !batch.draining),
        "a timer from a replaced batch must not touch the new one"
    );
    // The live timer drains and drops the batch (no gateway: the group is
    // taken and dropped, which is what a turn without a gateway does).
    channel.runtime.drain_inbound_batch(CHAT, 7, 3).await;
    assert!(
        !channel.runtime.lock_state().batches.contains_key(CHAT),
        "the live timer finishes the batch and removes the entry"
    );
}

/// `/new <正文>` resets the session and then submits the text on the *new*
/// session key (TS returns early only when `!mapped.message`). The mapper pin
/// covers the parse; this one covers the channel actually reaching the
/// gateway with the payload and not answering the bare confirmation instead.
#[tokio::test]
async fn new_command_with_text_submits_on_the_new_session() {
    // 18960-18972 belong to the oracle replay, 18973/18990-18993 to the other
    // pins, 18995-18998 to the wecom_callback suite: stay clear of all of them
    // so a parallel full-suite run cannot collide on bind.
    const NEW_TEXT_PIN_LARK_PORT: u16 = 18980;
    let record = Arc::new(Mutex::new(Record::default()));
    let lark = Lark::new();
    lark.arm_script(
        vec![
            (
                200,
                "{\"code\":0,\"tenant_access_token\":\"t-1\",\"expire\":7200}".to_owned(),
            ),
            (
                200,
                "{\"code\":0,\"data\":{\"reaction_id\":\"re-1\"}}".to_owned(),
            ),
            (
                200,
                "{\"code\":0,\"data\":{\"message_id\":\"om-out\"}}".to_owned(),
            ),
        ],
        record.clone(),
    );
    start_lark_on(lark.clone(), NEW_TEXT_PIN_LARK_PORT).await;
    let gateway = MockInbound::new(
        vec![TurnScript {
            gate_before: None,
            gate_each: None,
            events: vec![json!({"type": "assistant_text_delta", "text": "总结完毕"})],
            error: None,
        }],
        record.clone(),
        Gates::default(),
    );
    let (channel, slot) = case_channel(NEW_TEXT_PIN_LARK_PORT);
    let deps = case_deps(&record, gateway);
    let handle = bounded("new text start", channel.start(deps))
        .await
        .expect("#1329 start");
    let handler = slot.handler().expect("#1329 webhook handler");

    let (status, body) = bounded(
        "new text event",
        handler(v2_event(
            "ev-new-text",
            "c7",
            "/new 请总结",
            Some("om-in-7"),
        )),
    )
    .await;
    crate::negative_assert_eq!(status, 200, "#1329 the /new command is acknowledged");
    crate::negative_assert_eq!(body, "{\"ok\":true}", "#1329 the /new command is accepted");
    until(
        "#1329 /new with text submits the payload on the new session",
        "new text outbound",
        || record.lock().expect("#1329 record mutex").lark.len() >= 4,
    )
    .await;

    let submitted = record.lock().expect("#1329 record mutex").gateway.clone();
    crate::negative_assert_eq!(
        submitted,
        vec![json!({
            "sessionKey": "feishu:chat=c7:s_<uuid>",
            "channelKey": "feishu",
            "message": "请总结",
        })],
        "#1329 /new with text submits exactly the stripped payload"
    );
    let lark_entries = record.lock().expect("#1329 record mutex").lark.clone();
    crate::negative_assert!(
        lark_entries.iter().all(|entry| !entry["body"]
            .as_str()
            .unwrap_or_default()
            .contains("已创建新会话。")),
        "#1329 /new with text does not answer the bare confirmation"
    );
    let _ = bounded("new text stop", handle.stop("done")).await;
}

use super::super::im_interaction::{InteractionFuture, PermissionAnswer as PortPermissionAnswer};

// ---------------------------------------------------------------- #2055 pins

/// #2055-2: the elicitation branch's occupancy. TS consumes the pending
/// entry synchronously inside `answer()`, so at most one answer task ever
/// exists per pending elicitation. The pending entry here lives behind the
/// interaction port, so the channel holds the answering occupancy instead —
/// and the release at the end of the answer is what keeps the next event
/// from being swallowed forever.
#[derive(Default)]
struct ElicitationScript {
    /// Every `elicitation_answer` invocation, pending or not — the count is
    /// the property under test.
    answers: Mutex<Vec<String>>,
    pending: std::sync::atomic::AtomicBool,
    release: tokio::sync::watch::Sender<bool>,
}

impl ElicitationScript {
    fn armed() -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(Vec::new()),
            pending: std::sync::atomic::AtomicBool::new(true),
            release: tokio::sync::watch::Sender::new(false),
        })
    }

    /// No pending entry: inbound text flows to the gateway as a turn (F1 needs
    /// the gated turn to start, unlike F2 where the text is the answer).
    fn idle() -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(Vec::new()),
            pending: std::sync::atomic::AtomicBool::new(false),
            release: tokio::sync::watch::Sender::new(false),
        })
    }
}

impl InteractionPort for ElicitationScript {
    fn elicitation_pending(&self, _chat_id: &str) -> bool {
        self.pending.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn elicitation_capture(&self, _: &str, _: &str, event: &JsValue) -> String {
        // The question comes from the event, so a scripted case can ask a
        // marker question while the turn is current and the stale one after.
        // A question a stale turn must never ask (F1): unlike buffered text
        // it is sent the moment its event passes the per-event gate.
        event
            .get("question")
            .and_then(JsValue::as_string)
            .map(|question| String::from_utf16_lossy(question.units()))
            .unwrap_or_else(|| "STALE ELICITATION QUESTION".to_owned())
    }
    fn elicitation_answer<'a>(
        &'a self,
        _chat_id: &'a str,
        text: &'a str,
    ) -> InteractionFuture<'a, Result<Option<String>, ChannelError>> {
        // TS-faithful shape: the take happens before the first await, so a
        // second call while an answer is in flight finds nothing.
        let taken = self
            .pending
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        self.answers
            .lock()
            .expect("#2055 answers lock")
            .push(format!("{taken}:{text}"));
        let release = self.release.clone();
        Box::pin(async move {
            if !taken {
                return Ok(None);
            }
            let mut gate = release.subscribe();
            while !*gate.borrow_and_update() {
                gate.changed().await.expect("#2055 release retained");
            }
            Ok(None)
        })
    }
    fn elicitation_clear(&self, _chat_id: &str) {}

    fn permission_pending(&self, _: &str) -> bool {
        false
    }
    fn permission_capture(&self, _: &str, _: &str, _: &JsValue) -> Option<String> {
        None
    }
    fn permission_confirm_initial(&self, _: &str, _: bool, _: Option<&str>) {}
    fn permission_answer<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> InteractionFuture<'a, Result<Option<PortPermissionAnswer>, ChannelError>> {
        Box::pin(async { Ok(None) })
    }
    fn permission_release_answer(&self, _: &str, _: Option<u64>) {}
    fn permission_take_next_prompt(&self, _: &str, _: Option<u64>) -> Option<String> {
        None
    }
    fn permission_prompt_request_id(&self, _: &str, _: Option<u64>) -> Option<String> {
        None
    }
    fn permission_confirm_next(&self, _: &str, _: bool, _: Option<&str>, _: Option<u64>) {}
    fn permission_clear_after_turn(&self, _: &str) {}
}

/// #2775 fix-r4 (review P1-2): per-answer one-shot gates — the stale-release
/// pin below needs the old answer to finish while the new one stays parked,
/// which the shared release watch of [`ElicitationScript`] cannot express.
/// `finish(0)` ends the first answer only; `completed` records each answer
/// that came back, in order — the release that follows it runs in the same
/// task poll (no await between the gate and the token check), so observing
/// a completion means its release has run too.
struct StaleSeatScript {
    answers: Mutex<Vec<String>>,
    completed: Arc<Mutex<Vec<String>>>,
    pending: std::sync::atomic::AtomicBool,
    gates: Mutex<Vec<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl StaleSeatScript {
    fn armed() -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(Vec::new()),
            completed: Arc::new(Mutex::new(Vec::new())),
            pending: std::sync::atomic::AtomicBool::new(true),
            gates: Mutex::new(Vec::new()),
        })
    }

    /// End the parked answer `index` (0 = the first one that started).
    fn finish(&self, index: usize) {
        self.gates
            .lock()
            .expect("#2775 gates lock")
            .get_mut(index)
            .expect("#2775 gate exists")
            .take()
            .expect("#2775 gate not reused")
            .send(())
            .expect("#2775 gate receiver retained");
    }
}

impl InteractionPort for StaleSeatScript {
    fn elicitation_pending(&self, _chat_id: &str) -> bool {
        self.pending.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn elicitation_capture(&self, _: &str, _: &str, event: &JsValue) -> String {
        event
            .get("question")
            .and_then(JsValue::as_string)
            .map(|question| String::from_utf16_lossy(question.units()))
            .unwrap_or_else(|| "STALE ELICITATION QUESTION".to_owned())
    }
    fn elicitation_answer<'a>(
        &'a self,
        _chat_id: &'a str,
        text: &'a str,
    ) -> InteractionFuture<'a, Result<Option<String>, ChannelError>> {
        // TS-faithful shape: the take happens before the first await, so a
        // second call while an answer is in flight finds nothing.
        let taken = self
            .pending
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        self.answers
            .lock()
            .expect("#2775 answers lock")
            .push(format!("{taken}:{text}"));
        if !taken {
            return Box::pin(async { Ok(None) });
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.gates
            .lock()
            .expect("#2775 gates lock")
            .push(Some(sender));
        let text = text.to_owned();
        let completed = self.completed.clone();
        Box::pin(async move {
            let _ = receiver.await;
            completed.lock().expect("#2775 completed lock").push(text);
            Ok(None)
        })
    }
    fn elicitation_clear(&self, _chat_id: &str) {}

    fn permission_pending(&self, _: &str) -> bool {
        false
    }
    fn permission_capture(&self, _: &str, _: &str, _: &JsValue) -> Option<String> {
        None
    }
    fn permission_confirm_initial(&self, _: &str, _: bool, _: Option<&str>) {}
    fn permission_answer<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> InteractionFuture<'a, Result<Option<PortPermissionAnswer>, ChannelError>> {
        Box::pin(async { Ok(None) })
    }
    fn permission_release_answer(&self, _: &str, _: Option<u64>) {}
    fn permission_take_next_prompt(&self, _: &str, _: Option<u64>) -> Option<String> {
        None
    }
    fn permission_prompt_request_id(&self, _: &str, _: Option<u64>) -> Option<String> {
        None
    }
    fn permission_confirm_next(&self, _: &str, _: bool, _: Option<&str>, _: Option<u64>) {}
    fn permission_clear_after_turn(&self, _: &str) {}
}

/// #2055-3: the permission answer panics (standing in for any panic inside
/// the answer task); the token release must still run (TS `.finally`).
struct PanickingPermissionPort {
    calls: Mutex<Vec<String>>,
}

impl InteractionPort for PanickingPermissionPort {
    fn permission_pending(&self, _chat_id: &str) -> bool {
        true
    }
    fn permission_capture(&self, _: &str, _: &str, _: &JsValue) -> Option<String> {
        None
    }
    fn permission_confirm_initial(&self, _: &str, _: bool, _: Option<&str>) {}
    fn permission_answer<'a>(
        &'a self,
        _chat_id: &'a str,
        text: &'a str,
    ) -> InteractionFuture<'a, Result<Option<PortPermissionAnswer>, ChannelError>> {
        self.calls
            .lock()
            .expect("#2055 calls lock")
            .push(text.to_owned());
        Box::pin(async {
            panic!("#2055 injected permission answer panic");
        })
    }
    fn permission_release_answer(&self, _: &str, _: Option<u64>) {}
    fn permission_take_next_prompt(&self, _: &str, _: Option<u64>) -> Option<String> {
        None
    }
    fn permission_prompt_request_id(&self, _: &str, _: Option<u64>) -> Option<String> {
        None
    }
    fn permission_confirm_next(&self, _: &str, _: bool, _: Option<&str>, _: Option<u64>) {}
    fn permission_clear_after_turn(&self, _: &str) {}

    fn elicitation_pending(&self, _: &str) -> bool {
        false
    }
    fn elicitation_capture(&self, _: &str, _: &str, _: &JsValue) -> String {
        String::new()
    }
    fn elicitation_answer<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> InteractionFuture<'a, Result<Option<String>, ChannelError>> {
        Box::pin(async { Ok(None) })
    }
    fn elicitation_clear(&self, _: &str) {}
}

/// The channel seam variant with a scripted interaction port (the parity
/// cases pin `no_interaction()`; the #2055 pins need the helpers).
fn case_channel_with_interaction(
    lark_port: u16,
    port: Arc<dyn InteractionPort>,
) -> (FeishuChannel, Arc<FeishuWebhookSlot>) {
    let slot = Arc::new(FeishuWebhookSlot::new());
    let channel = FeishuChannel::with_seams(
        FeishuOptions {
            app_id: APP_ID.to_owned(),
            app_secret: APP_SECRET.to_owned(),
            encrypt_key: None,
            verify_token: Some(VERIFY_TOKEN.to_owned()),
            connection_mode: "webhook".to_owned(),
            domain_name: "feishu".to_owned(),
            permission_mode: None,
            base_url: format!("http://127.0.0.1:{lark_port}"),
        },
        port,
        Some(slot.clone()),
        Arc::new(FixedClock),
        Arc::new(|| JsString::from(FIXED_UUID)),
    );
    (channel, slot)
}

fn plain_message(chat: &str, text: &str) -> InboundMessage {
    InboundMessage {
        chat_id: chat.to_owned(),
        text: text.to_owned(),
        event_id: format!("ev-{chat}-{text}"),
        message_id: None,
        message_type: Some("text".to_owned()),
        content: None,
    }
}

/// F1: a turn gated in-flight (submit returned, no event polled yet) that a
/// `/new` supersedes must send nothing for the stale events and still remove
/// the reaction (TS: the per-event `isCurrentTurn()` break plus the
/// unconditional `finally` delete). The stale stream carries two probes: a
/// buffered delta (only the gated final flush could deliver it — it is
/// consumed *while the turn is current*, so the per-event gate alone cannot
/// account for it) and an elicitation whose question is sent the moment its
/// event passes the per-event gate — together they pin both generation
/// checks. The marker elicitation before the delta is the hard proof the
/// delta was consumed before the `/new`: the stream is FIFO, and its
/// question POSTs observably. The `/new` is delivered by calling
/// `process_inbound_messages` directly: through the enqueue path it would
/// only queue behind the chat's busy drainer, and the live window is the
/// second-drainer interleave the `/new` branch's batch replacement creates
/// (the TS probe `p5-feishu-new-gated-turn` reproduces the same interleave).
#[tokio::test]
async fn gated_submit_superseded_by_new_sends_nothing_and_deletes_reaction() {
    // 18980 belongs to the /new-with-text pin; stay clear of it.
    const GATED_PIN_LARK_PORT: u16 = 18981;
    let record = Arc::new(Mutex::new(Record::default()));
    let lark = Lark::new();
    lark.arm_script(
        vec![
            (
                200,
                "{\"code\":0,\"tenant_access_token\":\"t-1\",\"expire\":7200}".to_owned(),
            ),
            (
                200,
                "{\"code\":0,\"data\":{\"reaction_id\":\"re-1\"}}".to_owned(),
            ),
        ],
        record.clone(),
    );
    start_lark_on(lark.clone(), GATED_PIN_LARK_PORT).await;
    let gates = Gates::default();
    let gateway = MockInbound::new(
        vec![TurnScript {
            gate_before: None,
            gate_each: Some("f1-turn".to_owned()),
            events: vec![
                json!({"type": "assistant_text_delta", "text": "STALE OUTPUT"}),
                json!({"type": "elicitation_request", "question": "MARKER QUESTION"}),
                json!({"type": "elicitation_request", "question": "STALE ELICITATION QUESTION"}),
            ],
            error: None,
        }],
        record.clone(),
        gates.clone(),
    );
    let (channel, slot) =
        case_channel_with_interaction(GATED_PIN_LARK_PORT, ElicitationScript::idle());
    let deps = case_deps(&record, gateway);
    let handle = bounded("f1 start", channel.start(deps))
        .await
        .expect("#2055 start");
    let handler = slot.handler().expect("#2055 webhook handler");

    let (status, _) = bounded(
        "f1 event",
        handler(v2_event("ev-f1", "c-f1", "hello", Some("om-f1"))),
    )
    .await;
    crate::negative_assert_eq!(status, 200, "#2055 the gated turn's message is accepted");
    // The turn is in flight once its reaction is on the message.
    until(
        "#2055 the gated turn is in flight before the /new",
        "f1 reaction added",
        || record.lock().expect("#2055 record mutex").lark.len() >= 2,
    )
    .await;

    // Stage the stale delta: it is consumed while the turn is still current,
    // so it lands in the live-reply buffer and only the gated final flush
    // could ever deliver it. The marker question right behind it (FIFO) is
    // the observable proof of the consumption.
    gate(&gates, "f1-turn-0").send_replace(true);
    gate(&gates, "f1-turn-1").send_replace(true);
    until(
        "#2055 the gated turn buffers its stale text before the /new",
        "f1 marker question asked",
        || {
            record
                .lock()
                .expect("#2055 record mutex")
                .lark
                .iter()
                .any(|entry| {
                    entry["body"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("MARKER QUESTION")
                })
        },
    )
    .await;

    // The /new supersedes the gated turn (second-drainer interleave).
    channel
        .runtime
        .process_inbound_messages(&[plain_message("c-f1", "/new")])
        .await;
    let submitted = record.lock().expect("#2055 record mutex").gateway.clone();
    crate::negative_assert_eq!(
        submitted,
        vec![json!({
            "sessionKey": "feishu:chat=c-f1:general",
            "channelKey": "feishu",
            "message": "hello",
        })],
        "#2055 the /new carries no follow-up text, so only the gated turn was submitted"
    );

    // Release the stale elicitation: it must be gated before any send.
    gate(&gates, "f1-turn-2").send_replace(true);
    until(
        "#2055 a superseded turn still removes its reaction",
        "f1 reaction removed",
        || {
            record
                .lock()
                .expect("#2055 record mutex")
                .lark
                .iter()
                .any(|entry| entry["method"] == "DELETE")
        },
    )
    .await;

    let lark_entries = record.lock().expect("#2055 record mutex").lark.clone();
    crate::negative_assert!(
        lark_entries.iter().all(|entry| !entry["body"]
            .as_str()
            .unwrap_or_default()
            .contains("STALE ELICITATION QUESTION")),
        "#2055 the superseded turn's stale elicitation question is never asked"
    );
    crate::negative_assert!(
        lark_entries
            .iter()
            .all(|entry| !entry["body"].as_str().unwrap_or_default().contains("STALE")),
        "#2055 the superseded turn's stale events send nothing"
    );
    let confirmations = lark_entries
        .iter()
        .filter(|entry| {
            entry["body"]
                .as_str()
                .unwrap_or_default()
                .contains("已创建新会话。")
        })
        .count();
    crate::negative_assert_eq!(
        confirmations,
        1,
        "#2055 the /new answers exactly the bare confirmation"
    );
    let _ = bounded("f1 stop", handle.stop("done")).await;
}

/// F2: two webhook deliveries racing across threads while the first
/// elicitation answer has not been polled yet. On the current-thread test
/// runtime the spawned task cannot run between two synchronous enqueues, so
/// the second event deterministically sees the same pending entry — the
/// occupancy is the only thing standing between it and a second answer task.
#[tokio::test]
async fn concurrent_elicitation_events_answer_once() {
    let record = Arc::new(Mutex::new(Record::default()));
    let gateway = MockInbound::new(Vec::new(), record.clone(), Gates::default());
    let script = ElicitationScript::armed();
    let (channel, _slot) = case_channel_with_interaction(0, script.clone());
    let deps = case_deps(&record, gateway);
    let _handle = bounded("f2 start", channel.start(deps))
        .await
        .expect("#2055 start");

    // No await between the two enqueues: the first answer task has not been
    // polled, so both events observe the pending entry.
    channel
        .runtime
        .enqueue_inbound(plain_message("c-f2", "first"));
    channel
        .runtime
        .enqueue_inbound(plain_message("c-f2", "second"));

    until(
        "#2055 one racing elicitation event yields exactly one answer",
        "f2 first answer in flight",
        || {
            !script
                .answers
                .lock()
                .expect("#2055 answers lock")
                .is_empty()
        },
    )
    .await;
    // The first answer parks on the release gate; the second event was
    // consumed by the occupancy (like the permission branch) — no second
    // answer task exists.
    let answers = script.answers.lock().expect("#2055 answers lock").clone();
    crate::negative_assert_eq!(
        answers,
        vec!["true:first".to_owned()],
        "#2055 the racing second event does not start a second answer"
    );

    // The release at the end of the answer is what re-arms the chat: after
    // the gate opens and the answer ends, a fresh pending entry admits a
    // new answer.
    script.release.send_replace(true);
    until(
        "#2055 the elicitation occupancy is released when the answer ends",
        "f2 occupancy released",
        || {
            channel
                .runtime
                .lock_state()
                .elicitation_answering
                .is_empty()
        },
    )
    .await;
    script
        .pending
        .store(true, std::sync::atomic::Ordering::SeqCst);
    channel
        .runtime
        .enqueue_inbound(plain_message("c-f2", "third"));
    until(
        "#2055 the next event after the answer is not swallowed",
        "f2 third answer",
        || script.answers.lock().expect("#2055 answers lock").len() >= 2,
    )
    .await;
}

/// #2775 fix-r4 (review P1-2, DS concurrency#1): a `/new` landing between two
/// elicitation answers must not let the old answer's release delete the new
/// answer's seat. TS cannot hit the window — `answer` consumes the pending
/// entry synchronously before its first await, so no second answer task for
/// one pending entry ever exists — but the Rust seat survives the reset, so
/// an unconditional release would strand the occupancy the wrong way round
/// and let a third event start a second answer task for the same pending
/// entry. The permission branch guards the same window with its token
/// (`permission_answering.get(&chat_id) == Some(&token)`); this pin is the
/// elicitation mirror.
#[tokio::test]
async fn stale_elicitation_release_keeps_the_new_answers_seat() {
    let record = Arc::new(Mutex::new(Record::default()));
    let gateway = MockInbound::new(Vec::new(), record.clone(), Gates::default());
    let script = StaleSeatScript::armed();
    let (channel, _slot) = case_channel_with_interaction(0, script.clone());
    let deps = case_deps(&record, gateway);
    let _handle = bounded("stale-seat start", channel.start(deps))
        .await
        .expect("#2055 start");

    // The old answer takes the seat and parks on its own gate.
    channel
        .runtime
        .enqueue_inbound(plain_message("c-stale", "old"));
    until(
        "#2775 the old elicitation answer is in flight",
        "stale-seat old answer in flight",
        || *script.answers.lock().expect("#2775 answers lock") == vec!["true:old".to_owned()],
    )
    .await;

    // The `/new`: clears the seat and bumps the generation while the old
    // answer is still parked (the interleave under review).
    channel.runtime.reset_chat_interaction_state("c-stale");

    // A fresh pending entry and a new event: the new answer re-takes the seat.
    script
        .pending
        .store(true, std::sync::atomic::Ordering::SeqCst);
    channel
        .runtime
        .enqueue_inbound(plain_message("c-stale", "new"));
    until(
        "#2775 the new elicitation answer is in flight",
        "stale-seat new answer in flight",
        || {
            *script.answers.lock().expect("#2775 answers lock")
                == vec!["true:old".to_owned(), "true:new".to_owned()]
        },
    )
    .await;

    // Finish only the old answer: its release must leave the new seat alone.
    script.finish(0);
    until(
        "#2775 the stale old answer finishes before the new one",
        "stale-seat old answer finished",
        || *script.completed.lock().expect("#2775 completed lock") == vec!["old".to_owned()],
    )
    .await;
    crate::negative_assert!(
        channel
            .runtime
            .lock_state()
            .elicitation_answering
            .contains_key("c-stale"),
        "#2775 the stale old-answer release must not delete the new answer's seat"
    );

    // The seat is what swallows a third event while the new answer waits:
    // without the guard that event would start a second answer task for the
    // same pending entry — the TS-impossible double answer.
    script
        .pending
        .store(true, std::sync::atomic::Ordering::SeqCst);
    channel
        .runtime
        .enqueue_inbound(plain_message("c-stale", "third"));
    tokio::time::sleep(Duration::from_millis(10)).await;
    crate::negative_assert_eq!(
        script.answers.lock().expect("#2775 answers lock").clone(),
        vec!["true:old".to_owned(), "true:new".to_owned()],
        "#2775 a third event while the new answer waits starts no second answer"
    );

    // Releasing the new answer re-arms the chat: the next event is admitted.
    script.finish(1);
    until(
        "#2775 the new answer's release re-arms the chat",
        "stale-seat seat released",
        || {
            channel
                .runtime
                .lock_state()
                .elicitation_answering
                .is_empty()
        },
    )
    .await;
    script
        .pending
        .store(true, std::sync::atomic::Ordering::SeqCst);
    channel
        .runtime
        .enqueue_inbound(plain_message("c-stale", "fourth"));
    until(
        "#2775 the chat recovers after the guarded release",
        "stale-seat fourth answer",
        || script.answers.lock().expect("#2775 answers lock").len() == 3,
    )
    .await;
}

/// F3: a panic inside the permission answer task must still release the
/// token (TS chains the release onto the answer with `.finally`).
#[tokio::test]
async fn panicking_permission_answer_releases_the_token() {
    let record = Arc::new(Mutex::new(Record::default()));
    let gateway = MockInbound::new(Vec::new(), record.clone(), Gates::default());
    let port = Arc::new(PanickingPermissionPort {
        calls: Mutex::new(Vec::new()),
    });
    let (channel, _slot) = case_channel_with_interaction(0, port.clone());
    let deps = case_deps(&record, gateway);
    let _handle = bounded("f3 start", channel.start(deps))
        .await
        .expect("#2055 start");

    channel
        .runtime
        .enqueue_inbound(plain_message("c-f3", "yes"));
    until(
        "#2055 a panicking permission answer still releases the token",
        "f3 token released",
        || {
            !channel
                .runtime
                .lock_state()
                .permission_answering
                .contains_key("c-f3")
        },
    )
    .await;

    // The released token is what admits the next answer: the follow-up
    // event must reach the port instead of being swallowed forever.
    channel
        .runtime
        .enqueue_inbound(plain_message("c-f3", "always"));
    until(
        "#2055 the next permission event after the panic reaches the port",
        "f3 second call",
        || port.calls.lock().expect("#2055 calls lock").len() >= 2,
    )
    .await;
    let calls = port.calls.lock().expect("#2055 calls lock").clone();
    crate::negative_assert_eq!(
        calls,
        vec!["yes".to_owned(), "always".to_owned()],
        "#2055 the chat is not swallowed by the panicked answer's token"
    );
}

/// F4: a panic that unwinds the drainer task must still un-claim the batch
/// (TS `drainInboundBatch` is a try/finally), or the chat's later messages
/// queue behind a claim nobody will ever release.
#[tokio::test]
async fn panicking_drainer_resets_batch_and_chat_recovers() {
    let record = Arc::new(Mutex::new(Record::default()));
    let panicked = Arc::new(tokio::sync::Notify::new());
    let gateway = Arc::new(PanicFirstSubmit {
        record: record.clone(),
        panicked: panicked.clone(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let (channel, _slot) = case_channel(0);
    let deps = case_deps(&record, gateway);
    let _handle = bounded("f4 start", channel.start(deps))
        .await
        .expect("#2055 start");

    channel
        .runtime
        .enqueue_inbound(plain_message("c-f4", "first"));
    tokio::time::timeout(Duration::from_secs(5), panicked.notified())
        .await
        .expect("#2055 the injected drainer panic never fired");
    until(
        "#2055 a panicking drainer un-claims the batch",
        "f4 batch released",
        || {
            channel
                .runtime
                .lock_state()
                .batches
                .get("c-f4")
                .is_none_or(|batch| !batch.draining)
        },
    )
    .await;

    // The recovered chat takes the next message all the way to the gateway.
    channel
        .runtime
        .enqueue_inbound(plain_message("c-f4", "second"));
    until(
        "#2055 the chat recovers after the drainer panic",
        "f4 second submit",
        || record.lock().expect("#2055 record mutex").gateway.len() >= 2,
    )
    .await;
    let submitted: Vec<String> = record
        .lock()
        .expect("#2055 record mutex")
        .gateway
        .iter()
        .map(|entry| entry["message"].as_str().unwrap_or_default().to_owned())
        .collect();
    crate::negative_assert_eq!(
        submitted,
        vec!["first".to_owned(), "second".to_owned()],
        "#2055 both messages reach the gateway across the panic"
    );
}

/// The `submit` fake for F4: the first turn panics inside the drainer task
/// (standing in for any panic under `drain_inbound_batch`); the second
/// returns an already-closed stream.
struct PanicFirstSubmit {
    record: Arc<Mutex<Record>>,
    panicked: Arc<tokio::sync::Notify>,
    calls: std::sync::atomic::AtomicUsize,
}

impl TurnGateway for PanicFirstSubmit {
    fn submit(
        &self,
        input: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.record
            .lock()
            .expect("#2055 record mutex")
            .gateway
            .push(json!({
                "sessionKey": String::from_utf16_lossy(input.session_key.units()),
                "channelKey": input.channel_key.to_string(),
                "message": String::from_utf16_lossy(input.message.units()),
            }));
        if call == 0 {
            self.panicked.notify_one();
            panic!("#2055 injected drainer panic");
        }
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        drop(sender);
        Box::pin(async { Ok(Box::new(MockStream(receiver)) as Box<dyn TurnEventStream>) })
    }
}

/// F5: the `/new` branch's resolve→reset→generation stretch is one critical
/// section (#2055-4; TS keeps it in one synchronous block). The pin plants
/// a racing reset in the `onStateChange` window — the callback runs between
/// the session resolution and the branch's own reset, the spot a concurrent
/// `/new` would land in — and asserts the follow-up turn still carries a
/// generation the chat actually has, so it submits on the new session
/// instead of being gated out as stale.
#[tokio::test]
async fn new_followup_survives_a_reset_racing_in_the_state_change_window() {
    let record = Arc::new(Mutex::new(Record::default()));
    let gateway = MockInbound::new(
        vec![TurnScript::default()],
        record.clone(),
        Gates::default(),
    );
    // The hook needs the runtime, which only exists after construction: a
    // shared cell bridges the two halves.
    let cell: Arc<Mutex<Option<Arc<Runtime>>>> = Arc::new(Mutex::new(None));
    let hook_cell = cell.clone();
    let (channel, slot) = {
        let slot = Arc::new(FeishuWebhookSlot::new());
        let channel = FeishuChannel::with_seams_and_state_change(
            FeishuOptions {
                app_id: APP_ID.to_owned(),
                app_secret: APP_SECRET.to_owned(),
                encrypt_key: None,
                verify_token: Some(VERIFY_TOKEN.to_owned()),
                connection_mode: "webhook".to_owned(),
                domain_name: "feishu".to_owned(),
                permission_mode: None,
                base_url: "http://127.0.0.1:0".to_owned(),
            },
            no_interaction(),
            Some(slot.clone()),
            Arc::new(FixedClock),
            Arc::new(|| JsString::from(FIXED_UUID)),
            Some(Arc::new(move |_| {
                if let Some(runtime) = hook_cell.lock().expect("#2055 runtime cell").as_ref() {
                    runtime.reset_chat_interaction_state("c-race");
                }
            })),
        );
        (channel, slot)
    };
    *cell.lock().expect("#2055 runtime cell") = Some(channel.runtime.clone());
    let deps = case_deps(&record, gateway);
    let handle = bounded("f5 start", channel.start(deps))
        .await
        .expect("#2055 start");
    let handler = slot.handler().expect("#2055 webhook handler");

    let (status, _) = bounded(
        "f5 event",
        handler(v2_event("ev-f5", "c-race", "/new 请总结", None)),
    )
    .await;
    crate::negative_assert_eq!(status, 200, "#2055 the racing /new is accepted");
    until(
        "#2055 the /new follow-up submits despite the racing reset",
        "f5 follow-up submit",
        || {
            !record
                .lock()
                .expect("#2055 record mutex")
                .gateway
                .is_empty()
        },
    )
    .await;

    let submitted = record.lock().expect("#2055 record mutex").gateway.clone();
    crate::negative_assert_eq!(
        submitted,
        vec![json!({
            "sessionKey": "feishu:chat=c-race:s_<uuid>",
            "channelKey": "feishu",
            "message": "请总结",
        })],
        "#2055 the follow-up carries the reset's generation, not the resolved one"
    );
    let _ = bounded("f5 stop", handle.stop("done")).await;
}

/// #2055-3's lock companion: a panic that unwound through a state-lock guard
/// poisons the lock, and the channel must keep working anyway (TS has no
/// poisoning; the weixin lane made the same call for #1780).
#[tokio::test]
async fn poisoned_state_lock_does_not_kill_the_channel() {
    let record = Arc::new(Mutex::new(Record::default()));
    let gateway = MockInbound::new(Vec::new(), record.clone(), Gates::default());
    let (channel, _slot) = case_channel(0);
    let deps = case_deps(&record, gateway);
    let _handle = bounded("poison start", channel.start(deps))
        .await
        .expect("#2055 start");

    let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = channel.runtime.lock_state();
        panic!("#2055 poison the state lock");
    }));
    assert!(
        poison.is_err() && channel.runtime.state.is_poisoned(),
        "#2055 the injected panic poisons the state lock"
    );
    crate::negative_assert_eq!(
        channel.runtime.generation("c-poison"),
        0,
        "#2055 a poisoned state lock does not kill the channel"
    );
}
