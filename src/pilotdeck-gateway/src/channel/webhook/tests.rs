//! #1334: byte-level replay of the `webhook-channel` golden fixture (every
//! scenario one test, compared through the recorder's own normalization)
//! plus the lifecycle and state cells a real-clock recording cannot reach
//! on its own — fake-clock prune boundaries, the sliding rate-limit window,
//! the active-chat reentry guard, a stop during an in-flight turn, double
//! stop, hot-reload port release, the real `/new` command and the
//! delivery-id fallback. The compile-time negative-control switches bound
//! to these tests live in the component sources.
use super::*;
use crate::channel::protocol::{
    ChannelAdapter, ChannelError, ChannelGateway, ChannelKey, ChannelStartDeps, ChannelTurn,
    TurnEvent, TurnEvents,
};
use crate::user_dialog::NODE_408_RESPONSE;
use crate::{negative_assert, negative_assert_eq, test_baseline};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    time::timeout,
};

const ORACLE: &str = include_str!("../../../tests/fixtures/webhook-channel/oracle.json");
/// The recorder's silence probe for a null-body cell that never responds.
const PROBE: Duration = Duration::from_millis(400);
/// The recorder's overall exchange deadline.
const DEADLINE: Duration = Duration::from_millis(5000);
/// The recorder's quiet poll between `handleIncoming` observations.
const POLL: Duration = Duration::from_millis(25);
/// ...at most this many polls, like the generator.
const POLLS: usize = 200;
/// The generator's `mapperFixedUuid` option.
const FIXED_UUID: &str = "00000000-0000-4000-8000-000000000000";
/// The 408 the body-read deadline writes before closing the connection.
/// Node v22.22.0's raw `requestTimeoutResponse` is 51 unframed bytes
/// ([`NODE_408_RESPONSE`], probe
/// `_tmp/20261026-issue-1334/fix-r1/node-probe/probe4.out`); hyper's public
/// API frames an unknown-length HTTP/1.1 body as chunked, so the wire adds
/// `Transfer-Encoding: chunked` + `0\r\n\r\n`. The test
/// `webhook_408_is_node_bytes_plus_hyper_framing` below pins exactly that
/// relationship; the entry registering the framing is
/// `specs/gateway-parity/diffs/A8.3-CH-WEBHOOK-001.md` (draft, not an
/// accepted diff).
const WEBHOOK_REQUEST_TIMEOUT_RESPONSE: &[u8] = b"HTTP/1.1 408 Request Timeout\r\nConnection: \
     close\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";

/// hyper's chunked framing of an empty, unknown-length body.
const HYPER_CHUNKED_EMPTY_FRAMING: &[u8] = b"Transfer-Encoding: chunked\r\n\r\n0\r\n\r\n";

/// The pinned 408 bytes are Node's raw 51 bytes minus their terminating CRLF
/// plus hyper's chunked framing: the body-read assertion pins a presentation
/// difference from the baseline, not "the Node 408".
#[test]
fn webhook_408_is_node_bytes_plus_hyper_framing() {
    let node = NODE_408_RESPONSE;
    let mut expected = node[..node.len() - 2].to_vec();
    expected.extend_from_slice(HYPER_CHUNKED_EMPTY_FRAMING);
    negative_assert_eq!(
        WEBHOOK_REQUEST_TIMEOUT_RESPONSE,
        expected.as_slice(),
        "#1334 hyper's 408 is Node's unframed 408 plus the chunked framing"
    );
}

fn oracle() -> Value {
    test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1334 oracle JSON")
}

fn case(name: &str) -> Value {
    let value = oracle();
    value["cases"]
        .as_array()
        .expect("#1334 oracle cases array")
        .iter()
        .find(|case| case["name"].as_str() == Some(name))
        .cloned()
        .unwrap_or_else(|| panic!("#1334 oracle case {name} is missing"))
}

/// The generator's gateway mock: every `submitTurn` records its input and
/// replays one scripted event list; the string `"<throw>"` rejects the
/// iterator with `Error: gateway exploded`.
struct MockGateway {
    calls: Arc<Mutex<Vec<Value>>>,
    scripts: Mutex<VecDeque<Value>>,
}

impl MockGateway {
    fn from_scripts(scripts: &[Value]) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            scripts: Mutex::new(scripts.iter().cloned().collect()),
        }
    }

    fn calls_snapshot(&self) -> Vec<Value> {
        self.calls.lock().expect("#1334 calls mutex").clone()
    }
}

impl ChannelGateway for MockGateway {
    fn submit_turn(&self, input: ChannelTurn) -> TurnEvents {
        self.calls.lock().expect("#1334 calls mutex").push(json!({
            "sessionKey": String::from_utf16_lossy(input.session_key.units()),
            "channelKey": input.channel_key.to_string(),
            "message": String::from_utf16_lossy(input.message.units()),
        }));
        let script = self
            .scripts
            .lock()
            .expect("#1334 scripts mutex")
            .pop_front()
            .unwrap_or_default();
        let (sender, receiver) = mpsc::unbounded_channel();
        for step in script.as_array().cloned().unwrap_or_default() {
            if step.as_str() == Some("<throw>") {
                let _ = sender.send(Err(ChannelError::error("gateway exploded")));
            } else {
                let event =
                    parse_json(&step.to_string()).expect("#1334 scripted gateway event JSON");
                let _ = sender.send(Ok(event));
            }
        }
        TurnEvents::from(receiver)
    }
}

/// A gateway whose event streams never end: the turn under test stays in
/// flight until the test aborts it.
struct HeldGateway {
    calls: Arc<Mutex<Vec<String>>>,
    held: Mutex<Vec<mpsc::UnboundedSender<TurnEvent>>>,
}

impl ChannelGateway for HeldGateway {
    fn submit_turn(&self, input: ChannelTurn) -> TurnEvents {
        self.calls
            .lock()
            .expect("#1334 held gateway calls mutex")
            .push(String::from_utf16_lossy(input.session_key.units()));
        let (sender, receiver) = mpsc::unbounded_channel();
        self.held
            .lock()
            .expect("#1334 held gateway mutex")
            .push(sender);
        TurnEvents::from(receiver)
    }
}

/// A gateway that records the turn and withholds its single reply event
/// behind a watch gate, so a stop can race a genuinely in-flight turn.
struct GatedGateway {
    calls: Arc<Mutex<Vec<String>>>,
    releases: Mutex<Vec<watch::Sender<bool>>>,
}

impl ChannelGateway for GatedGateway {
    fn submit_turn(&self, input: ChannelTurn) -> TurnEvents {
        self.calls
            .lock()
            .expect("#1334 gated calls mutex")
            .push(String::from_utf16_lossy(input.session_key.units()));
        let (sender, receiver) = mpsc::unbounded_channel();
        let (gate, _) = watch::channel(false);
        self.releases
            .lock()
            .expect("#1334 releases mutex")
            .push(gate.clone());
        tokio::spawn(async move {
            released(gate).await;
            let event = parse_json(r#"{"type":"assistant_text_delta","text":"first reply"}"#)
                .expect("#1334 scripted gateway event JSON");
            // The sender drops after the event: the turn completes on its reply.
            let _ = sender.send(Ok(event));
        });
        TurnEvents::from(receiver)
    }
}

/// The direct tests' channel log capture.
#[derive(Default)]
struct Recorder {
    logs: Arc<Mutex<Vec<(String, String)>>>,
}

impl Recorder {
    fn logger(&self) -> ChannelLog {
        let logs = self.logs.clone();
        Arc::new(move |level: &str, text: &str| {
            logs.lock()
                .expect("#1334 recorder mutex")
                .push((level.to_owned(), text.to_owned()));
        })
    }

    fn has(&self, needle: &str) -> bool {
        self.logs
            .lock()
            .expect("#1334 recorder mutex")
            .iter()
            .any(|(_, text)| text.contains(needle))
    }
}

async fn released(gate: watch::Sender<bool>) {
    let mut receiver = gate.subscribe();
    while !*receiver.borrow_and_update() {
        receiver
            .changed()
            .await
            .expect("#1334 gate sender retained");
    }
}

async fn until(predicate: impl Fn() -> bool) {
    timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1334 bounded wait for the channel state")
}

fn insecure_routes() -> JsObject {
    let mut route = JsObject::new();
    route.insert("secret".into(), JsValue::String(INSECURE_NO_AUTH.into()));
    let mut routes = JsObject::new();
    routes.insert("main".into(), JsValue::Object(route));
    routes
}

fn test_options() -> WebhookChannelOptions {
    WebhookChannelOptions {
        port: Some(0.0),
        routes: Some(insecure_routes()),
        ..Default::default()
    }
}

/// Install a gateway and logger without a listener, the state `start` would
/// have set, for the direct `handle_incoming`/cron paths.
fn install(channel: &WebhookChannel, gateway: Option<Arc<dyn ChannelGateway>>, logger: ChannelLog) {
    let mut state = channel.ctx.state.lock().expect("#1334 webhook state mutex");
    state.gateway = gateway;
    state.logger = Some(logger);
}

/// The recorder's `requestBytes`: Host first, the step's headers in order,
/// then `Connection: close` for close mode, then the body's byte length.
fn request_bytes(step: &Value) -> String {
    let mut lines = vec![format!(
        "{} {} HTTP/1.1",
        step["method"].as_str().unwrap_or_default(),
        step["target"].as_str().unwrap_or_default()
    )];
    lines.push("Host: 127.0.0.1".to_owned());
    let headers = step["headers"].as_object();
    let has = |name: &str| headers.is_some_and(|map| map.contains_key(name));
    if let Some(map) = headers {
        for (name, value) in map {
            lines.push(format!("{name}: {}", value.as_str().unwrap_or_default()));
        }
    }
    let mode = step["mode"].as_str().unwrap_or("close");
    if mode == "close" && !has("Connection") {
        lines.push("Connection: close".to_owned());
    }
    let body = step.get("body").and_then(Value::as_str);
    if body.is_some() && !has("Content-Length") {
        lines.push(format!(
            "Content-Length: {}",
            body.unwrap_or_default().len()
        ));
    }
    format!("{}\r\n\r\n{}", lines.join("\r\n"), body.unwrap_or_default())
}

/// One direct-test delivery: the same shape the recorder's `post()` built.
fn post_body(delivery_id: Option<&str>, body: &str) -> String {
    let mut lines = vec![
        "POST /webhooks/main HTTP/1.1".to_owned(),
        "Host: 127.0.0.1".to_owned(),
    ];
    if let Some(id) = delivery_id {
        lines.push(format!("x-delivery-id: {id}"));
    }
    lines.push("Connection: close".to_owned());
    lines.push(format!("Content-Length: {}", body.len()));
    format!("{}\r\n\r\n{}", lines.join("\r\n"), body)
}

fn latin1_bytes(text: &str) -> Vec<u8> {
    text.chars()
        .map(|character| {
            u8::try_from(u32::from(character))
                .unwrap_or_else(|_| panic!("#1334 oracle char {character:?} is outside latin1"))
        })
        .collect()
}

/// Index just past a complete response in `buffer`, like the recorder's
/// `responseEnd` (including its lowercase last `0\r\n\r\n` chunk search).
fn response_end(buffer: &str) -> Option<usize> {
    let head = buffer.find("\r\n\r\n")?;
    let head_text = &buffer[..head];
    if chunked_head(head_text) {
        let lower = buffer.to_ascii_lowercase();
        return lower.rfind("0\r\n\r\n").map(|at| at + 5);
    }
    if let Some(length) = content_length(head_text) {
        let end = head + 4 + length;
        return (buffer.len() >= end).then_some(end);
    }
    Some(head + 4)
}

fn chunked_head(head_text: &str) -> bool {
    let lower = head_text.to_ascii_lowercase();
    let Some(at) = lower.find("transfer-encoding:") else {
        return false;
    };
    lower[at + "transfer-encoding:".len()..]
        .trim_start_matches([' ', '\t', '\r', '\n', '\x0b', '\x0c'])
        .starts_with("chunked")
}

fn content_length(head_text: &str) -> Option<usize> {
    let lower = head_text.to_ascii_lowercase();
    let at = lower.find("content-length:")?;
    let rest = lower[at + "content-length:".len()..].trim_start_matches([' ', '\t']);
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<usize>().ok()
}

#[derive(Debug)]
enum Exchange {
    Error(String),
    NoResponse,
    Raws(Vec<String>),
}

fn finish_raws(mut raws: Vec<String>, buffer: String) -> Vec<String> {
    if !buffer.is_empty() {
        raws.push(buffer);
    }
    raws
}

fn timer_finish(raws: Vec<String>, buffer: String) -> Exchange {
    if buffer.is_empty() && raws.is_empty() {
        Exchange::NoResponse
    } else {
        Exchange::Raws(finish_raws(raws, buffer))
    }
}

fn error_code(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => "ECONNREFUSED".to_owned(),
        std::io::ErrorKind::ConnectionReset => "ECONNRESET".to_owned(),
        std::io::ErrorKind::TimedOut => "ETIMEDOUT".to_owned(),
        _ => error.to_string(),
    }
}

/// The recorder's `rawExchange`: a latin1 socket, one write per keep-alive
/// response, ended by the response count, EOF, a socket error or the mode's
/// own deadline (the 400 ms silence probe of the null-body cell).
async fn raw_exchange(port: Option<u16>, request: &str, mode: &str, count: usize) -> Exchange {
    let Some(port) = port else {
        return Exchange::Error("no-listener".to_owned());
    };
    let mut stream = match timeout(DEADLINE, TcpStream::connect(("127.0.0.1", port))).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return Exchange::Error(error_code(&error)),
        Err(_) => return Exchange::Error("ETIMEDOUT".to_owned()),
    };
    stream
        .write_all(&latin1_bytes(request))
        .await
        .expect("#1334 write request bytes");
    let budget = if mode == "none" { PROBE } else { DEADLINE };
    let start = Instant::now();
    let mut buffer = String::new();
    let mut raws: Vec<String> = Vec::new();
    let mut sent = 1usize;
    let mut chunk = [0u8; 8192];
    loop {
        let Some(wait) = budget.checked_sub(start.elapsed()) else {
            return timer_finish(raws, buffer);
        };
        match timeout(wait, stream.read(&mut chunk)).await {
            Err(_elapsed) => return timer_finish(raws, buffer),
            Ok(Ok(0)) => {
                if buffer.is_empty() && raws.is_empty() {
                    return Exchange::NoResponse;
                }
                return Exchange::Raws(finish_raws(raws, buffer));
            }
            Ok(Ok(read)) => {
                buffer.extend(chunk[..read].iter().map(|&byte| byte as char));
                if mode == "keepalive" && response_end(&buffer).is_some() {
                    raws.push(std::mem::take(&mut buffer));
                    if sent < count {
                        sent += 1;
                        stream
                            .write_all(&latin1_bytes(request))
                            .await
                            .expect("#1334 rewrite request bytes");
                    } else {
                        return Exchange::Raws(raws);
                    }
                }
            }
            Ok(Err(error)) => return Exchange::Error(error_code(&error)),
        }
    }
}

/// The recorder's `normalize`: status line verbatim, lowercased header
/// sequence with `date` masked, hyper's bare-400 `content-length: 0`
/// dropped (A8.4-NATIVE-HTTP-001) and chunk-size lines case-folded.
fn normalize(raw: &str) -> Value {
    let Some(split) = raw.find("\r\n\r\n") else {
        return json!({"status": null, "headers": [], "body": ""});
    };
    let head = &raw[..split];
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let mut headers: Vec<String> = Vec::new();
    for line in lines {
        // A header line without a colon keeps the recorder's slice bounds.
        let (name, value) = match line.find(':') {
            Some(at) => (&line[..at], &line[at + 1..]),
            None => (line.strip_suffix(|_: char| true).unwrap_or(line), line),
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        let entry = format!("{name}: {}", if name == "date" { "<date>" } else { value });
        if entry != "content-length: 0" {
            headers.push(entry);
        }
    }
    let body = if chunked_head(head) {
        lowercase_chunk_sizes(body)
    } else {
        body.to_owned()
    };
    json!({"status": status, "headers": headers, "body": body})
}

/// `body.replace(/[0-9a-fA-F]+\r\n/g, m => m.toLowerCase())`: a run of hex
/// digits is folded only when the `\r\n` follows it immediately.
fn lowercase_chunk_sizes(body: &str) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut at = 0;
    while at < chars.len() {
        if chars[at].is_ascii_hexdigit() {
            let mut end = at;
            while end < chars.len() && chars[end].is_ascii_hexdigit() {
                end += 1;
            }
            if end + 2 <= chars.len() && chars[end] == '\r' && chars[end + 1] == '\n' {
                for unit in &chars[at..end] {
                    out.push(unit.to_ascii_lowercase());
                }
                out.push_str("\r\n");
                at = end + 2;
            } else {
                out.extend(&chars[at..end]);
                at = end;
            }
        } else {
            out.push(chars[at]);
            at += 1;
        }
    }
    out
}

/// The recorder's `pin`: byte-exact except the masked `Date` line.
fn pin(raw: &str) -> String {
    raw.split("\r\n")
        .map(|line| {
            if line
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("date:"))
            {
                "Date: <date>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// The recorder's `runRequests`: one oracle response entry per exchange
/// outcome, keep-alive counts included.
async fn run_request(port: &Option<u16>, step: &Value, responses: &mut Vec<Value>) {
    let label = step["label"].as_str().expect("#1334 oracle step label");
    let mode = step["mode"].as_str().unwrap_or("close");
    let count = if mode == "keepalive" {
        step["count"].as_u64().unwrap_or(1) as usize
    } else {
        1
    };
    let exchange = raw_exchange(*port, &request_bytes(step), mode, count).await;
    match exchange {
        Exchange::Error(code) => responses.push(json!({"label": label, "error": code})),
        Exchange::NoResponse => responses.push(json!({"label": label, "noResponse": true})),
        Exchange::Raws(raws) => {
            if count == 1 {
                let one = raws.into_iter().next().unwrap_or_default();
                let mut entry = normalize(&one);
                entry["label"] = json!(label);
                responses.push(entry);
                if step["pin"].as_bool() == Some(true) {
                    responses.push(json!({"label": format!("{label}-pin"), "raw": pin(&one)}));
                }
            } else {
                for (index, one) in raws.iter().enumerate() {
                    let mut entry = normalize(one);
                    entry["label"] = json!(format!("{label}-{}", index + 1));
                    responses.push(entry);
                }
            }
        }
    }
}

/// The recorder's failed-start mask: the bind error tail carries errno
/// text that differs between Node and Rust.
fn mask_failed_start(text: &str) -> String {
    if text.starts_with("webhook: failed to start: ") {
        "webhook: failed to start: <masked>".to_owned()
    } else {
        text.to_owned()
    }
}

fn delivery_from_step(step: &Value) -> CronResultDelivery {
    CronResultDelivery {
        task_id: JsString::from("t"),
        run_id: JsString::from("r"),
        session_key: JsString::from(step["sessionKey"].as_str().unwrap_or_default()),
        channel_key: ChannelKey::new("webhook"),
        origin_session_key: step["originSessionKey"].as_str().map(JsString::from),
        origin_channel_key: step["originChannelKey"].as_str().map(ChannelKey::new),
        text: JsString::from(step["text"].as_str().unwrap_or("cron text")),
    }
}

/// One oracle scenario through the native channel: build the options the
/// generator built (the `<busy-port>` sentinel gets its own blocker), drive
/// the step DSL against the real listener, then capture after the same
/// quiet wait the recorder used.
async fn run_case(case: &Value) -> Value {
    let options = &case["options"];
    let mut blocker = None;
    let port_option = match options["port"].as_str() {
        Some("<busy-port>") => {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")
                .expect("#1334 bind the busy-port blocker");
            let port = listener
                .local_addr()
                .expect("#1334 blocker local address")
                .port();
            blocker = Some(listener);
            Some(port as f64)
        }
        _ => options["port"].as_f64(),
    };
    let routes = parse_json(&options["routes"].to_string())
        .expect("#1334 oracle routes JSON")
        .as_object()
        .expect("#1334 oracle routes object")
        .clone();
    let gateway = Arc::new(MockGateway::from_scripts(
        &case["scripts"].as_array().cloned().unwrap_or_default(),
    ));
    let logs: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let cron: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let logger: ChannelLog = {
        let logs = logs.clone();
        Arc::new(move |level: &str, text: &str| {
            logs.lock()
                .expect("#1334 replay logs mutex")
                .push(json!([level, text]));
        })
    };
    let channel = WebhookChannel::new(WebhookChannelOptions {
        port: port_option,
        host: None,
        secret: options
            .get("secret")
            .and_then(Value::as_str)
            .map(str::to_owned),
        routes: Some(routes),
        rate_limit: options.get("rateLimit").and_then(Value::as_f64),
        max_body_bytes: options.get("maxBodyBytes").and_then(Value::as_f64),
        mapper: (options["mapperFixedUuid"].as_bool() == Some(true))
            .then(|| WebhookSessionMapper::new(Arc::new(|| JsString::from(FIXED_UUID)))),
        now: None,
        request_timeout: None,
    });
    let deps = ChannelStartDeps {
        config: None,
        report_status: None,
        log: Some(logger),
        gateway: (case["name"].as_str() != Some("gateway-missing"))
            .then(|| gateway.clone() as Arc<dyn ChannelGateway>),
    };
    channel
        .start(deps.clone())
        .await
        .expect("#1334 channel start");
    let mut port = channel.bound_addr().map(|address| address.port());
    let mut responses: Vec<Value> = Vec::new();
    for step in case["steps"].as_array().cloned().unwrap_or_default() {
        match step["op"].as_str().unwrap_or_default() {
            "request" => run_request(&port, &step, &mut responses).await,
            "stop" => {
                channel
                    .stop_with_reason(step["reason"].as_str())
                    .await
                    .expect("#1334 oracle stop step");
            }
            "start" => {
                channel
                    .start(deps.clone())
                    .await
                    .expect("#1334 oracle restart step");
                port = channel.bound_addr().map(|address| address.port());
            }
            "cron" => {
                let taken = channel
                    .deliver_cron_result(&delivery_from_step(&step))
                    .await;
                cron.lock().expect("#1334 replay cron mutex").push(taken);
            }
            other => panic!("#1334 unknown oracle step op {other:?}"),
        }
    }
    // handleIncoming is fire-and-forget: wait for the log/gateway/cron
    // streams to go quiet, two consecutive stable polls like the recorder.
    let mut last: i64 = -1;
    for _ in 0..POLLS {
        let count = (logs.lock().expect("#1334 replay logs mutex").len()
            + gateway.calls_snapshot().len()
            + cron.lock().expect("#1334 replay cron mutex").len()) as i64;
        if count == last {
            break;
        }
        last = count;
        tokio::time::sleep(POLL).await;
    }
    let masked = logs
        .lock()
        .expect("#1334 replay logs mutex")
        .iter()
        .map(|entry| {
            json!([
                entry[0].as_str().unwrap_or_default(),
                mask_failed_start(entry[1].as_str().unwrap_or_default())
            ])
        })
        .collect::<Vec<_>>();
    // The stopping log of the post-capture stop is intentionally not part of
    // the oracle, like the recorder's trailing `handle.stop()`.
    let _ = channel.stop_with_reason(None).await;
    drop(blocker);
    json!({
        "responses": responses,
        "gateway": gateway.calls_snapshot(),
        "logs": masked,
        "cron": *cron.lock().expect("#1334 replay cron mutex"),
    })
}

async fn replay(name: &str) {
    let case = case(name);
    let actual = run_case(&case).await;
    negative_assert_eq!(
        actual,
        case["expected"].clone(),
        "#1334 webhook oracle case {}",
        name
    );
}

macro_rules! replay_tests {
    ($($function:ident => $name:literal),* $(,)?) => {
        /// Every case name the macro instantiates; a test asserts this is
        /// exactly the oracle's case set, so neither list can drift alone.
        const REPLAY_CASE_NAMES: &[&str] = &[$($name),*];
        $(
            #[tokio::test]
            async fn $function() {
                replay($name).await;
            }
        )*
    };
}

replay_tests! {
    health_and_routing => "health-and-routing",
    unknown_route => "unknown-route",
    rate_limit => "rate-limit",
    too_large => "too-large",
    invalid_signature => "invalid-signature",
    insecure_no_auth => "insecure-no-auth",
    duplicate_delivery => "duplicate-delivery",
    no_text => "no-text",
    extract_github => "extract-github",
    submit_and_render => "submit-and-render",
    deliver_other => "deliver-other",
    submit_error => "submit-error",
    new_command => "new-command",
    gateway_missing => "gateway-missing",
    no_secret_route => "no-secret-route",
    port_in_use => "port-in-use",
    invalid_port => "invalid-port",
    cron_delivery => "cron-delivery",
    stop_and_restart => "stop-and-restart",
}

/// The replay list and the oracle describe the same case set; a scenario
/// added to the oracle without a replay test (or vice versa) fails here.
#[test]
fn replay_cases_cover_exactly_the_oracle() {
    let recorded = oracle();
    let cases = recorded["cases"]
        .as_array()
        .expect("#1334 oracle cases array");
    negative_assert_eq!(
        recorded["expectedCaseCount"].as_u64(),
        Some(19),
        "#1334 oracle declares nineteen cases"
    );
    negative_assert_eq!(cases.len(), 19, "#1334 oracle carries nineteen cases");
    negative_assert_eq!(
        recorded["node"].as_str(),
        Some("v22.22.0"),
        "#1334 oracle node baseline"
    );
    negative_assert_eq!(
        recorded["icu"].as_str(),
        Some("78.2"),
        "#1334 oracle ICU baseline"
    );
    let mut names: Vec<&str> = cases
        .iter()
        .map(|case| case["name"].as_str().expect("#1334 oracle case name"))
        .collect();
    names.sort_unstable();
    let mut replayed: Vec<&str> = REPLAY_CASE_NAMES.to_vec();
    replayed.sort_unstable();
    negative_assert_eq!(
        replayed,
        names,
        "#1334 replay case names and oracle case names are the same set"
    );
}

/// The fixture must regenerate byte-identically on a host with the fixed
/// baseline; the authoring session has no such host.
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn webhook_channel_regenerates_identically() {
    crate::test_baseline::regenerates_identically("webhook-channel");
}

#[test]
fn extract_text_field_order_is_text_first() {
    let body = parse_json(r#"{"text":"a","message":"b"}"#).expect("#1334 body JSON");
    negative_assert_eq!(
        extract_text(&body).as_deref(),
        Some("a"),
        "#1334 extractText prefers text over message"
    );
}

#[test]
fn prune_keeps_the_exact_cutoff() {
    let mut state = State::default();
    state.seen_deliveries.insert("before".to_owned(), 6399.0);
    state.seen_deliveries.insert("at".to_owned(), 6400.0);
    state.seen_deliveries.insert("after".to_owned(), 6400.5);
    prune_seen_deliveries(&mut state, 10000.0);
    negative_assert!(
        state.seen_deliveries.contains_key("at"),
        "#1334 a delivery seen exactly at the cutoff survives pruning"
    );
    negative_assert!(
        !state.seen_deliveries.contains_key("before"),
        "#1334 a delivery seen before the cutoff is pruned"
    );
    negative_assert!(
        state.seen_deliveries.contains_key("after"),
        "#1334 a delivery seen after the cutoff survives pruning"
    );
}

fn rate_ctx(limit: f64) -> Arc<Ctx> {
    Arc::new(Ctx {
        key: ChannelKey::new("webhook"),
        host: "127.0.0.1".to_owned(),
        port: 0.0,
        routes: JsObject::new(),
        global_secret: String::new(),
        rate_limit: limit,
        max_body_bytes: 1024.0,
        request_timeout: DEFAULT_REQUEST_TIMEOUT,
        now: Arc::new(|| 0.0),
        mapper: Mutex::new(WebhookSessionMapper::new(
            WebhookSessionMapper::random_uuid(),
        )),
        state: Mutex::new(State::default()),
    })
}

#[test]
fn rate_limit_window_slides_at_sixty_seconds() {
    let ctx = rate_ctx(2.0);
    negative_assert!(
        check_rate_limit(&ctx, "main", 100.0),
        "#1334 the first request is admitted"
    );
    negative_assert!(
        check_rate_limit(&ctx, "main", 100.0),
        "#1334 the second request is admitted"
    );
    negative_assert!(
        !check_rate_limit(&ctx, "main", 159.9),
        "#1334 the window has not slid at 59.9 s"
    );
    negative_assert!(
        check_rate_limit(&ctx, "main", 160.0),
        "#1334 the rate limit window slides at exactly 60 s"
    );
}

#[tokio::test]
async fn missing_delivery_id_falls_back_to_route_and_now() {
    let gateway = Arc::new(MockGateway::from_scripts(&[]));
    let recorder = Recorder::default();
    let channel = WebhookChannel::new(WebhookChannelOptions {
        port: Some(0.0),
        routes: Some(insecure_routes()),
        now: Some(Arc::new(|| 12345.0)),
        ..Default::default()
    });
    channel
        .start(ChannelStartDeps {
            log: Some(recorder.logger()),
            gateway: Some(gateway.clone() as Arc<dyn ChannelGateway>),
            ..Default::default()
        })
        .await
        .expect("#1334 start for the delivery-id fallback test");
    let port = channel.bound_addr().expect("#1334 bound listener").port();
    let _ = raw_exchange(
        Some(port),
        &post_body(None, r#"{"message":"hi"}"#),
        "close",
        1,
    )
    .await;
    until(|| !gateway.calls_snapshot().is_empty()).await;
    let calls = gateway.calls_snapshot();
    negative_assert_eq!(
        calls.first().and_then(|call| call["sessionKey"].as_str()),
        Some("webhook:chat=webhook:main:main-12345:general"),
        "#1334 the delivery id falls back to route and now"
    );
    timeout(DEADLINE, channel.stop_with_reason(None))
        .await
        .expect("#1334 the fallback test stops its channel")
        .expect("#1334 stop ok");
}

#[tokio::test]
async fn handle_incoming_reentry_skips_active_chat() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let gateway = Arc::new(HeldGateway {
        calls: calls.clone(),
        held: Mutex::new(Vec::new()),
    });
    let recorder = Recorder::default();
    let channel = WebhookChannel::new(test_options());
    install(&channel, Some(gateway), recorder.logger());
    let ctx = channel.ctx.clone();
    let first = {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _ = handle_incoming(&ctx, "webhook:main:chat", "one".to_owned()).await;
        })
    };
    let second = {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _ = handle_incoming(&ctx, "webhook:main:other", "other".to_owned()).await;
        })
    };
    until(|| calls.lock().expect("#1334 held calls mutex").len() >= 2).await;
    handle_incoming(&ctx, "webhook:main:chat", "two".to_owned())
        .await
        .expect("#1334 reentry returns Ok");
    negative_assert!(
        recorder.has("webhook: chat webhook:main:chat already active, skipping"),
        "#1334 an active chat is not re-entered"
    );
    negative_assert_eq!(
        calls.lock().expect("#1334 held calls mutex").len(),
        2,
        "#1334 a different chat proceeds concurrently"
    );
    first.abort();
    second.abort();
}

#[tokio::test]
async fn stop_does_not_cancel_an_in_flight_turn() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let gateway = Arc::new(GatedGateway {
        calls: calls.clone(),
        releases: Mutex::new(Vec::new()),
    });
    let recorder = Recorder::default();
    let channel = WebhookChannel::new(test_options());
    channel
        .start(ChannelStartDeps {
            log: Some(recorder.logger()),
            gateway: Some(gateway.clone() as Arc<dyn ChannelGateway>),
            ..Default::default()
        })
        .await
        .expect("#1334 start for the in-flight stop test");
    let port = channel.bound_addr().expect("#1334 bound listener").port();
    match raw_exchange(
        Some(port),
        &post_body(Some("d1"), r#"{"message":"hi"}"#),
        "close",
        1,
    )
    .await
    {
        Exchange::Raws(raws) => negative_assert!(
            raws.first().is_some_and(|raw| raw.contains("\"accepted\"")),
            "#1334 the in-flight turn's delivery is accepted"
        ),
        other => panic!("#1334 unexpected exchange {other:?}"),
    }
    until(|| !calls.lock().expect("#1334 gated calls mutex").is_empty()).await;
    timeout(DEADLINE, channel.stop_with_reason(Some("drain")))
        .await
        .expect("#1334 stop resolves while a turn is in flight")
        .expect("#1334 stop ok");
    negative_assert!(
        !recorder.has("webhook: response for webhook:main:d1: first reply"),
        "#1334 the withheld reply has not been delivered at the stop"
    );
    let releases = gateway
        .releases
        .lock()
        .expect("#1334 releases mutex")
        .clone();
    releases
        .first()
        .expect("#1334 one gated turn")
        .send(true)
        .expect("#1334 gate receiver retained");
    until(|| recorder.has("webhook: response for webhook:main:d1: first reply")).await;
    negative_assert!(
        recorder.has("webhook: response for webhook:main:d1: first reply"),
        "#1334 an in-flight turn delivers its reply after stop"
    );
}

/// #1334 fix-r1 S1: a partial body the client never completes is bounded by
/// the body-read deadline (Node's default `requestTimeout`, probe-shortened
/// here) — the connection is answered and closed, so a concurrent `stop()`
/// resolves instead of waiting forever on the in-flight connection.
#[tokio::test]
async fn shutdown_with_partial_body_finishes_at_request_timeout() {
    let gateway = Arc::new(MockGateway::from_scripts(&[]));
    let recorder = Recorder::default();
    let channel = WebhookChannel::new(WebhookChannelOptions {
        port: Some(0.0),
        routes: Some(insecure_routes()),
        request_timeout: Some(Duration::from_millis(200)),
        ..Default::default()
    });
    channel
        .start(ChannelStartDeps {
            log: Some(recorder.logger()),
            gateway: Some(gateway.clone() as Arc<dyn ChannelGateway>),
            ..Default::default()
        })
        .await
        .expect("#1334 start for the partial-body requestTimeout test");
    let port = channel
        .bound_addr()
        .expect("#1334 bound listener for the partial-body test")
        .port();
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1334 connect to the partial-body listener");
    stream
        .write_all(
            b"POST /webhooks/main HTTP/1.1\r\nHost: 127.0.0.1\r\n\
             Content-Length: 100\r\n\r\nhalf-body",
        )
        .await
        .expect("#1334 send the partial body");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let stopping = tokio::spawn(async move { channel.stop_with_reason(Some("drain")).await });
    let mut response = Vec::new();
    timeout(DEADLINE, stream.read_to_end(&mut response))
        .await
        .expect("#1334 the requestTimeout closes the half-open body during stop")
        .expect("#1334 read the partial-body response bytes");
    negative_assert_eq!(
        response.as_slice(),
        WEBHOOK_REQUEST_TIMEOUT_RESPONSE,
        "#1334 the body requestTimeout answers Node's 408 in hyper's chunked framing and closes"
    );
    timeout(DEADLINE, stopping)
        .await
        .expect("#1334 stop finishes after the body requestTimeout")
        .expect("#1334 stop poll")
        .expect("#1334 stop ok after the body requestTimeout");
}

#[tokio::test]
async fn double_stop_resolves_without_a_listener() {
    let channel = WebhookChannel::new(test_options());
    timeout(DEADLINE, channel.stop_with_reason(Some("first")))
        .await
        .expect("#1334 the first stop without a listener resolves")
        .expect("#1334 first stop ok");
    timeout(DEADLINE, channel.stop_with_reason(None))
        .await
        .expect("#1334 the second stop without a listener resolves")
        .expect("#1334 second stop ok");
}

#[tokio::test]
async fn replacement_channel_rebinds_the_released_port() {
    let first_gateway = Arc::new(MockGateway::from_scripts(&[]));
    let first_recorder = Recorder::default();
    let first = WebhookChannel::new(test_options());
    first
        .start(ChannelStartDeps {
            log: Some(first_recorder.logger()),
            gateway: Some(first_gateway.clone() as Arc<dyn ChannelGateway>),
            ..Default::default()
        })
        .await
        .expect("#1334 first start");
    let port = first
        .bound_addr()
        .expect("#1334 first bound listener")
        .port();
    match raw_exchange(
        Some(port),
        &post_body(Some("d1"), r#"{"message":"hi"}"#),
        "close",
        1,
    )
    .await
    {
        Exchange::Raws(raws) => negative_assert!(
            raws.first().is_some_and(|raw| raw.contains("\"accepted\"")),
            "#1334 the first channel accepts the delivery"
        ),
        other => panic!("#1334 unexpected exchange {other:?}"),
    }
    until(|| !first_gateway.calls_snapshot().is_empty()).await;
    timeout(DEADLINE, first.stop_with_reason(Some("replace")))
        .await
        .expect("#1334 the replaced channel stops")
        .expect("#1334 stop ok");
    let second_gateway = Arc::new(MockGateway::from_scripts(&[]));
    let second_recorder = Recorder::default();
    let second = WebhookChannel::new(WebhookChannelOptions {
        port: Some(port as f64),
        routes: Some(insecure_routes()),
        ..Default::default()
    });
    second
        .start(ChannelStartDeps {
            log: Some(second_recorder.logger()),
            gateway: Some(second_gateway.clone() as Arc<dyn ChannelGateway>),
            ..Default::default()
        })
        .await
        .expect("#1334 replacement start");
    negative_assert_eq!(
        second.bound_addr().map(|address| address.port()),
        Some(port),
        "#1334 the replacement rebinds the released port"
    );
    match raw_exchange(
        Some(port),
        &post_body(Some("d1"), r#"{"message":"hi"}"#),
        "close",
        1,
    )
    .await
    {
        Exchange::Raws(raws) => negative_assert!(
            raws.first().is_some_and(|raw| raw.contains("\"accepted\"")),
            "#1334 a replacement channel rebinds the released port and admits the same delivery id"
        ),
        other => panic!("#1334 unexpected exchange {other:?}"),
    }
    until(|| !second_gateway.calls_snapshot().is_empty()).await;
    timeout(DEADLINE, second.stop_with_reason(None))
        .await
        .expect("#1334 the replacement stops")
        .expect("#1334 stop ok");
}

#[tokio::test]
async fn new_command_creates_a_session_and_maps_the_followup() {
    let gateway = Arc::new(MockGateway::from_scripts(&[]));
    let recorder = Recorder::default();
    let channel = WebhookChannel::new(WebhookChannelOptions {
        port: Some(0.0),
        routes: Some(insecure_routes()),
        mapper: Some(WebhookSessionMapper::new(Arc::new(|| {
            JsString::from(FIXED_UUID)
        }))),
        ..Default::default()
    });
    install(&channel, Some(gateway.clone()), recorder.logger());
    let ctx = channel.ctx.clone();
    handle_incoming(&ctx, "webhook:main:c1", "/new".to_owned())
        .await
        .expect("#1334 /new turn ok");
    negative_assert!(
        recorder.has("webhook: response for webhook:main:c1: 已创建新会话。"),
        "#1334 /new replies with the new-session message"
    );
    handle_incoming(&ctx, "webhook:main:c1", "hello".to_owned())
        .await
        .expect("#1334 follow-up turn ok");
    let calls = gateway.calls_snapshot();
    negative_assert_eq!(
        calls.first().and_then(|call| call["sessionKey"].as_str()),
        Some("webhook:chat=webhook:main:c1:s_00000000-0000-4000-8000-000000000000"),
        "#1334 the mapped session key follows the /new assignment"
    );
}

#[tokio::test]
async fn cron_delivery_resolves_session_suffix_chat_ids() {
    let channel = WebhookChannel::new(test_options());
    let delivery = CronResultDelivery {
        task_id: JsString::from("t"),
        run_id: JsString::from("r"),
        session_key: JsString::from(
            "webhook:chat=webhook:main:d1:s_0123456789abcdef0123456789abcdef0123",
        ),
        channel_key: ChannelKey::new("webhook"),
        origin_session_key: None,
        origin_channel_key: None,
        text: JsString::from("cron reply"),
    };
    let taken = channel.deliver_cron_result(&delivery).await;
    negative_assert!(taken, "#1334 an s_ session key is a cron target");
}
