//! #1336: replay of the `wecom-callback` golden fixture — the same step
//! scripts the TS generator ran against the real channel sources (Node
//! v22.22.0, baseline cc7c4e6), driven through the native hyper listener with
//! the same loopback qyapi fake, a scripted inbound gateway and the same
//! scripted interaction port — plus the component pins (config `Number(port)`
//! semantics, crypto envelopes, UTF-16 signature sort, outbound body
//! truncation) that keep every assertion on a recorded observable.

use super::super::protocol::{TurnEventStream, TurnGateway, TurnRequest};
use super::crypto::sha1_hex;
use super::*;
use crate::channel::im_interaction::{self as interaction, NoInteraction, PermissionAnswer};
use crate::test_baseline;
use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use pilotdeck_protocol::{JsObject, JsValue, parse_json};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const ORACLE: &str = include_str!("../../../tests/fixtures/wecom-callback/oracle.json");
const QYAPI_PORT: u16 = 18998;
const QYAPI: &str = "http://127.0.0.1:18998/cgi-bin";
/// Every blocking step is bounded so a regression fails instead of hanging the
/// suite. Loose enough that a loaded host (the whole lib suite runs ~4,200
/// tests in parallel) does not turn scheduling delay into a false FAIL; the
/// parity cells themselves are compared exactly, not by time.
const DEADLINE: Duration = Duration::from_secs(20);

fn oracle() -> Value {
    test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1336 oracle JSON")
}

fn cases(oracle: &Value) -> &Vec<Value> {
    let cases = oracle["cases"].as_array().expect("#1336 oracle cases");
    crate::negative_assert!(!cases.is_empty(), "#1336 oracle cases non-empty");
    crate::negative_assert_eq!(
        cases.len() as u64,
        oracle["expectedCaseCount"]
            .as_u64()
            .expect("#1336 expectedCaseCount"),
        "#1336 oracle case count matches its declaration"
    );
    // The declaration alone is not a ruler: dropping a case and lowering
    // `expectedCaseCount` together would keep both assertions above green.
    // The generator's fixed count is pinned separately (`EXPECTED_CASE_COUNT`
    // in `generate.mts`), so coverage cannot shrink silently.
    crate::negative_assert_eq!(
        cases.len() as u64,
        22,
        "#1336 oracle holds the generator's 22 cases"
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
            &format!("#1336 {context} matches the TS oracle"),
        ),
    }
}

async fn until(context: &str, predicate: impl Fn() -> bool) {
    let started = Instant::now();
    while !predicate() {
        crate::negative_assert!(
            started.elapsed() < DEADLINE,
            "#1336 {context} matches the TS oracle"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

// ---------------------------------------------------------------- recording

#[derive(Default)]
struct Record {
    http: Vec<Value>,
    qyapi: Vec<Value>,
    gateway: Vec<Value>,
    helpers: Vec<Value>,
    logs: Vec<Value>,
    events: Vec<Value>,
}

impl Record {
    fn snapshot(&self) -> Value {
        json!({
            "http": self.http,
            "qyapi": self.qyapi,
            "gateway": self.gateway,
            "helpers": self.helpers,
            "logs": self.logs,
            "events": self.events,
        })
    }
}

type Gate = Arc<tokio::sync::watch::Sender<bool>>;
type Gates = Arc<Mutex<HashMap<String, Gate>>>;

fn gate(gates: &Gates, name: &str) -> Gate {
    gates
        .lock()
        .expect("#1336 gate mutex")
        .entry(name.to_owned())
        .or_insert_with(|| Arc::new(tokio::sync::watch::Sender::new(false)))
        .clone()
}

async fn released(gate: Gate) {
    let mut rx = gate.subscribe();
    while !*rx.borrow_and_update() {
        rx.changed().await.expect("#1336 gate sender retained");
    }
}

fn js_text(value: &JsValue) -> String {
    value
        .as_string()
        .map(|text| String::from_utf16_lossy(text.units()))
        .unwrap_or_default()
}

// ---------------------------------------------------------------- mocks

#[derive(Clone, Default)]
struct TurnScript {
    gate_before: Option<String>,
    events: Vec<Value>,
    error: Option<String>,
}

/// The `gateway.submitTurn` fake: records the turn input, then streams the
/// scripted events (optionally behind a gate) and finally the scripted error.
struct MockInbound {
    turns: Vec<TurnScript>,
    next: Mutex<usize>,
    record: Arc<Mutex<Record>>,
    gates: Gates,
}

/// Adapts the scripted `mpsc` receiver to the shared `TurnEventStream`
/// (`crate::channel::protocol::TurnEventStream`, #1334).
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

impl TurnGateway for MockInbound {
    fn submit(
        &self,
        input: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let turn = {
            let mut next = self.next.lock().expect("#1336 turn index");
            let index = *next;
            *next += 1;
            self.turns.get(index).cloned().unwrap_or_default()
        };
        self.record
            .lock()
            .expect("#1336 record mutex")
            .gateway
            .push(json!({
                "sessionKey": String::from_utf16_lossy(input.session_key.units()),
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
                let _ = events.send(Ok(parse_json(&event.to_string()).expect("#1336 event JSON")));
            }
            if let Some(error) = turn.error {
                let _ = events.send(Err(ChannelError::error(error)));
            }
        });
        Box::pin(async move { Ok(Box::new(MockStream(stream)) as Box<dyn TurnEventStream>) })
    }
}

/// The `ImElicitationHelper` / `ImPermissionHelper` fakes, one instance per
/// case, driven by the case's script and recording every call with the TS
/// generator's `[name, ...args]` shape.
struct ScriptInteraction {
    script: Value,
    record: Arc<Mutex<Record>>,
}

impl ScriptInteraction {
    fn note(&self, entry: Value) {
        self.record
            .lock()
            .expect("#1336 record mutex")
            .helpers
            .push(entry);
    }
    fn pending(&self, kind: &str, chat_id: &str) -> bool {
        self.script[kind]["pending"]
            .as_array()
            .is_some_and(|chats| chats.iter().any(|chat| chat.as_str() == Some(chat_id)))
    }
    fn token_json(token: Option<u64>) -> Value {
        token.map_or(Value::Null, |token| json!(token))
    }
}

impl InteractionPort for ScriptInteraction {
    fn elicitation_pending(&self, chat_id: &str) -> bool {
        self.note(json!(["elicitation.hasPending", chat_id]));
        self.pending("elicitation", chat_id)
    }
    fn elicitation_capture(&self, chat_id: &str, session_key: &str, event: &JsValue) -> String {
        self.note(json!([
            "elicitation.capture",
            chat_id,
            session_key,
            js_text(event.get("type").unwrap_or(&JsValue::Undefined)),
        ]));
        self.script["elicitation"]["capture"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    }
    fn elicitation_answer<'a>(
        &'a self,
        chat_id: &'a str,
        text: &'a str,
    ) -> interaction::InteractionFuture<'a, Result<Option<String>, ChannelError>> {
        self.note(json!(["elicitation.answer", chat_id, text]));
        let answer = self.script["elicitation"]["answer"]
            .as_str()
            .map(str::to_owned);
        Box::pin(async move { Ok(answer) })
    }
    fn elicitation_clear(&self, chat_id: &str) {
        self.note(json!(["elicitation.clear", chat_id]));
    }

    fn permission_pending(&self, chat_id: &str) -> bool {
        self.note(json!(["permission.hasPending", chat_id]));
        self.pending("permission", chat_id)
    }
    fn permission_capture(
        &self,
        chat_id: &str,
        session_key: &str,
        event: &JsValue,
    ) -> Option<String> {
        self.note(json!([
            "permission.capture",
            chat_id,
            session_key,
            js_text(event.get("type").unwrap_or(&JsValue::Undefined)),
        ]));
        self.script["permission"]["capture"]
            .as_str()
            .map(str::to_owned)
    }
    fn permission_confirm_initial(&self, chat_id: &str, delivered: bool, request_id: Option<&str>) {
        self.note(json!([
            "permission.confirmInitialPrompt",
            chat_id,
            delivered,
            request_id,
        ]));
    }
    fn permission_answer<'a>(
        &'a self,
        chat_id: &'a str,
        text: &'a str,
    ) -> interaction::InteractionFuture<'a, Result<Option<PermissionAnswer>, ChannelError>> {
        self.note(json!(["permission.answerWithState", chat_id, text]));
        let answer = self.script["permission"]["answer"].clone();
        Box::pin(async move {
            if answer.is_null() {
                return Ok(None);
            }
            Ok(Some(PermissionAnswer {
                text: answer["text"].as_str().map(str::to_owned),
                can_advance: answer["canAdvance"].as_bool().unwrap_or(false),
                retry_prompt: answer["retryPrompt"].as_bool().unwrap_or(false),
                answer_token: answer["answerToken"].as_u64(),
            }))
        })
    }
    fn permission_release_answer(&self, chat_id: &str, answer_token: Option<u64>) {
        self.note(json!([
            "permission.releaseAnswer",
            chat_id,
            Self::token_json(answer_token),
        ]));
    }
    fn permission_take_next_prompt(
        &self,
        chat_id: &str,
        answer_token: Option<u64>,
    ) -> Option<String> {
        self.note(json!([
            "permission.takeNextPrompt",
            chat_id,
            Self::token_json(answer_token),
        ]));
        self.script["permission"]["nextPrompt"]
            .as_str()
            .map(str::to_owned)
    }
    fn permission_prompt_request_id(
        &self,
        chat_id: &str,
        answer_token: Option<u64>,
    ) -> Option<String> {
        self.note(json!([
            "permission.getPromptRequestId",
            chat_id,
            Self::token_json(answer_token),
        ]));
        self.script["permission"]["promptRequestId"]
            .as_str()
            .map(str::to_owned)
    }
    fn permission_confirm_next(
        &self,
        chat_id: &str,
        delivered: bool,
        request_id: Option<&str>,
        answer_token: Option<u64>,
    ) {
        self.note(json!([
            "permission.confirmNextPrompt",
            chat_id,
            delivered,
            request_id,
            Self::token_json(answer_token),
        ]));
    }
    fn permission_clear(&self, chat_id: &str) {
        self.note(json!(["permission.clear", chat_id]));
    }
    fn permission_clear_after_turn(&self, chat_id: &str) {
        self.note(json!(["permission.clearAfterTurn", chat_id]));
    }
}

// ---------------------------------------------------------------- qyapi fake

/// The loopback qyapi fake: one server for the whole replay (exactly like the
/// TS generator), a scripted response queue and a per-case record slot.
struct Qyapi {
    script: Mutex<VecDeque<(u16, String)>>,
    record: Mutex<Option<Arc<Mutex<Record>>>>,
    stray: Mutex<Vec<Value>>,
}

impl Qyapi {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(VecDeque::new()),
            record: Mutex::new(None),
            stray: Mutex::new(Vec::new()),
        })
    }

    fn arm(&self, case: &Value, record: Arc<Mutex<Record>>) {
        let mut script = self.script.lock().expect("#1336 qyapi script");
        script.clear();
        for entry in case["qyapi"].as_array().expect("#1336 qyapi script") {
            script.push_back((
                entry["status"].as_u64().expect("#1336 qyapi status") as u16,
                entry["json"].to_string(),
            ));
        }
        *self.record.lock().expect("#1336 qyapi record slot") = Some(record);
    }

    fn disarm(&self) {
        *self.record.lock().expect("#1336 qyapi record slot") = None;
    }
}

async fn start_qyapi(qyapi: Arc<Qyapi>) {
    start_qyapi_on(qyapi, QYAPI_PORT).await;
}

/// The same fake on another port: the cron-delivery pin runs beside the golden
/// replay, so the two must not race for one listener.
async fn start_qyapi_on(qyapi: Arc<Qyapi>, port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("#1336 qyapi bind");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let qyapi = qyapi.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let qyapi = qyapi.clone();
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
                            "contentType": parts.headers
                                .get(hyper::header::CONTENT_TYPE)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned),
                            "body": if bytes.is_empty() {
                                Value::Null
                            } else {
                                json!(String::from_utf8_lossy(&bytes))
                            },
                        });
                        let record = qyapi
                            .record
                            .lock()
                            .expect("#1336 qyapi record slot")
                            .clone();
                        match record {
                            Some(record) => {
                                record.lock().expect("#1336 record mutex").qyapi.push(entry)
                            }
                            None => qyapi.stray.lock().expect("#1336 stray mutex").push(entry),
                        }
                        let (status, payload) = qyapi
                            .script
                            .lock()
                            .expect("#1336 qyapi script")
                            .pop_front()
                            .unwrap_or_else(|| (200, "{\"errcode\":0}".to_owned()));
                        let response = hyper::Response::builder()
                            .status(
                                hyper::StatusCode::from_u16(status).expect("#1336 qyapi status"),
                            )
                            .header(hyper::header::CONTENT_TYPE, "application/json")
                            .body(Full::new(Bytes::from(payload)))
                            .expect("#1336 qyapi response");
                        Ok::<_, Infallible>(response)
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

fn parse_raw_response(context: &str, raw: &[u8]) -> Value {
    // A mutation that stops a response from arriving at all must fail on the
    // case marker, not on an unnamed helper expect (host matrix: INVALID).
    let header_end = crate::test_negative_receipt::expect_some(
        raw.windows(4).position(|window| window == b"\r\n\r\n"),
        &format!("#1336 {context} matches the TS oracle"),
    );
    let head = String::from_utf8_lossy(&raw[..header_end]).into_owned();
    let body = String::from_utf8_lossy(&raw[header_end + 4..]).into_owned();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("#1336 status line");
    let status: u16 = status_line
        .split(' ')
        .nth(1)
        .expect("#1336 status token")
        .parse()
        .expect("#1336 status number");
    let mut content_type: Option<String> = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-type")
        {
            content_type = Some(value.trim().to_owned());
        }
    }
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

// ---------------------------------------------------------------- replay

async fn replay(case: &Value, qyapi: &Arc<Qyapi>) -> Value {
    let record = Arc::new(Mutex::new(Record::default()));
    qyapi.arm(case, record.clone());
    let gates = Gates::default();
    // Every blocking step names its case, so a mutation that never converges
    // fails on the same marker as the cell comparison it stands in for.
    let context = format!("case {}", case["name"].as_str().expect("#1336 case name"));

    // The env guard and the fixture→options mapping both derive from the same
    // `STRING_FIELDS` the constructor guard uses: a sixth string field joins
    // one list, not three (the replay must then unset its env and forward its
    // fixture value or the cell stops being driven).
    for name in super::STRING_FIELDS
        .iter()
        .map(|(_, env)| *env)
        .chain(["WECOM_CB_PORT"])
    {
        assert!(
            std::env::var_os(name).is_none(),
            "#1336 replay requires {name} to be unset"
        );
    }

    let config = case["config"].as_object().expect("#1336 case config");
    let mut options = JsObject::new();
    for key in super::STRING_FIELDS.map(|(name, _env)| name) {
        if let Some(value) = config.get(key) {
            options.insert(
                key.into(),
                JsValue::String(JsString::from(value.as_str().expect("#1336 config string"))),
            );
        }
    }
    options.insert(
        "port".into(),
        JsValue::Number(config["port"].as_f64().expect("#1336 case port number")),
    );

    let turns: Vec<TurnScript> = case["turns"]
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
        .unwrap_or_default();

    let uuid_counter = Arc::new(Mutex::new(0u32));
    let uuid: mapper::Uuid = Arc::new(move || {
        let mut next = uuid_counter.lock().expect("#1336 uuid counter");
        *next += 1;
        format!("s_uuid-{next}")
    });

    let gateway: Arc<dyn TurnGateway> = Arc::new(MockInbound {
        turns,
        next: Mutex::new(0),
        record: record.clone(),
        gates: gates.clone(),
    });

    let channel = WeComCallbackChannel::with_uuid(
        WeComCallbackOptions::from_object(&options, QYAPI),
        Arc::new(ScriptInteraction {
            script: json!({
                "elicitation": case["elicitation"],
                "permission": case["permission"],
            }),
            record: record.clone(),
        }),
        uuid,
    );

    let logger = record.clone();
    let deps = ChannelStartDeps {
        config: None,
        report_status: None,
        log: Some(Arc::new(move |level, message| {
            logger
                .lock()
                .expect("#1336 record mutex")
                .logs
                .push(json!([level, message]));
        })),
        turn_gateway: Some(gateway),
    };

    let case_port = || -> u16 {
        case["config"]["port"]
            .as_u64()
            .expect("#1336 case port")
            .try_into()
            .expect("#1336 case port fits u16")
    };

    let mut handle: Option<Arc<dyn ChannelHandle>> = None;
    let mut pending: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut sockets: HashMap<String, TcpStream> = HashMap::new();
    let mut occupier: Option<TcpListener> = None;

    for step in case["steps"].as_array().expect("#1336 case steps") {
        match step["op"].as_str().expect("#1336 step op") {
            "start" => {
                handle = Some(
                    bounded(&context, channel.start(deps.clone()))
                        .await
                        .expect("#1336 start"),
                );
            }
            "http" => {
                let result = http_request(
                    &context,
                    case_port(),
                    step["method"].as_str().expect("#1336 http method"),
                    step["path"].as_str().expect("#1336 http path"),
                    step["body"].as_str(),
                )
                .await;
                record.lock().expect("#1336 record mutex").http.push(result);
            }
            "stop" => {
                let reason = step["reason"].as_str().expect("#1336 stop reason");
                bounded(
                    &context,
                    handle.as_ref().expect("#1336 started").stop(reason),
                )
                .await
                .expect("#1336 stop result");
                record
                    .lock()
                    .expect("#1336 record mutex")
                    .events
                    .push(json!({"stop": reason}));
            }
            "stop-pending" => {
                let reason = step["reason"]
                    .as_str()
                    .expect("#1336 stop reason")
                    .to_owned();
                let stop = handle.as_ref().expect("#1336 started").clone();
                let detached = context.clone();
                pending.insert(
                    step["label"].as_str().expect("#1336 stop label").to_owned(),
                    tokio::spawn(async move {
                        let _ = bounded(&detached, stop.stop(&reason)).await;
                    }),
                );
            }
            "await" => {
                let reason = step["reason"].as_str().expect("#1336 stop reason");
                let label = step["label"].as_str().expect("#1336 stop label");
                pending
                    .remove(label)
                    .expect("#1336 pending stop")
                    .await
                    .expect("#1336 pending stop task");
                record
                    .lock()
                    .expect("#1336 record mutex")
                    .events
                    .push(json!({"stop": reason}));
            }
            "tcp-open" => {
                // Wait for the accept, not for a fixed slice of wall clock: a
                // `stop-pending` that flips the shutdown watch before the
                // accept loop took the connection makes the oracle's drain
                // cell fail for scheduling reasons only. The counter belongs
                // to this channel, so a test running in parallel cannot
                // satisfy the barrier on its own connection.
                let counter = channel.runtime.accepted.clone();
                let before = counter.load(std::sync::atomic::Ordering::SeqCst);
                let mut stream = TcpStream::connect(("127.0.0.1", case_port()))
                    .await
                    .expect("#1336 slow socket connect");
                let _ = stream
                    .write_all(
                        step["request"]
                            .as_str()
                            .expect("#1336 request bytes")
                            .as_bytes(),
                    )
                    .await;
                until(&context, move || {
                    counter.load(std::sync::atomic::Ordering::SeqCst) > before
                })
                .await;
                sockets.insert(
                    step["label"]
                        .as_str()
                        .expect("#1336 socket label")
                        .to_owned(),
                    stream,
                );
            }
            "tcp-chunk" => {
                let stream = sockets
                    .get_mut(step["label"].as_str().expect("#1336 socket label"))
                    .expect("#1336 open socket");
                let _ = stream
                    .write_all(
                        step["chunk"]
                            .as_str()
                            .expect("#1336 chunk bytes")
                            .as_bytes(),
                    )
                    .await;
            }
            "tcp-end" => {
                let label = step["label"].as_str().expect("#1336 socket label");
                let mut stream = sockets.remove(label).expect("#1336 open socket");
                let _ = stream.shutdown().await;
                let mut raw = Vec::new();
                let _ = bounded(&context, stream.read_to_end(&mut raw)).await;
                record
                    .lock()
                    .expect("#1336 record mutex")
                    .http
                    .push(parse_raw_response(&context, &raw));
            }
            "occupy" => {
                let port = case_port();
                occupier = Some(
                    TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port))
                        .await
                        .expect("#1336 occupy port"),
                );
            }
            "release" => {
                gate(&gates, step["gate"].as_str().expect("#1336 gate name")).send_replace(true);
            }
            "wait" => {
                if let Some(count) = step["qyapi"].as_u64() {
                    let record = record.clone();
                    until(&context, move || {
                        record.lock().expect("#1336 record mutex").qyapi.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["helpers"].as_u64() {
                    let record = record.clone();
                    until(&context, move || {
                        record.lock().expect("#1336 record mutex").helpers.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["gateway"].as_u64() {
                    let record = record.clone();
                    until(&context, move || {
                        record.lock().expect("#1336 record mutex").gateway.len() >= count as usize
                    })
                    .await;
                } else if let Some(ticks) = step["ticks"].as_u64() {
                    for _ in 0..ticks {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                } else {
                    panic!("#1336 wait without condition");
                }
            }
            other => panic!("#1336 unknown op {other}"),
        }
    }
    drop(sockets);
    drop(occupier);
    qyapi.disarm();
    record.lock().expect("#1336 record mutex").snapshot()
}

#[tokio::test]
async fn channel_matches_real_ts() {
    let oracle = oracle();
    let qyapi = Qyapi::new();
    start_qyapi(qyapi.clone()).await;
    for case in cases(&oracle) {
        let name = case["name"].as_str().expect("#1336 case name");
        let actual = replay(case, &qyapi).await;
        crate::negative_assert_eq!(
            actual,
            case["expected"],
            "#1336 case {} matches the TS oracle",
            name
        );
    }
    let strays = qyapi.stray.lock().expect("#1336 stray mutex");
    crate::negative_assert_eq!(
        strays.len(),
        0,
        "#1336 no qyapi request arrives outside an armed case"
    );
}

/// `WeComCallbackChannel.deliverCronResult` is the shared
/// `deliverChatCronResult(delivery, channelKey, sendReply)` call: a foreign
/// `originChannelKey` answers `false` without touching qyapi, a session key
/// without the `<key>:chat=` prefix does the same, and a matching delivery
/// sends the text and answers the `sendReply` boolean. The TS golden fixture
/// has no cron case (it scripts `submitTurn`, not `deliverCronResult`), so this
/// pin covers the helper hand-off rather than a recorded cell.
#[tokio::test]
async fn deliver_cron_result_follows_the_shared_helper() {
    const PORT: u16 = 18997;
    let qyapi = Qyapi::new();
    start_qyapi_on(qyapi.clone(), PORT).await;
    let record = Arc::new(Mutex::new(Record::default()));
    qyapi.arm(
        &json!({"qyapi": [
            {"status": 200, "json": {"errcode": 0, "access_token": "tok-1", "expires_in": 7200}},
            {"status": 200, "json": {"errcode": 0, "errmsg": "ok"}},
        ]}),
        record.clone(),
    );

    let mut options = JsObject::new();
    for (key, value) in [
        ("corpId", "corp-x"),
        ("agentId", "agent-1"),
        ("secret", "corp-secret"),
        ("token", "callback-token"),
        (
            "encodingAesKey",
            "MDEyMzQ1Njc4OWFiY2RlZmdoaWprbG1ub3BxcnN0dXY",
        ),
    ] {
        options.insert(key.into(), JsValue::String(JsString::from(value)));
    }
    options.insert("port".into(), JsValue::Number(0.0));
    let channel = WeComCallbackChannel::new(
        WeComCallbackOptions::from_object(&options, &format!("http://127.0.0.1:{PORT}/cgi-bin")),
        Arc::new(NoInteraction),
    );
    let delivery = |origin_channel: Option<&str>, session: &str| CronResultDelivery {
        task_id: JsString::from("task-1"),
        run_id: JsString::from("run-1"),
        session_key: JsString::from(session),
        channel_key: ChannelKey::new("cron"),
        origin_session_key: None,
        origin_channel_key: origin_channel.map(ChannelKey::new),
        text: JsString::from("cron says hi"),
    };

    let foreign = channel
        .deliver_cron_result(&delivery(
            Some("webhook"),
            "wecom_callback:chat=user-1:general",
        ))
        .await;
    crate::negative_assert!(!foreign, "#1336 cron: foreign origin channel answers false");

    let unparsable = channel
        .deliver_cron_result(&delivery(Some(CHANNEL_KEY), "wecom_callback:chat="))
        .await;
    crate::negative_assert!(
        !unparsable,
        "#1336 cron: a session key without a parsable chat id answers false"
    );
    crate::negative_assert_eq!(
        record.lock().expect("#1336 record mutex").qyapi.len(),
        0,
        "#1336 cron: neither rejected delivery reaches qyapi"
    );

    let delivered = channel
        .deliver_cron_result(&delivery(
            Some(CHANNEL_KEY),
            "wecom_callback:chat=user-1:s_0123abcd-0000-4000-8000-000000000000",
        ))
        .await;
    crate::negative_assert!(
        delivered,
        "#1336 cron: a matching delivery answers sendReply"
    );
    let calls = record.lock().expect("#1336 record mutex").qyapi.clone();
    crate::negative_assert_eq!(calls.len(), 2, "#1336 cron: gettoken then message/send");
    crate::negative_assert_eq!(
        calls[1]["path"].as_str(),
        Some("/cgi-bin/message/send"),
        "#1336 cron: the delivery goes out through sendReply"
    );
    let body: Value = serde_json::from_str(calls[1]["body"].as_str().expect("#1336 cron body"))
        .expect("#1336 cron body JSON");
    crate::negative_assert_eq!(
        body["touser"].as_str(),
        Some("user-1"),
        "#1336 cron: the chat id comes from the session-key parse"
    );
    crate::negative_assert!(
        body["text"]["content"]
            .as_str()
            .is_some_and(|text| text.contains("cron says hi")),
        "#1336 cron: the delivery text is the message body"
    );
}

// ---------------------------------------------------------------- component pins

/// The oracle's own echostr envelope, decrypted through the production path.
#[test]
fn decrypt_matches_the_recorded_envelope() {
    let aes_key = "MDEyMzQ1Njc4OWFiY2RlZmdoaWprbG1ub3BxcnN0dXY";
    let echostr = "jJmLrfcJY2jImAGfCSTy+Zm1A0OphjE5bddoXZ5ZQkJHqz35ukzAQw7QC4qlt5gw";
    crate::negative_assert_eq!(
        decrypt_wx_message(aes_key, echostr, Some("corp-x")).expect("#1336 echostr decrypts"),
        "echo-plain-1",
        "#1336 recorded echostr decrypts to the recorded plain text"
    );
    let mismatch = decrypt_wx_message(aes_key, echostr, Some("other-corp"))
        .expect_err("#1336 corpId mismatch is an error");
    crate::negative_assert_eq!(
        mismatch.to_string(),
        "Error: corpId mismatch after decrypt",
        "#1336 corpId mismatch keeps the Node error shape"
    );
}

/// JavaScript's default sort compares UTF-16 code units, not code points:
/// U+10000 (surrogate D800) sorts before U+FFFD, while UTF-8 byte order
/// (F0 > EF) would sort it after. A byte-order sort must fail this pin.
#[test]
fn signature_sort_uses_utf16_code_units() {
    let sig = sha1_hex("nnts\u{10000}\u{FFFD}");
    crate::negative_assert!(
        verify_msg_signature("\u{FFFD}", "ts", "nn", "\u{10000}", &sig),
        "#1336 signature sort follows UTF-16 code-unit order"
    );
}

#[test]
fn config_port_follows_number_semantics() {
    for name in ["WECOM_CB_PORT", "WECOM_CORP_ID"] {
        assert!(
            std::env::var_os(name).is_none(),
            "#1336 config_port pin requires {name} to be unset"
        );
    }
    let port = |value: JsValue| {
        let mut options = JsObject::new();
        options.insert("port".into(), value);
        config_port(&options)
    };
    crate::negative_assert_eq!(port(JsValue::Number(8780.0)), 8780, "#1336 whole port");
    crate::negative_assert_eq!(port(JsValue::Number(9100.9)), 9100, "#1336 floor");
    crate::negative_assert_eq!(
        port(JsValue::Number(0.0)),
        DEFAULT_PORT,
        "#1336 zero falls back"
    );
    crate::negative_assert_eq!(
        port(JsValue::Number(-5.0)),
        DEFAULT_PORT,
        "#1336 negative falls back"
    );
    crate::negative_assert_eq!(
        port(JsValue::Number(f64::NAN)),
        DEFAULT_PORT,
        "#1336 NaN falls back"
    );
    crate::negative_assert_eq!(
        port(JsValue::String(JsString::from("12"))),
        12,
        "#1336 Number('12') is 12"
    );
    crate::negative_assert_eq!(
        port(JsValue::String(JsString::from("x"))),
        DEFAULT_PORT,
        "#1336 Number('x') is NaN"
    );
    // An out-of-range port survives Number() and fails later at listen with
    // the recorded RangeError; it must not be silently clamped here.
    crate::negative_assert_eq!(
        port(JsValue::Number(70000.0)),
        70000,
        "#1336 bad port survives"
    );
}

/// #2432: the TS constructor coerces all five string fields through `String()`
/// *before* the port (WeComCallbackChannel.ts:100-105 field order), so a value
/// whose conversion throws — an own `toString`, a prototype-carried one, a
/// null-terminating chain, or an array whose join reaches such an element —
/// fails the whole construction with the TS error text. A plain object still
/// stringifies as `[object Object]` and must construct: only the throwing
/// shapes take the loader's catch path.
#[test]
fn constructor_string_fields_report_the_ts_type_error() {
    let constructor = WeComCallbackChannel::constructor(no_interaction());
    // The same list `from_object` coerces — a sixth string field added to
    // one half of the mirror must fail this loop, not silently skip the guard.
    for field in super::STRING_FIELDS.map(|(name, _env)| name) {
        let mut options = JsObject::new();
        options.insert(
            field.into(),
            parse_json(r#"{"toString": null}"#).expect("#2432 own-toString shape parses"),
        );
        let error = constructor(&options)
            .err()
            .unwrap_or_else(|| panic!("#2432 {field} must refuse the throwing shape"));
        match error {
            ChannelError::Plain(message) => crate::negative_assert_eq!(
                message,
                "TypeError: Cannot convert object to primitive value",
                "#2432 {field} reports the TS TypeError"
            ),
            other => panic!(
                "#2432 {field} reports the port's Plain shape, found {}",
                other.to_js_string()
            ),
        }
    }
    // The array element join takes the same walk: `String([{toString: null}])`
    // throws before the port coercion is ever reached.
    let mut options = JsObject::new();
    options.insert(
        "agentId".into(),
        parse_json(r#"[{"toString": null}]"#).expect("#2432 array shape parses"),
    );
    let error = constructor(&options)
        .err()
        .expect("#2432 agentId array must refuse the throwing shape");
    match error {
        ChannelError::Plain(message) => crate::negative_assert_eq!(
            message,
            "TypeError: Cannot convert object to primitive value",
            "#2432 agentId array join reports the TS TypeError"
        ),
        other => panic!(
            "#2432 agentId array reports the port's Plain shape, found {}",
            other.to_js_string()
        ),
    }
    let mut options = JsObject::new();
    options.insert(
        "corpId".into(),
        parse_json(r#"{"a": 1}"#).expect("#2432 plain object parses"),
    );
    assert!(
        constructor(&options).is_ok(),
        "#2432 a plain object still stringifies as [object Object] and constructs"
    );
}

/// #2432, loader level: `extra.corpId: {toString: null}` makes the TS
/// constructor throw, so `loadEnabledChannels` records exactly one console.error
/// line for the key and continues — the channel never enters the returned
/// array (the same catch-and-continue shape the port's TypeError already has).
#[test]
fn loader_records_string_field_type_errors_and_continues() {
    let mut mounts = crate::channel::loader::ChannelLoaderMounts::default();
    mounts.register(
        "wecomCallback",
        WeComCallbackChannel::constructor(no_interaction()),
    );
    let config = parse_json(
        r#"{"adapters": {"wecomCallback": {"enabled": true, "extra": {"corpId": {"toString": null}}}}}"#,
    )
    .expect("#2432 loader fixture config parses");
    let mut errors = Vec::new();
    let channels = crate::channel::loader::load_enabled_channels(
        config.get("adapters"),
        &crate::channel::loader::LoadEnabledChannelsOptions { pilot_home: None },
        &mounts,
        &mut |message| errors.push(message),
    );
    crate::negative_assert_eq!(
        errors,
        vec![
            "[adapters] Failed to load channel \"wecomCallback\": TypeError: Cannot convert object to primitive value"
                .to_owned()
        ],
        "#2432 the loader records the corpId TypeError and skips the key"
    );
    crate::negative_assert!(
        channels.is_empty(),
        "#2432 the failed key constructs no channel"
    );
}

/// #2426: `wecomCallback` was the only implemented loader key without a mount
/// test (the five siblings' shape, template `channel/webhook/tests.rs:2906`).
/// The loader's `build_options` maps the config's six fields onto the option
/// object, the mount's `WeComCallbackChannel::constructor` builds the adapter
/// from it, and the constructor product — `WeComCallbackOptions`, the same
/// type the channel reads at `start` — matches the fixture config item by
/// item. A recording mount wraps the production constructor so the
/// loader-built option object is observable without widening the adapter's
/// product type; the wrapped call is the only path to the assertions.
#[tokio::test]
async fn loader_mount_builds_wecom_callback_channel() {
    let mut mounts = crate::channel::loader::ChannelLoaderMounts::default();
    let captured: Arc<Mutex<Option<JsObject>>> = Arc::default();
    let sink = captured.clone();
    let constructor = WeComCallbackChannel::constructor(no_interaction());
    mounts.register(
        "wecomCallback",
        Arc::new(move |options: &JsObject| {
            *sink.lock().expect("#2426 capture mutex") = Some(options.clone());
            constructor(options)
        }),
    );
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("#2426 free-port probe");
    let port = probe.local_addr().expect("#2426 probe address").port();
    drop(probe);
    let adapters = parse_json(
        &json!({
            "wecomCallback": {"enabled": true, "token": "probe-token", "extra": {
                "corpId": "probe-corp",
                "agentId": "probe-agent",
                "secret": "probe-secret",
                "encodingAesKey": "probe-aes",
                "port": port
            }},
            "telegram": {"enabled": false},
        })
        .to_string(),
    )
    .expect("#2426 adapters JSON");
    let mut errors = Vec::new();
    let channels = crate::channel::loader::load_enabled_channels(
        Some(&adapters),
        &crate::channel::loader::LoadEnabledChannelsOptions { pilot_home: None },
        &mounts,
        &mut |message| errors.push(message),
    );
    crate::negative_assert!(errors.is_empty(), "#2426 loader errors: {errors:?}");
    crate::negative_assert_eq!(
        channels.len(),
        1,
        "#2426 exactly the wecom_callback channel loads"
    );
    crate::negative_assert_eq!(
        channels[0].channel_key().to_string(),
        "wecom_callback",
        "#2426 the loader-built channel carries the wecom_callback key"
    );
    let captured = captured
        .lock()
        .expect("#2426 capture mutex")
        .clone()
        .expect("#2426 the mount recorded the loader-built options");
    let options = WeComCallbackOptions::from_object(&captured, DEFAULT_QYAPI);
    crate::negative_assert_eq!(
        options.corp_id,
        "probe-corp",
        "#2426 loader mount maps corpId through to the channel options"
    );
    crate::negative_assert_eq!(
        options.agent_id,
        "probe-agent",
        "#2426 loader mount maps agentId through to the channel options"
    );
    crate::negative_assert_eq!(
        options.secret,
        "probe-secret",
        "#2426 loader mount maps secret through to the channel options"
    );
    crate::negative_assert_eq!(
        options.token,
        "probe-token",
        "#2426 loader mount maps token through to the channel options"
    );
    crate::negative_assert_eq!(
        options.encoding_aes_key,
        "probe-aes",
        "#2426 loader mount maps encodingAesKey through to the channel options"
    );
    crate::negative_assert_eq!(
        options.port,
        port as u32,
        "#2426 loader mount maps the extra port through to the channel options"
    );
    crate::negative_assert_eq!(
        options.qyapi,
        DEFAULT_QYAPI,
        "#2426 the mount keeps the production qyapi constant"
    );

    // The constructed adapter really listens through the mount's options: the
    // start line is the TS dist anchor and the port accepts a connection.
    let logs: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let sink = logs.clone();
    let handle = channels[0]
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: Some(Arc::new(move |level: &str, message: &str| {
                sink.lock()
                    .expect("#2426 log sink")
                    .push((level.to_owned(), message.to_owned()));
            })),
            turn_gateway: None,
        })
        .await
        .expect("#2426 loader-built wecom_callback start");
    let listening = format!("wecom_callback: listening on 0.0.0.0:{port}");
    assert!(
        logs.lock()
            .expect("#2426 log sink")
            .iter()
            .any(|(level, message)| level == "info" && message == &listening),
        "#2426 the TS dist anchor line {listening:?} in {:?}",
        logs.lock().expect("#2426 log sink")
    );
    TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#2426 the loader-built listener accepts connections");
    handle
        .stop("test")
        .await
        .expect("#2426 wecom_callback stop");
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "#2426 the listener is gone after stop"
    );
}

#[test]
fn message_body_truncates_at_2048_utf16_units() {
    let body = message_body("1000002", "chat1", &"😀".repeat(1500));
    let value: Value = serde_json::from_str(&body).expect("#1336 body JSON");
    crate::negative_assert_eq!(
        value["text"]["content"].as_str().expect("#1336 content"),
        "😀".repeat(1024),
        "#1336 3000 UTF-16 units truncate to 2048 (1024 surrogate pairs)"
    );
    crate::negative_assert_eq!(
        value["agentid"],
        json!(1000002),
        "#1336 agentid is a number"
    );
    crate::negative_assert_eq!(value["touser"], json!("chat1"), "#1336 touser echo");
    crate::negative_assert_eq!(value["msgtype"], json!("text"), "#1336 msgtype");

    // The truncation point can fall *inside* a surrogate pair: JS
    // `slice(0, 2048)` then keeps the lone high half, and `JSON.stringify`
    // escapes it as `\ud83d` (ES2019 well-formed stringify). Counting code
    // points, or `from_utf16_lossy`, would silently turn it into U+FFFD or
    // keep 2049 units instead — neither shows up in the paired-pair case
    // above, so pin the cut here.
    let split = message_body("1000002", "chat1", &format!("{}😀", "q".repeat(2047)));
    crate::negative_assert_eq!(
        split.matches('q').count(),
        2047,
        "#1336 2047 BMP units before the cut are kept"
    );
    crate::negative_assert!(
        split.contains(r"\ud83d"),
        "#1336 a split surrogate keeps its high half as \\ud83d"
    );
    crate::negative_assert!(
        !split.contains('\u{FFFD}'),
        "#1336 truncation never substitutes U+FFFD"
    );
}

/// #1336 r1 (concurrency, P1, DeepSeek): Node's `server.close()` finishes even
/// when a client stalls mid-request, because `headersTimeout` / `requestTimeout`
/// retire the connection. A/B on the header timer: with a short timeout
/// `stop()` drains; with a long one the identical stall still pins it, so the
/// timer — not something else in the loop — is what unblocks production.
#[tokio::test]
async fn stop_drains_a_half_sent_header_once_the_header_timeout_fires() {
    const PORT: u16 = 18996;
    // Destructure by name: the tuple grew a middle `complete` flag for the 408
    // half, and a positional `.1` here would silently assert EOF instead of
    // `stop()` draining — both are `bool`, so nothing but this comment catches
    // the swap (#1336 r7 sibling/concurrency/boundary/Codex P1).
    let (_, _, drained) = stop_after_stalled_request(
        PORT,
        http::Timeouts {
            headers: Duration::from_millis(300),
            request: Duration::from_secs(30),
        },
        b"POST /cb HTTP/1.1\r\nHost: x\r\n",
        Duration::from_secs(5),
    )
    .await;
    crate::negative_assert!(
        drained,
        "#1336 headersTimeout lets stop() retire a half-sent request"
    );
    let (_, _, drained) = stop_after_stalled_request(
        PORT,
        http::Timeouts {
            headers: Duration::from_secs(30),
            request: Duration::from_secs(30),
        },
        b"POST /cb HTTP/1.1\r\nHost: x\r\n",
        Duration::from_secs(1),
    )
    .await;
    crate::negative_assert!(
        !drained,
        "#1336 without the header timeout a half-sent request still pins stop()"
    );
}

/// Node's `requestTimeout`: a request whose body never arrives is answered
/// `408` instead of holding the connection, and the timeout is what lets
/// `stop()` drain a client that stalls after sending its headers.
///
/// The bytes are Node's 51 unframed bytes plus hyper's framing (`date`,
/// `content-length: 0`) — the `connection: close` header is what closes the
/// socket, and without it the connection stays open and `stop()` pins. The
/// presentation difference is registered in
/// `specs/gateway-parity/diffs/A8.3-WECOM-CALLBACK-408-001.md`; a status-only
/// check (the earlier shape of this test) could not tell the two apart.
#[tokio::test]
async fn request_timeout_answers_408_and_releases_stop() {
    const PORT: u16 = 18995;
    let head = b"POST /cb HTTP/1.1\r\nHost: x\r\nContent-Length: 100\r\n\r\n";
    let (response, complete, drained) = stop_after_stalled_request(
        PORT,
        http::Timeouts {
            headers: Duration::from_secs(30),
            request: Duration::from_millis(300),
        },
        head,
        Duration::from_secs(5),
    )
    .await;
    // The read must reach EOF: a deadline that expires on a header-only prefix
    // leaves exactly the bytes a bodyless response leaves, so the "no body"
    // pin below cannot tell them apart on its own (#1336 r6 Codex P1).
    crate::negative_assert!(
        complete,
        "#1336 the 408 response is read to EOF inside the read deadline"
    );
    let raw = response.as_deref().unwrap_or_default();
    crate::negative_assert!(
        raw.starts_with(b"HTTP/1.1 408 Request Timeout\r\n"),
        "#1336 requestTimeout answers 408 Request Timeout"
    );
    crate::negative_assert!(
        raw.windows(b"\r\nconnection: close\r\n".len())
            .any(|window| window == b"\r\nconnection: close\r\n"),
        "#1336 the 408 closes the connection like Node's requestTimeout"
    );
    // Split at the header terminator: `ends_with("\r\n\r\n")` also holds for a
    // body that itself ends in those four bytes, so it cannot prove the body is
    // empty (#1336 r5 Codex P1).
    let body = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| &raw[at + 4..]);
    crate::negative_assert!(
        matches!(body, Some(rest) if rest.is_empty()),
        "#1336 the 408 has no body"
    );
    crate::negative_assert!(
        drained,
        "#1336 requestTimeout lets stop() retire a stalled body"
    );
    let (response, complete, drained) = stop_after_stalled_request(
        PORT,
        http::Timeouts {
            headers: Duration::from_secs(30),
            request: Duration::from_secs(30),
        },
        head,
        Duration::from_secs(1),
    )
    .await;
    // `None` is "not one byte arrived in 3 s", not "the read returned": the
    // helper keeps partial bytes even when its deadline expires, so an early
    // answer that never reaches EOF still fails this half. `!complete` pins the
    // other direction too — a server that silently closed the socket inside the
    // deadline would also read as "no answer" (#1336 r7 concurrency P2).
    crate::negative_assert!(
        !complete,
        "#1336 the long-timeout half is answered by no EOF either"
    );
    crate::negative_assert!(response.is_none(), "#1336 no answer before requestTimeout");
    crate::negative_assert!(
        !drained,
        "#1336 without requestTimeout a stalled body still pins stop()"
    );
}

/// Exercise the real accept/drain task through the production channel handle,
/// retaining a duplicate of its listening socket until after stop and rebind.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn stop_disables_listening_even_while_a_duplicate_descriptor_is_alive() {
    for served_request in [false, true] {
        let listener = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
            .await
            .unwrap();
        let bound = listener.local_addr().unwrap();
        let address = ("127.0.0.1", bound.port());
        let duplicate = socket2::SockRef::from(&listener).try_clone().unwrap();
        let mut options = JsObject::new();
        for (key, value) in [
            ("corpId", "corp-x"),
            ("agentId", "agent-1"),
            ("secret", "corp-secret"),
            ("token", "callback-token"),
            (
                "encodingAesKey",
                "MDEyMzQ1Njc4OWFiY2RlZmdoaWprbG1ub3BxcnN0dXY",
            ),
        ] {
            options.insert(key.into(), JsValue::String(JsString::from(value)));
        }
        options.insert("port".into(), JsValue::Number(f64::from(bound.port())));
        let channel = WeComCallbackChannel::new(
            WeComCallbackOptions::from_object(&options, QYAPI),
            Arc::new(NoInteraction),
        );
        let listener = http::start_listener(
            channel.runtime.clone(),
            listener,
            http::Timeouts::default(),
            Some(channel.runtime.accepted.clone()),
        )
        .unwrap();
        let handle = ListenerHandle {
            runtime: channel.runtime.clone(),
            listener: Arc::new(Mutex::new(Some(listener))),
        };
        if served_request {
            let mut socket = TcpStream::connect(address).await.unwrap();
            socket
                .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut response = Vec::new();
            timeout(DEADLINE, socket.read_to_end(&mut response))
                .await
                .expect("the callback listener answers before stop")
                .unwrap();
            assert!(response.starts_with(b"HTTP/1.1 400 "));
        }
        timeout(DEADLINE, handle.stop("duplicate-listener"))
            .await
            .expect("the listener and accepted connections stop")
            .expect("the listener shutdown succeeds");
        assert!(
            TcpStream::connect(address).await.is_err(),
            "stop must disable the shared listener; served_request={served_request}"
        );
        let replacement = TcpListener::bind(bound)
            .await
            .expect("the stopped listener's exact address can be rebound with its duplicate alive");
        assert!(duplicate.local_addr().is_ok(), "the duplicate is still owned");
        drop(replacement);
        drop(duplicate);
    }
}

/// A listener on `port` with the given timers and no interaction port; the
/// dispatch path is never reached by the stalling helpers below.
async fn stalled_server(port: u16, timeouts: http::Timeouts) -> Listener {
    let mut options = JsObject::new();
    for (key, value) in [
        ("corpId", "corp-x"),
        ("agentId", "agent-1"),
        ("secret", "corp-secret"),
        ("token", "callback-token"),
        (
            "encodingAesKey",
            "MDEyMzQ1Njc4OWFiY2RlZmdoaWprbG1ub3BxcnN0dXY",
        ),
    ] {
        options.insert(key.into(), JsValue::String(JsString::from(value)));
    }
    options.insert("port".into(), JsValue::Number(f64::from(port)));
    let channel = WeComCallbackChannel::new(
        WeComCallbackOptions::from_object(&options, QYAPI),
        Arc::new(NoInteraction),
    );
    http::listen_with(
        channel.runtime.clone(),
        u32::from(port),
        timeouts,
        Some(channel.runtime.accepted.clone()),
    )
    .await
    .expect("#1336 stalled listen")
}

/// One client, one stalled request, one `stop()`: returns every byte the
/// client received within a fixed 3 s read deadline (empty ⇒ it received
/// nothing), whether that read reached a clean EOF inside the deadline, and
/// whether `stop()` returned within `budget`. Always releases the
/// client and waits for the port, so the two halves of an A/B can share one
/// port. The listener polls each connection once before it honours `stop()`,
/// so the bytes below are already in the socket buffer when the shutdown
/// branch is first considered — no sleep-based settle.
async fn stop_after_stalled_request(
    port: u16,
    timeouts: http::Timeouts,
    request: &[u8],
    budget: Duration,
) -> (Option<Vec<u8>>, bool, bool) {
    let listener = stalled_server(port, timeouts).await;
    let mut client = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1336 stalled connect");
    client
        .write_all(request)
        .await
        .expect("#1336 stalled write");
    let mut buf = Vec::new();
    // A stalled request answers nothing; only the `408` half reads a response.
    // Read to EOF rather than a single `read`: `connection: close` makes EOF
    // the terminator, and one `read` that happens to stop on a segment
    // boundary would let a response *with* a body slip past the "no body"
    // assertion — a pin that can falsely pass. Keep whatever arrived even when
    // the deadline expires or the read errors (#1336 r5 boundary/Codex P1):
    // dropping a partial `buf` would let an early response read as "no
    // answer" in the long-timeout half, the same false pass from the other
    // side. The EOF flag is what makes the `408` half complete: bytes that
    // stop at the header terminator because the deadline expired look exactly
    // like a bodyless response (#1336 r6 Codex P1). The deadline is 3 s, not
    // the 300 ms request timeout plus a thin margin: this crate runs thousands
    // of tests in parallel and a loaded host must not turn a correct 408 into a
    // false red (#1336 r7 boundary P2).
    let read = timeout(Duration::from_secs(3), client.read_to_end(&mut buf)).await;
    let complete = matches!(read, Ok(Ok(_)));
    let response = if buf.is_empty() { None } else { Some(buf) };
    let drained = timeout(budget, listener.stop())
        .await
        .is_ok_and(|result| result.is_ok());
    let _ = client.shutdown().await;
    drop(client);
    until("#1336 stalled listener teardown", || {
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
    })
    .await;
    (response, complete, drained)
}

/// #1941: the `POST *` body cap. A request that declares far more than
/// `MAX_BODY_BYTES` but sends one byte past the cap must be closed *at the
/// cap* — the listener must not keep reading (and buffering) the declared
/// body. The ending is the webhook precedent's zero-byte close, never a 413.
///
/// The declared length is 64 MiB larger than what the client sends on
/// purpose: a listener without the cap would still be waiting for the rest
/// (until `requestTimeout`) and this read would time out, so the cell proves
/// the cap bounds the *buffered* bytes rather than merely rejecting the
/// request after reading it all.
#[tokio::test]
async fn oversize_post_closes_at_the_cap_with_zero_bytes() {
    // 18991 / 18990 are unused by this module; 18993 belongs to the
    // concurrent-dispatch test's "nothing listens here" qyapi address.
    const PORT: u16 = 18991;
    let listener = stalled_server(
        PORT,
        http::Timeouts {
            headers: Duration::from_secs(30),
            request: Duration::from_secs(30),
        },
    )
    .await;
    let cap = MAX_BODY_BYTES as usize;
    let declared = cap + 64 * 1024 * 1024;
    let mut client = TcpStream::connect(("127.0.0.1", PORT))
        .await
        .expect("#1941 oversize connect");
    let head = format!(
        "POST /cb HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {declared}\r\n\r\n"
    );
    // The cap closes the socket mid-write, so a write error here is expected
    // and is not the observable under test.
    let _ = client.write_all(head.as_bytes()).await;
    let _ = client.write_all(&vec![b'a'; cap + 1]).await;
    let mut buf = Vec::new();
    let read = timeout(Duration::from_secs(3), client.read_to_end(&mut buf)).await;
    // The connection must END (clean EOF or reset) instead of waiting for the
    // undeclared remainder. A reset is still a zero-byte close on the wire,
    // so only the byte count below is asserted exactly.
    crate::negative_assert!(
        read.is_ok(),
        "#1941 an oversize POST ends at the cap instead of waiting for the declared body"
    );
    crate::negative_assert!(
        buf.is_empty(),
        "#1941 an oversize POST gets no response bytes (zero-byte close, not 413)"
    );
    let _ = client.shutdown().await;
    drop(client);
    let drained = timeout(Duration::from_secs(10), listener.stop())
        .await
        .is_ok_and(|result| result.is_ok());
    crate::negative_assert!(drained, "#1941 the oversize connection does not pin stop()");
    until("#1941 oversize listener teardown", || {
        std::net::TcpListener::bind(("127.0.0.1", PORT)).is_ok()
    })
    .await;
}

/// #1941 boundary: exactly `MAX_BODY_BYTES` is still read and parsed (400
/// `no Encrypt`), and a small body keeps the same path. Only a body that
/// crosses the cap destroys the connection, so the off-by-one cannot drift.
#[tokio::test]
async fn post_up_to_the_cap_is_still_read() {
    const PORT: u16 = 18990;
    let listener = stalled_server(
        PORT,
        http::Timeouts {
            headers: Duration::from_secs(30),
            request: Duration::from_secs(30),
        },
    )
    .await;
    let at_cap = "a".repeat(MAX_BODY_BYTES as usize);
    let response = http_request("#1941 at-cap", PORT, "POST", "/cb", Some(&at_cap)).await;
    crate::negative_assert_eq!(
        response["status"],
        400,
        "#1941 a body of exactly MAX_BODY_BYTES is still read"
    );
    crate::negative_assert_eq!(
        response["body"],
        "no Encrypt",
        "#1941 an at-cap body reaches the XML parse like before"
    );
    let small = http_request("#1941 small", PORT, "POST", "/cb", Some("hello")).await;
    crate::negative_assert_eq!(
        small["status"],
        400,
        "#1941 a small body still walks the normal path"
    );
    crate::negative_assert_eq!(
        small["body"],
        "no Encrypt",
        "#1941 a small body still reaches the XML parse"
    );
    let drained = timeout(Duration::from_secs(10), listener.stop())
        .await
        .is_ok_and(|result| result.is_ok());
    crate::negative_assert!(drained, "#1941 the at-cap listener stops cleanly");
    until("#1941 at-cap listener teardown", || {
        std::net::TcpListener::bind(("127.0.0.1", PORT)).is_ok()
    })
    .await;
}

/// #1336 r2 (concurrency P1): the active-chat claim is one critical section —
/// `contains`, `resolve` and `insert` under a single lock — so simultaneous
/// POSTs for the same chat dispatch exactly one turn. The recorded case
/// `post-duplicate-active` cannot see the difference, because it waits for the
/// first turn's submit before sending the second POST; a gate split back into
/// two `state.lock()` scopes passes it. Here eight POSTs race with no ordering
/// between them, and every one of them must either dispatch or be logged as
/// already active.
///
/// The reply path points at a loopback port nothing listens on, so this test
/// never touches the golden run's qyapi fake (18998) or its port.
///
/// Multi-threaded on purpose (#1336 r3 boundary/sibling P1): the regression
/// this pins is a claim block split back into two `state.lock()` scopes. The
/// block is pure synchronous code, so on the default `current_thread` runtime
/// no second task can run between the scopes and the mutant would pass. Two
/// workers, as the module's own comment assumes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_posts_for_one_chat_dispatch_once() {
    const PORT: u16 = 18994;
    const REQUESTS: usize = 8;
    let oracle = oracle();
    let case = oracle["cases"]
        .as_array()
        .expect("#1336 cases")
        .iter()
        .find(|case| case["name"].as_str() == Some("post-duplicate-active"))
        .expect("#1336 post-duplicate-active case")
        .clone();
    let post = case["steps"]
        .as_array()
        .expect("#1336 steps")
        .iter()
        .find(|step| step["op"].as_str() == Some("http"))
        .expect("#1336 concurrency POST step")
        .clone();
    let path = post["path"].as_str().expect("#1336 POST path").to_owned();
    let body = post["body"].as_str().expect("#1336 POST body").to_owned();

    let record = Arc::new(Mutex::new(Record::default()));
    let gates = Gates::default();
    let mut options = JsObject::new();
    for (key, value) in [
        ("corpId", "corp-x"),
        ("agentId", "agent-1"),
        ("secret", "corp-secret"),
        ("token", "cbToken1"),
        (
            "encodingAesKey",
            "MDEyMzQ1Njc4OWFiY2RlZmdoaWprbG1ub3BxcnN0dXY",
        ),
    ] {
        options.insert(key.into(), JsValue::String(JsString::from(value)));
    }
    options.insert("port".into(), JsValue::Number(f64::from(PORT)));
    // Nothing listens here: the winning turn's replies fail fast instead of
    // landing in the golden run's qyapi fake.
    let channel = WeComCallbackChannel::new(
        WeComCallbackOptions::from_object(&options, "http://127.0.0.1:18993/cgi-bin"),
        Arc::new(ScriptInteraction {
            script: json!({}),
            record: record.clone(),
        }),
    );
    let gateway: Arc<dyn TurnGateway> = Arc::new(MockInbound {
        turns: vec![TurnScript {
            gate_before: Some("hold".into()),
            events: Vec::new(),
            error: None,
        }],
        next: Mutex::new(0),
        record: record.clone(),
        gates: gates.clone(),
    });
    let logger = record.clone();
    let deps = ChannelStartDeps {
        config: None,
        report_status: None,
        log: Some(Arc::new(move |level, message| {
            logger
                .lock()
                .expect("#1336 record mutex")
                .logs
                .push(json!([level, message]));
        })),
        turn_gateway: Some(gateway),
    };
    let context = "case concurrent-posts".to_owned();
    let handle = bounded(&context, channel.start(deps))
        .await
        .expect("#1336 concurrent start");

    let mut requests = Vec::new();
    for _ in 0..REQUESTS {
        let context = context.clone();
        let path = path.clone();
        let body = body.clone();
        requests.push(tokio::spawn(async move {
            http_request(&context, PORT, "POST", &path, Some(&body)).await
        }));
    }
    // Every request either dispatched (exactly one) or logged the skip; wait
    // until all eight have decided instead of sleeping for a settle window.
    // The winning POST is still parked inside its turn, so its HTTP response
    // has not been written yet — awaiting the handles here would deadlock.
    until(&context, || {
        let record = record.lock().expect("#1336 record mutex");
        let skipped = record
            .logs
            .iter()
            .filter(|entry| {
                entry[1]
                    .as_str()
                    .is_some_and(|message| message.ends_with("already active, skipping"))
            })
            .count();
        record.gateway.len() + skipped >= REQUESTS
    })
    .await;
    let gateway_calls = record.lock().expect("#1336 record mutex").gateway.len();
    crate::negative_assert_eq!(
        gateway_calls,
        1,
        "#1336 concurrent posts for one chat dispatch exactly one turn"
    );
    // Open the gate: the winning turn streams its (empty) script and the
    // channel writes the one response all eight clients are waiting on.
    // `send_replace` (not `send`): with two workers the producer may not have
    // subscribed yet, and `watch::Sender::send` silently drops the value when
    // there is no receiver — the parked turn would then never be released.
    let _ = gate(&gates, "hold").send_replace(true);
    for request in requests {
        bounded(&context, request)
            .await
            .expect("#1336 concurrent request task");
    }
    bounded(&context, handle.stop("done"))
        .await
        .expect("#1336 concurrent stop");
}
