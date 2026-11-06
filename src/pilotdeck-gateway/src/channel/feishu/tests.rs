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

async fn until(context: &str, predicate: impl Fn() -> bool) {
    let started = Instant::now();
    while !predicate() {
        crate::negative_assert!(
            started.elapsed() < DEADLINE,
            "#1329 {context} matches the TS oracle"
        );
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
            for event in turn.events {
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
                if let Some(count) = step["lark"].as_u64() {
                    let record = record.clone();
                    until(&format!("{context} wait lark {count}"), move || {
                        record.lock().expect("#1329 record mutex").lark.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["gateway"].as_u64() {
                    let record = record.clone();
                    until(&format!("{context} wait gateway {count}"), move || {
                        record.lock().expect("#1329 record mutex").gateway.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["logs"].as_u64() {
                    let record = record.clone();
                    until(&format!("{context} wait logs {count}"), move || {
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
    until("token pin outbound", move || {
        record_reader.lock().expect("#1329 record mutex").lark.len() >= 6
    })
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
