//! #1336: replay of the `wecom-callback` golden fixture — the same step
//! scripts the TS generator ran against the real channel sources (Node
//! v22.22.0, baseline cc7c4e6), driven through the native hyper listener with
//! the same loopback qyapi fake, a scripted inbound gateway and the same
//! scripted interaction port — plus the component pins (config `Number(port)`
//! semantics, crypto envelopes, UTF-16 signature sort, outbound body
//! truncation) that keep every assertion on a recorded observable.

use super::super::protocol::{TurnEventStream, TurnGateway, TurnRequest};
use super::crypto::sha1_hex;
use super::interaction::PermissionAnswer;
use super::*;
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
const DEADLINE: Duration = Duration::from_secs(5);
/// `tcp-open` settles briefly before the next step (a `stop-pending`) so the
/// listener's accept loop has accepted the connection and the request is
/// parked on its body. Node's event loop processes the server-side accept in
/// the same iteration that fires the client's 'connect' event; a raw Rust
/// executor has no such pairing, so the settle provides the equivalent point.
const ACCEPT_SETTLE: Duration = Duration::from_millis(25);

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
    cases
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    timeout(DEADLINE, future).await.expect("#1336 bounded wait")
}

async fn until(predicate: impl Fn() -> bool) {
    let started = Instant::now();
    while !predicate() {
        assert!(started.elapsed() < DEADLINE, "#1336 wait condition timeout");
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
        Box::pin(async move {
            Ok(Box::new(MockStream(stream)) as Box<dyn TurnEventStream>)
        })
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
    let listener = TcpListener::bind(("127.0.0.1", QYAPI_PORT))
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

fn parse_raw_response(raw: &[u8]) -> Value {
    let header_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("#1336 response header end");
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

async fn http_request(port: u16, method: &str, path: &str, body: Option<&str>) -> Value {
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
    let _ = stream.read_to_end(&mut raw).await;
    parse_raw_response(&raw)
}

// ---------------------------------------------------------------- replay

async fn replay(case: &Value, qyapi: &Arc<Qyapi>) -> Value {
    let record = Arc::new(Mutex::new(Record::default()));
    qyapi.arm(case, record.clone());
    let gates = Gates::default();

    for name in [
        "WECOM_CORP_ID",
        "WECOM_AGENT_ID",
        "WECOM_CB_SECRET",
        "WECOM_CB_TOKEN",
        "WECOM_ENCODING_AES_KEY",
        "WECOM_CB_PORT",
    ] {
        assert!(
            std::env::var_os(name).is_none(),
            "#1336 replay requires {name} to be unset"
        );
    }

    let config = case["config"].as_object().expect("#1336 case config");
    let mut options = JsObject::new();
    for key in ["corpId", "agentId", "secret", "token", "encodingAesKey"] {
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
                    bounded(channel.start(deps.clone()))
                        .await
                        .expect("#1336 start"),
                );
            }
            "http" => {
                let result = http_request(
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
                bounded(handle.as_ref().expect("#1336 started").stop(reason))
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
                pending.insert(
                    step["label"].as_str().expect("#1336 stop label").to_owned(),
                    tokio::spawn(async move {
                        let _ = bounded(stop.stop(&reason)).await;
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
                tokio::time::sleep(ACCEPT_SETTLE).await;
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
                let _ = stream.read_to_end(&mut raw).await;
                record
                    .lock()
                    .expect("#1336 record mutex")
                    .http
                    .push(parse_raw_response(&raw));
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
                    until(move || {
                        record.lock().expect("#1336 record mutex").qyapi.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["helpers"].as_u64() {
                    let record = record.clone();
                    until(move || {
                        record.lock().expect("#1336 record mutex").helpers.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["gateway"].as_u64() {
                    let record = record.clone();
                    until(move || {
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
}
