//! #1333: replay the real TS `ApiServerChannel` cells (wire bytes over a
//! loopback listener) and the lifecycle matrix with a scripted A7 port.
//!
//! The external world is a mock `TurnGateway` only; no account, model or user
//! home is touched. Wire requests are raw HTTP/1.1 so the target the TS
//! generator sent (`//host/health`, `/v1/../health`) is reproduced verbatim.
use super::*;
use pilotdeck_protocol::{JsString, JsValue};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Notify,
};

const ORACLE: &str = include_str!("../../../tests/fixtures/api-server-channel/oracle.json");
const FIXED_UUID: &str = "00000000-0000-4000-8000-000000000000";
const FIXED_DATE: &str = "Thu, 01 Jan 1970 00:00:00 GMT";

fn oracle() -> Value {
    crate::test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1333 api-server oracle JSON")
}

#[derive(Default, Debug)]
struct RawResponse {
    status: Option<u16>,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    error: Option<String>,
}

struct ReqSpec {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    keep_alive: bool,
}

impl ReqSpec {
    fn from_json(value: &Value) -> Self {
        let headers = value["headers"]
            .as_object()
            .map(|headers| {
                headers
                    .iter()
                    .map(|(name, value)| (name.clone(), value.as_str().unwrap_or("").to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let body = value["body"].as_str().unwrap_or("").as_bytes().to_vec();
        let keep_alive = value["headers"]["connection"]
            .as_str()
            .map(|value| value.eq_ignore_ascii_case("keep-alive"))
            .unwrap_or(false);
        Self {
            method: value["method"].as_str().unwrap_or("GET").to_owned(),
            path: value["path"].as_str().unwrap_or("/").to_owned(),
            headers,
            body,
            keep_alive,
        }
    }

    fn post(path: &str, chat: &str, body: &str) -> Self {
        Self {
            method: "POST".to_owned(),
            path: path.to_owned(),
            headers: vec![("x-hermes-session-id".to_owned(), chat.to_owned())],
            body: body.as_bytes().to_vec(),
            keep_alive: false,
        }
    }

    fn get(path: &str) -> Self {
        Self {
            method: "GET".to_owned(),
            path: path.to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
            keep_alive: false,
        }
    }
}

fn parse_head(data: &[u8]) -> Option<(usize, u16, BTreeMap<String, String>)> {
    let end = data.windows(4).position(|window| window == b"\r\n\r\n")? + 4;
    let head = std::str::from_utf8(&data[..end]).ok()?;
    let mut lines = head.split("\r\n");
    let status = lines.next()?.split(' ').nth(1)?.parse().ok()?;
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    Some((end, status, headers))
}

fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = body.windows(2).position(|window| window == b"\r\n") {
        let Ok(size) =
            usize::from_str_radix(std::str::from_utf8(&body[..line_end]).unwrap_or("0"), 16)
        else {
            break;
        };
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        let take = size.min(body.len());
        out.extend_from_slice(&body[..take]);
        body = &body[take..];
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        }
    }
    out
}

fn complete(data: &[u8], method: &str) -> bool {
    let Some((end, status, headers)) = parse_head(data) else {
        return false;
    };
    if method.eq_ignore_ascii_case("HEAD") || status == 204 {
        return true;
    }
    if headers
        .get("transfer-encoding")
        .map(|value| value.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
    {
        return data.ends_with(b"\r\n0\r\n\r\n");
    }
    match headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        Some(length) => data.len() >= end + length,
        None => false,
    }
}

async fn exchange(addr: std::net::SocketAddr, spec: &ReqSpec) -> RawResponse {
    match tokio::time::timeout(Duration::from_secs(10), exchange_inner(addr, spec)).await {
        Ok(response) => response,
        Err(_) => RawResponse {
            error: Some("timeout".to_owned()),
            ..RawResponse::default()
        },
    }
}

async fn exchange_inner(addr: std::net::SocketAddr, spec: &ReqSpec) -> RawResponse {
    let mut stream = match TcpStream::connect(addr).await {
        Ok(stream) => stream,
        Err(error) => {
            return RawResponse {
                error: Some(error.to_string()),
                ..RawResponse::default()
            };
        }
    };
    let mut request = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\n",
        spec.method, spec.path, addr
    );
    let mut has_length = false;
    for (name, value) in &spec.headers {
        if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("content-length") {
            has_length |= name.eq_ignore_ascii_case("content-length");
        }
        if name.eq_ignore_ascii_case("connection") && !spec.keep_alive {
            continue;
        }
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if !spec.keep_alive
        && !spec
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("connection"))
    {
        request.push_str("Connection: close\r\n");
    }
    if !has_length && !spec.body.is_empty() {
        request.push_str(&format!("Content-Length: {}\r\n", spec.body.len()));
    }
    request.push_str("\r\n");
    let mut wire = request.into_bytes();
    wire.extend_from_slice(&spec.body);
    let write = async {
        if let Err(error) = stream.write_all(&wire).await {
            return Some(error.to_string());
        }
        None
    };
    let write_error = write.await;

    let mut data = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        if complete(&data, &spec.method) {
            break;
        }
        match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(count) => data.extend_from_slice(&buffer[..count]),
            Err(error) => {
                return RawResponse {
                    error: Some(error.to_string()),
                    body: data,
                    ..RawResponse::default()
                };
            }
        }
        if data.len() > 4_000_000 {
            break;
        }
    }
    let Some((end, status, headers)) = parse_head(&data) else {
        return RawResponse {
            error: Some(write_error.unwrap_or_else(|| "connection closed".to_owned())),
            body: data,
            ..RawResponse::default()
        };
    };
    let mut body = data[end..].to_vec();
    if spec.method.eq_ignore_ascii_case("HEAD") || status == 204 {
        body.clear();
    } else if headers
        .get("transfer-encoding")
        .map(|value| value.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
    {
        body = dechunk(&body);
    } else if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        body.truncate(length);
    }
    RawResponse {
        status: Some(status),
        headers,
        body,
        error: None,
    }
}

fn mask_text(text: &str) -> String {
    let mut ids = String::new();
    let mut rest = text;
    while let Some(position) = rest.find("\"id\":\"chatcmpl-") {
        ids.push_str(&rest[..position]);
        ids.push_str("\"id\":\"chatcmpl-FIXED\"");
        let after = &rest[position + "\"id\":\"chatcmpl-".len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        rest = &after[digits..];
        if let Some(stripped) = rest.strip_prefix('"') {
            rest = stripped;
        }
    }
    ids.push_str(rest);
    let mut created = String::new();
    let mut rest = ids.as_str();
    while let Some(position) = rest.find("\"created\":") {
        created.push_str(&rest[..position]);
        created.push_str("\"created\":0");
        let after = &rest[position + "\"created\":".len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        rest = &after[digits..];
    }
    created.push_str(rest);
    created.replace(&format!("api-{FIXED_UUID}"), "api-FIXED")
}

fn actual_headers(response: &RawResponse) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in &response.headers {
        out.insert(
            name.clone(),
            if name == "date" {
                FIXED_DATE.to_owned()
            } else {
                mask_text(value)
            },
        );
    }
    out
}

fn compare_response(case: &str, index: usize, expected: &Value, actual: &RawResponse) {
    if expected.get("error").is_some() {
        assert!(
            actual.error.is_some(),
            "#1333 {case} request {index}: expected a zero-byte connection close, got status {:?} body {:?}",
            actual.status,
            String::from_utf8_lossy(&actual.body)
        );
        return;
    }
    assert_eq!(
        actual.status,
        expected["status"].as_u64().map(|status| status as u16),
        "#1333 {case} request {index}: status differs (body {:?})",
        String::from_utf8_lossy(&actual.body)
    );
    let expected_headers = expected["headers"]
        .as_object()
        .expect("#1333 oracle headers")
        .iter()
        .map(|(name, value)| {
            (
                name.to_ascii_lowercase(),
                value.as_str().unwrap_or("").to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual_headers(actual),
        expected_headers,
        "#1333 {case} request {index}: headers differ"
    );
    assert_eq!(
        mask_text(&String::from_utf8_lossy(&actual.body)),
        expected["body"].as_str().expect("#1333 oracle body"),
        "#1333 {case} request {index}: body differs"
    );
}

struct Latch {
    started: AtomicBool,
    released: AtomicBool,
    started_notify: Notify,
    released_notify: Notify,
}

impl Latch {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: AtomicBool::new(false),
            released: AtomicBool::new(false),
            started_notify: Notify::new(),
            released_notify: Notify::new(),
        })
    }
    async fn wait_started(&self) {
        loop {
            let notified = self.started_notify.notified();
            if self.started.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.released_notify.notify_one();
    }
    async fn wait_release(&self) {
        loop {
            let notified = self.released_notify.notified();
            if self.released.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

struct ScriptedTurn {
    events: Vec<Value>,
    error: Option<String>,
}

struct ScriptStream {
    events: VecDeque<JsValue>,
    error: Option<String>,
    latch: Option<Arc<Latch>>,
    dropped: Arc<AtomicBool>,
}

impl TurnEventStream for ScriptStream {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move {
            if let Some(latch) = self.latch.take() {
                latch.started.store(true, Ordering::SeqCst);
                latch.started_notify.notify_one();
                latch.wait_release().await;
            }
            if let Some(error) = self.error.take() {
                return Err(ChannelError::Error(error));
            }
            Ok(self.events.pop_front())
        })
    }
}

impl Drop for ScriptStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

struct ScriptedGateway {
    turns: Vec<ScriptedTurn>,
    calls: Arc<Mutex<Vec<Value>>>,
    latch: Option<Arc<Latch>>,
    dropped: Arc<AtomicBool>,
    index: Mutex<usize>,
}

impl ScriptedGateway {
    fn with_latch(
        turns: Vec<ScriptedTurn>,
        latch: Option<Arc<Latch>>,
        dropped: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            turns,
            calls: Arc::default(),
            latch,
            dropped,
            index: Mutex::new(0),
        })
    }
}

impl TurnGateway for ScriptedGateway {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let mut index = self.index.lock().expect("#1333 scripted gateway index");
        assert!(
            !self.turns.is_empty(),
            "#1333 scripted gateway needs at least one turn"
        );
        let position = (*index).min(self.turns.len() - 1);
        let turn = &self.turns[position];
        *index += 1;
        let events = turn
            .events
            .iter()
            .map(|value| {
                pilotdeck_protocol::parse_json(&value.to_string())
                    .expect("#1333 scripted turn event")
            })
            .collect();
        let error = turn.error.clone();
        self.calls
            .lock()
            .expect("#1333 scripted gateway calls")
            .push(json!({
                "sessionKey": request.session_key.to_utf8().unwrap_or_default(),
                "channelKey": request.channel_key.to_string(),
                "message": request.message.to_utf8().unwrap_or_default(),
            }));
        let latch = self.latch.clone();
        let dropped = self.dropped.clone();
        Box::pin(async move {
            Ok(Box::new(ScriptStream {
                events,
                error,
                latch,
                dropped,
            }) as Box<dyn TurnEventStream>)
        })
    }
}

fn js_string_array(values: &[Value]) -> JsValue {
    JsValue::Array(
        values
            .iter()
            .map(|value| JsValue::String(JsString::from(value.as_str().unwrap_or(""))))
            .collect(),
    )
}

fn options(case: &Value, timeout: Option<Duration>) -> ApiServerOptions {
    let options = &case["options"];
    ApiServerOptions {
        port: Some(JsValue::Number(0.0)),
        api_key: Some(options["apiKey"].as_str().unwrap_or("").to_owned()),
        host: Some("127.0.0.1".to_owned()),
        model_name: Some(
            options["modelName"]
                .as_str()
                .unwrap_or("parity-model")
                .to_owned(),
        ),
        cors_origins: options["corsOrigins"]
            .as_array()
            .map(|values| js_string_array(values)),
        mapper: None,
        request_timeout: timeout,
        uuid: Some(Arc::new(|| FIXED_UUID.to_owned())),
    }
}

struct Started {
    channel: ApiServerChannel,
    handle: Arc<dyn ChannelHandle>,
    addr: std::net::SocketAddr,
    logs: Arc<Mutex<Vec<Value>>>,
}

async fn start(options: ApiServerOptions, gateway: Option<Arc<dyn TurnGateway>>) -> Started {
    let channel = ApiServerChannel::with_env(options, HashMap::new());
    let logs: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = logs.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: Some(Arc::new(move |level: &str, message: &str| {
                sink.lock()
                    .expect("#1333 log sink")
                    .push(json!([level, mask_text(message)]));
            })),
            turn_gateway: gateway,
        })
        .await
        .expect("#1333 channel start");
    let addr = channel.bound_addr().expect("#1333 bound listener address");
    Started {
        channel,
        handle,
        addr,
        logs,
    }
}

fn gateway_from(
    case: &Value,
    latch: Option<Arc<Latch>>,
    dropped: Arc<AtomicBool>,
) -> Arc<ScriptedGateway> {
    let turns = case["gateway"]["turns"]
        .as_array()
        .map(|turns| {
            turns
                .iter()
                .map(|turn| ScriptedTurn {
                    events: turn["events"].as_array().cloned().unwrap_or_default(),
                    error: turn["error"].as_str().map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default();
    ScriptedGateway::with_latch(turns, latch, dropped)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replays_real_ts_api_server_cells() {
    let oracle = oracle();
    assert_eq!(
        oracle["sourceCommit"], "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",
        "#1333 baseline commit"
    );
    assert_eq!(oracle["node"], "v22.22.0", "#1333 Node baseline");
    let cases = oracle["cases"].as_array().expect("#1333 oracle cases");
    assert!(
        cases.len() >= 13,
        "#1333 oracle must keep every surface case"
    );
    for case in cases {
        let id = case["id"].as_str().expect("#1333 case id");
        if id.starts_with("oversized-") {
            // 1 MB writes are exercised separately by `oversized_body_closes`
            // to keep this replay's per-request timeout predictable.
            continue;
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let gateway = case
            .get("gateway")
            .map(|_| gateway_from(case, None, dropped.clone()) as Arc<dyn TurnGateway>);
        let started = start(options(case, None), gateway.clone()).await;
        let requests = case["requests"].as_array().expect("#1333 requests");
        for (index, request) in requests.iter().enumerate() {
            let spec = ReqSpec::from_json(request);
            let response = exchange(started.addr, &spec).await;
            compare_response(id, index, &case["expect"][index], &response);
        }
        started
            .handle
            .stop("surface-done")
            .await
            .expect("#1333 channel stop");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_inputs_match_real_ts() {
    let oracle = oracle();
    for case in oracle["cases"].as_array().expect("#1333 oracle cases") {
        let Some(expected) = case.get("submits") else {
            continue;
        };
        let id = case["id"].as_str().unwrap_or("");
        let dropped = Arc::new(AtomicBool::new(false));
        let gateway = gateway_from(case, None, dropped);
        let started = start(
            options(case, None),
            Some(gateway.clone() as Arc<dyn TurnGateway>),
        )
        .await;
        for request in case["requests"].as_array().expect("#1333 requests") {
            let spec = ReqSpec::from_json(request);
            let response = exchange(started.addr, &spec).await;
            assert!(
                response.status.is_some(),
                "#1333 {id} request must reach the listener"
            );
        }
        started.handle.stop("surface-done").await.unwrap();
        let calls = gateway.calls.lock().expect("#1333 gateway calls").clone();
        assert_eq!(
            Value::Array(calls),
            *expected,
            "#1333 {id}: submitTurn inputs differ from the real TS channel"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_drains_in_flight_buffered_turn() {
    let latch = Latch::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let gateway = ScriptedGateway::with_latch(
        vec![ScriptedTurn {
            events: vec![json!({"type":"assistant_text_delta","text":"done"})],
            error: None,
        }],
        Some(latch.clone()),
        dropped,
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway.clone())).await;
    let addr = started.addr;
    let spec = ReqSpec::post(
        "/v1/chat/completions",
        "chat-drain",
        "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
    );
    let request = tokio::spawn(async move { exchange(addr, &spec).await });
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 buffered turn reached the mock gateway");
    let handle = started.handle.clone();
    let stop = tokio::spawn(async move { handle.stop("drain-probe").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !stop.is_finished(),
        "#1333 stop must stay pending while a buffered turn is in flight"
    );
    latch.release();
    let response = request.await.expect("#1333 buffered request task");
    assert_eq!(response.status, Some(200), "#1333 buffered turn completes");
    let stop = tokio::time::timeout(Duration::from_secs(5), stop)
        .await
        .expect("#1333 stop resolves after drain")
        .expect("#1333 stop task");
    assert!(stop.is_ok(), "#1333 stop result");
    let logs = started.logs.lock().expect("#1333 logs").clone();
    assert!(
        logs.iter()
            .any(|entry| entry[1] == "api-server: stopping (drain-probe)"),
        "#1333 stop reason is logged: {logs:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_drains_in_flight_stream() {
    let latch = Latch::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let gateway = ScriptedGateway::with_latch(
        vec![ScriptedTurn {
            events: vec![json!({"type":"assistant_text_delta","text":"streamed"})],
            error: None,
        }],
        Some(latch.clone()),
        dropped,
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway)).await;
    let addr = started.addr;
    let spec = ReqSpec::post(
        "/v1/chat/completions",
        "chat-drain-stream",
        "{\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
    );
    let request = tokio::spawn(async move { exchange(addr, &spec).await });
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 stream reached the mock gateway");
    let handle = started.handle.clone();
    let stop = tokio::spawn(async move { handle.stop("drain-probe").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !stop.is_finished(),
        "#1333 stop must stay pending while an SSE stream is in flight"
    );
    latch.release();
    let response = request.await.expect("#1333 stream request task");
    assert_eq!(response.status, Some(200), "#1333 stream status");
    let body = String::from_utf8_lossy(&response.body);
    assert!(
        body.contains("\"content\":\"streamed\"") && body.contains("data: [DONE]"),
        "#1333 stream keeps writing while stop waits: {body:?}"
    );
    tokio::time::timeout(Duration::from_secs(5), stop)
        .await
        .expect("#1333 stop resolves after the stream ends")
        .expect("#1333 stop task")
        .expect("#1333 stop result");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_chat_is_busy_then_free() {
    let latch = Latch::new();
    let gateway = ScriptedGateway::with_latch(
        vec![
            ScriptedTurn {
                events: vec![json!({"type":"assistant_text_delta","text":"first"})],
                error: None,
            },
            ScriptedTurn {
                events: vec![json!({"type":"assistant_text_delta","text":"second"})],
                error: None,
            },
        ],
        Some(latch.clone()),
        Arc::new(AtomicBool::new(false)),
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway.clone())).await;
    let body = "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}";
    let addr = started.addr;
    let spec = ReqSpec::post("/v1/chat/completions", "chat-dup", body);
    let first = tokio::spawn(async move { exchange(addr, &spec).await });
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 first turn started");
    let second = exchange(
        started.addr,
        &ReqSpec::post("/v1/chat/completions", "chat-dup", body),
    )
    .await;
    assert_eq!(second.status, Some(429), "#1333 duplicate chat is rejected");
    let text = String::from_utf8_lossy(&second.body);
    assert!(
        text.contains("\"code\":\"session_busy\"") && text.contains("\"scope\":\"session\""),
        "#1333 duplicate chat error body: {text}"
    );
    latch.release();
    let first = first.await.expect("#1333 first request task");
    assert_eq!(first.status, Some(200), "#1333 first turn completes");
    let third = exchange(
        started.addr,
        &ReqSpec::post("/v1/chat/completions", "chat-dup", body),
    )
    .await;
    assert_eq!(
        third.status,
        Some(200),
        "#1333 chat is reusable after the turn"
    );
    assert_eq!(
        gateway.calls.lock().expect("#1333 calls").len(),
        2,
        "#1333 rejected duplicate is never submitted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_rebinds_listener_and_stop_is_idempotent() {
    let started = start(options(&json!({"options":{}}), None), None).await;
    let health = exchange(started.addr, &ReqSpec::get("/health")).await;
    assert_eq!(
        health.status,
        Some(200),
        "#1333 first listener serves /health"
    );
    started
        .handle
        .stop("first")
        .await
        .expect("#1333 first stop");
    started
        .handle
        .stop("again")
        .await
        .expect("#1333 stop is idempotent");
    let closed = exchange(started.addr, &ReqSpec::get("/health")).await;
    assert!(
        closed.error.is_some(),
        "#1333 stopped listener refuses connections"
    );
    let restarted = started
        .channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#1333 restart");
    let addr = started.channel.bound_addr().expect("#1333 rebound address");
    let health = exchange(addr, &ReqSpec::get("/health")).await;
    assert_eq!(
        health.status,
        Some(200),
        "#1333 restarted listener serves /health"
    );
    restarted.stop("last").await.expect("#1333 last stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_disconnect_releases_chat_without_resubmit() {
    let latch = Latch::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let gateway = ScriptedGateway::with_latch(
        vec![
            ScriptedTurn {
                events: vec![json!({"type":"assistant_text_delta","text":"never delivered"})],
                error: None,
            },
            ScriptedTurn {
                events: vec![json!({"type":"assistant_text_delta","text":"ok"})],
                error: None,
            },
        ],
        Some(latch.clone()),
        dropped.clone(),
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway.clone())).await;
    // Send the SSE request, read the response head, then drop the client
    // socket while the mock turn is still held.
    let mut socket = TcpStream::connect(started.addr).await.unwrap();
    let body = "{\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}";
    let wire = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nx-hermes-session-id: chat-cancel\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(wire.as_bytes()).await.unwrap();
    let mut head = [0u8; 256];
    let _ = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut head))
        .await
        .expect("#1333 SSE response head");
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 cancel turn started");
    drop(socket);
    latch.release();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1333 disconnected stream is dropped");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !started
            .channel
            .0
            .active
            .lock()
            .expect("#1333 active chats")
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1333 disconnect releases the active chat guard");
    let body = "{\"messages\":[{\"role\":\"user\",\"content\":\"again\"}]}";
    let response = exchange(
        started.addr,
        &ReqSpec::post("/v1/chat/completions", "chat-cancel", body),
    )
    .await;
    assert_eq!(
        response.status,
        Some(200),
        "#1333 chat is free after disconnect"
    );
    assert_eq!(
        gateway.calls.lock().expect("#1333 calls").len(),
        2,
        "#1333 disconnect must not resubmit the cancelled turn"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn null_body_is_registered_divergence() {
    // TS cc7c4e6 crashes the process on a `null` body (probe4.out); Rust
    // answers the ordinary `messages_required` 400 instead. Registered in
    // specs/gateway-parity/diffs/A8.3-API-SERVER-001.md.
    let started = start(options(&json!({"options":{}}), None), None).await;
    let response = exchange(
        started.addr,
        &ReqSpec::post("/v1/chat/completions", "chat-null", "null"),
    )
    .await;
    assert_eq!(response.status, Some(400), "#1333 null body stays a 400");
    assert!(
        String::from_utf8_lossy(&response.body).contains("\"code\":\"messages_required\""),
        "#1333 null body uses messages_required"
    );
    started.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_body_closes_without_response() {
    let started = start(options(&json!({"options":{}}), None), None).await;
    let body = "x".repeat(1_000_001);
    let response = exchange(
        started.addr,
        &ReqSpec {
            method: "POST".to_owned(),
            path: "/v1/chat/completions".to_owned(),
            headers: Vec::new(),
            body: body.into_bytes(),
            keep_alive: false,
        },
    )
    .await;
    assert!(
        response.error.is_some() && response.status.is_none(),
        "#1333 oversized body closes with zero bytes, got status {:?}",
        response.status
    );
    started.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_close_request_uses_hyper_close_header() {
    // Registered divergence A8.3-API-SERVER-002: Node keeps the handler's
    // `Connection: keep-alive` even when the request says `close`; hyper
    // 1.7 forces `close` once the request disabled keep-alive. The oracle's
    // SSE cells use keep-alive requests, so `replays_real_ts_api_server_cells`
    // does not cover this corner; this test pins the Rust side.
    let gateway = ScriptedGateway::with_latch(
        vec![ScriptedTurn {
            events: vec![json!({"type":"assistant_text_delta","text":"close"})],
            error: None,
        }],
        None,
        Arc::new(AtomicBool::new(false)),
    );
    let started = start(options(&json!({"options":{}}), None), Some(gateway)).await;
    let response = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-close",
            "{\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
        ),
    )
    .await;
    assert_eq!(response.status, Some(200), "#1333 close SSE status");
    assert_eq!(
        response.headers.get("connection").map(String::as_str),
        Some("close"),
        "#1333 hyper forces close when the request asked for it"
    );
    assert!(
        String::from_utf8_lossy(&response.body).contains("data: [DONE]"),
        "#1333 close SSE still completes the stream"
    );
    started.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_listening_log_and_bind_failure_returns_noop() {
    let first = start(options(&json!({"options":{}}), None), None).await;
    let logs = first.logs.lock().expect("#1333 logs").clone();
    assert!(
        logs.iter().any(|entry| {
            entry[0] == "info"
                && entry[1]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("api-server: listening on http://127.0.0.1:")
        }),
        "#1333 listening log: {logs:?}"
    );

    let mut busy = options(&json!({"options":{}}), None);
    busy.port = Some(JsValue::Number(f64::from(first.addr.port())));
    let second = ApiServerChannel::with_env(busy, HashMap::new());
    let failed_logs: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = failed_logs.clone();
    let noop = second
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: Some(Arc::new(move |level: &str, message: &str| {
                sink.lock()
                    .expect("#1333 bind failure log")
                    .push(json!([level, message]));
            })),
            turn_gateway: None,
        })
        .await
        .expect("#1333 bind failure still returns Ok");
    assert!(
        second.bound_addr().is_none(),
        "#1333 bind failure returns a no-op handle"
    );
    let logs = failed_logs.lock().expect("#1333 bind failure logs").clone();
    assert!(
        logs.iter().any(|entry| entry[0] == "error"
            && entry[1]
                .as_str()
                .unwrap_or("")
                .starts_with("api-server: failed to start:")),
        "#1333 bind failure log: {logs:?}"
    );
    noop.stop("noop").await.expect("#1333 no-op stop");
    first.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_closes_idle_keep_alive_connection() {
    let started = start(options(&json!({"options":{}}), None), None).await;
    let mut socket = TcpStream::connect(started.addr).await.unwrap();
    let wire = format!(
        "GET /health HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\n\r\n",
        started.addr
    );
    socket.write_all(wire.as_bytes()).await.unwrap();
    let mut buffer = [0u8; 1024];
    let read = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
        .await
        .expect("#1333 idle keep-alive response")
        .expect("#1333 idle keep-alive read");
    assert!(
        String::from_utf8_lossy(&buffer[..read]).starts_with("HTTP/1.1 200"),
        "#1333 idle keep-alive health response"
    );
    tokio::time::timeout(Duration::from_secs(2), started.handle.stop("idle-close"))
        .await
        .expect("#1333 stop resolves with an idle keep-alive connection")
        .expect("#1333 idle stop");
    let closed = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buffer))
        .await
        .expect("#1333 idle socket closes on stop")
        .expect("#1333 idle socket read after stop");
    assert_eq!(closed, 0, "#1333 idle keep-alive socket is closed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_timeout_ends_without_done() {
    let latch = Latch::new();
    let gateway = ScriptedGateway::with_latch(
        vec![ScriptedTurn {
            events: vec![json!({"type":"assistant_text_delta","text":"late"})],
            error: None,
        }],
        Some(latch.clone()),
        Arc::new(AtomicBool::new(false)),
    );
    let started = start(
        options(&json!({"options":{}}), Some(Duration::from_millis(100))),
        Some(gateway),
    )
    .await;
    let response = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-timeout",
            "{\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
        ),
    )
    .await;
    assert_eq!(response.status, Some(200), "#1333 timeout SSE status");
    let body = String::from_utf8_lossy(&response.body);
    assert!(
        !body.contains("[DONE]"),
        "#1333 timed-out stream must not close with [DONE]: {body:?}"
    );
    latch.release();
    started.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buffered_timeout_only_warns_and_keeps_turn() {
    let latch = Latch::new();
    let gateway = ScriptedGateway::with_latch(
        vec![ScriptedTurn {
            events: vec![json!({"type":"assistant_text_delta","text":"late"})],
            error: None,
        }],
        Some(latch.clone()),
        Arc::new(AtomicBool::new(false)),
    );
    let started = start(
        options(&json!({"options":{}}), Some(Duration::from_millis(100))),
        Some(gateway),
    )
    .await;
    let addr = started.addr;
    let spec = ReqSpec::post(
        "/v1/chat/completions",
        "chat-buffered-timeout",
        "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
    );
    let request = tokio::spawn(async move { exchange(addr, &spec).await });
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 buffered timeout turn started");
    tokio::time::sleep(Duration::from_millis(250)).await;
    let logs = started.logs.lock().expect("#1333 timeout logs").clone();
    assert!(
        logs.iter().any(|entry| {
            entry[0] == "warn"
                && entry[1]
                    .as_str()
                    .unwrap_or("")
                    .contains("buffered turn timeout for chat-buffered-timeout")
        }),
        "#1333 buffered timeout only warns: {logs:?}"
    );
    assert!(
        !request.is_finished(),
        "#1333 buffered timeout must not abort the turn"
    );
    latch.release();
    let response = request.await.expect("#1333 buffered timeout request");
    assert_eq!(
        response.status,
        Some(200),
        "#1333 buffered timeout completes"
    );
    assert!(
        String::from_utf8_lossy(&response.body).contains("\"content\":\"late\""),
        "#1333 buffered timeout keeps the eventual reply"
    );
    started.handle.stop("done").await.unwrap();
}
