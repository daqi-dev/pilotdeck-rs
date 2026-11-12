//! #1328 oracle replay: the 15 committed TS scenarios (`weixin-channel`
//! `generate.mts` running the real baseline stack against a scripted loopback
//! ilink mock) re-run against the Rust assembly — `NativeChannels` + the
//! `WeixinChannel` on the #1327 lifecycle core — through the same step
//! language, and compared cell-for-cell with the recorded expectation. The
//! harness mirrors the generator's own (its `FakeIlink` hold-and-release
//! long-poll semantics, its `HarnessGateway` turn scripts, and its golden
//! masking); every difference between the TS baseline and this port shows up
//! as a failing cell, not a changed harness.
use super::{
    IlinkHttpClient, SavedCredentials, WeixinClientError, WeixinIlinkClient, WeixinStreamItem,
    WeixinTurnStream, login_with_qr,
};
use crate::channel::{
    protocol::BoxFuture,
    runtime_status::{ChannelRuntimeStatusUpdate, create_channel_runtime_status_reporter},
};
use crate::native_process::channels::{NativeChannels, NativeChannelsOptions};
use http_body_util::BodyExt;
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot};

const ORACLE: &str = include_str!("../../../tests/fixtures/weixin-channel/oracle.json");
/// `generate.mts` records only these request headers, in this order.
const HEADER_KEYS: [&str; 5] = [
    "content-type",
    "authorization",
    "authorizationtype",
    "ilink-app-clientversion",
    "x-wechat-uin",
];

fn oracle(raw: &str) -> Value {
    crate::test_baseline::oracle(raw);
    serde_json::from_str(raw).expect("#1328 weixin oracle JSON")
}

// ── golden masking (generate.mts makeMask, same rules on both sides) ────────

struct Mask {
    scenario_dir: String,
    uuid: regex::Regex,
    iso: regex::Regex,
    at_tail: regex::Regex,
    port: regex::Regex,
}
impl Mask {
    fn new(scenario_dir: &std::path::Path) -> Self {
        Self {
            scenario_dir: scenario_dir.to_string_lossy().into_owned(),
            uuid: regex::Regex::new(
                r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
            )
            .expect("#1328 uuid mask regex"),
            iso: regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z")
                .expect("#1328 iso mask regex"),
            // JS `.` never crosses a newline and the pattern has no `g` flag,
            // so only the first line tail is stripped.
            at_tail: regex::Regex::new(r"; at=.*").expect("#1328 stack tail mask regex"),
            port: regex::Regex::new(r"http://127\.0\.0\.1:\d+").expect("#1328 port mask regex"),
        }
    }
    fn string(&self, text: &str) -> String {
        let text = text.replace(&self.scenario_dir, "<tmp>");
        let text = self.uuid.replace_all(&text, "<uuid>").into_owned();
        let text = self.iso.replace_all(&text, "<now>").into_owned();
        let text = self.at_tail.replace(&text, "").into_owned();
        self.port
            .replace_all(&text, "http://127.0.0.1:<port>")
            .into_owned()
    }
    fn value(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.string(text)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| self.value(item)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, item)| {
                        let masked = if key == "client_id" && item.is_string() {
                            Value::String("<client-id>".into())
                        } else {
                            self.value(item)
                        };
                        (key.clone(), masked)
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

/// A `serde_json` view of a `JsValue` (the harness records JSON, not JS
/// objects, exactly like the generator's `mask(...)` snapshots).
fn describe(value: &JsValue) -> Value {
    match value {
        JsValue::Undefined => json!("<undefined>"),
        JsValue::Object(object) => Value::Object(
            object
                .own_entries()
                .into_iter()
                .map(|(key, item)| {
                    (
                        key.to_utf8().expect("#1328 object key UTF-8"),
                        describe(item),
                    )
                })
                .collect(),
        ),
        JsValue::Array(items) => Value::Array(items.iter().map(describe).collect()),
        other => serde_json::from_str(
            &stringify_json(other)
                .expect("#1328 stringify value")
                .expect("#1328 defined value"),
        )
        .expect("#1328 replay value JSON"),
    }
}

fn js_event(value: &Value) -> JsValue {
    parse_json(&serde_json::to_string(value).expect("#1328 event JSON"))
        .expect("#1328 event parses")
}

fn js_text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

// ── the scripted loopback ilink server (generate.mts FakeIlink) ────────────

struct HeldPoll {
    record: Value,
    sender: oneshot::Sender<(u16, Vec<u8>, String)>,
}

/// The generator records raw CDN bodies as hex (`bodyHex`), so the replay
/// decodes them back into the bytes the download must serve.
fn decode_hex(text: &str) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2),
        "#1822 bodyHex has an even length"
    );
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("#1822 bodyHex pair"))
        .collect()
}

#[derive(Default)]
struct FakeShared {
    requests: Mutex<Vec<Value>>,
    scripts: Mutex<HashMap<String, VecDeque<Value>>>,
    held: Mutex<VecDeque<HeldPoll>>,
    last_qr_status: Mutex<Option<Value>>,
}

impl FakeShared {
    fn push_synthetic(&self, name: &str) {
        self.requests
            .lock()
            .expect("#1328 fake requests mutex")
            .push(json!({ "synthetic": name }));
    }
    fn count_path(&self, path: &str) -> usize {
        self.requests
            .lock()
            .expect("#1328 fake requests mutex")
            .iter()
            .filter(|request| {
                request
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|recorded| {
                        recorded == path || recorded.starts_with(&format!("{path}?"))
                    })
            })
            .count()
    }
    /// The response a scripted item produces: status failures keep their text
    /// body, `bodyHex` items serve raw CDN bytes, and everything else is the
    /// item (or the endpoint default) as JSON. The third element is the
    /// response `content-type`.
    fn respond(&self, pathname: &str, item: Option<Value>) -> (u16, Vec<u8>, String) {
        if let Some(status) = item
            .as_ref()
            .and_then(|item| item.get("status"))
            .and_then(Value::as_i64)
        {
            let text = item
                .as_ref()
                .and_then(|item| item.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            return (
                status as u16,
                text.as_bytes().to_vec(),
                "text/plain".to_string(),
            );
        }
        // #1822: the CDN download path answers with raw bytes; `contentType`
        // is what `saveFromUrl` reads for its mime fallback.
        if let Some(hex) = item
            .as_ref()
            .and_then(|item| item.get("bodyHex"))
            .and_then(Value::as_str)
        {
            let content_type = item
                .as_ref()
                .and_then(|item| item.get("contentType"))
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream")
                .to_string();
            return (200, decode_hex(hex), content_type);
        }
        let json = item.unwrap_or_else(|| self.default_for(pathname));
        if pathname == "/ilink/bot/get_qrcode_status" {
            *self.last_qr_status.lock().expect("#1328 qr mutex") = Some(json.clone());
        }
        (
            200,
            serde_json::to_string(&json)
                .expect("#1328 response JSON")
                .into_bytes(),
            "application/json".to_string(),
        )
    }
    fn default_for(&self, pathname: &str) -> Value {
        match pathname {
            "/ilink/bot/getconfig" => json!({"ret": 0, "typing_ticket": "tt-1"}),
            "/ilink/bot/get_bot_qrcode" => {
                json!({"qrcode": "qr-1", "qrcode_img_content": "https://mock.example/qr-1"})
            }
            "/ilink/bot/get_qrcode_status" => self
                .last_qr_status
                .lock()
                .expect("#1328 qr mutex")
                .clone()
                .unwrap_or_else(|| json!({"status": "wait"})),
            _ => json!({"ret": 0}),
        }
    }
    /// A script push answers a parked long poll when its queue is non-empty
    /// (an item pushed with no poll parked stays queued for the next one).
    fn pump(&self) {
        loop {
            let next = {
                let mut scripts = self.scripts.lock().expect("#1328 scripts mutex");
                let mut held = self.held.lock().expect("#1328 held mutex");
                if held.is_empty() {
                    return;
                }
                let Some(item) = scripts
                    .get_mut("/ilink/bot/getupdates")
                    .and_then(|queue| queue.pop_front())
                else {
                    return;
                };
                let poll = held.pop_front().expect("#1328 held poll present");
                self.requests
                    .lock()
                    .expect("#1328 fake requests mutex")
                    .push(poll.record);
                (
                    poll.sender,
                    self.respond("/ilink/bot/getupdates", Some(item)),
                )
            };
            let (sender, response) = next;
            let _ = sender.send(response);
        }
    }
    /// Stopping channels park on held long polls; answering them (FIFO, like
    /// the generator's `splice(0)` loop) with the empty shape lets the poll
    /// loop exit at its top-of-loop check.
    fn release_held(&self) {
        let polls: Vec<HeldPoll> = self
            .held
            .lock()
            .expect("#1328 held mutex")
            .drain(..)
            .collect();
        for poll in polls {
            self.requests
                .lock()
                .expect("#1328 fake requests mutex")
                .push(poll.record.clone());
            let buffer = poll
                .record
                .get("body")
                .and_then(|body| body.get("get_updates_buf"))
                .cloned()
                .unwrap_or_else(|| json!(""));
            let _ = poll.sender.send((
                200,
                format!(r#"{{"ret":0,"msgs":[],"get_updates_buf":{buffer}}}"#).into_bytes(),
                "application/json".to_string(),
            ));
        }
    }
    fn push_script(&self, endpoint: &str, items: &[Value]) {
        let mut scripts = self.scripts.lock().expect("#1328 scripts mutex");
        scripts
            .entry(endpoint.to_string())
            .or_default()
            .extend(items.iter().cloned());
        drop(scripts);
        self.pump();
    }
    async fn handle(
        self: Arc<Self>,
        mask: &Mask,
        request: hyper::Request<hyper::body::Incoming>,
    ) -> Result<hyper::Response<http_body_util::Full<hyper::body::Bytes>>, std::convert::Infallible>
    {
        let method = request.method().as_str().to_string();
        let uri = request.uri().clone();
        let headers: Vec<(String, String)> = HEADER_KEYS
            .iter()
            .filter_map(|key| {
                request.headers().get(*key).map(|value| {
                    (
                        (*key).to_string(),
                        value.to_str().unwrap_or_default().to_string(),
                    )
                })
            })
            .collect();
        let bytes = request
            .into_body()
            .collect()
            .await
            .map(|collected| collected.to_bytes())
            .unwrap_or_default();
        let body_text = if bytes.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&bytes).into_owned())
        };
        let path = format!(
            "{}{}",
            uri.path(),
            uri.query()
                .map(|query| format!("?{query}"))
                .unwrap_or_default()
        );
        let record = json!({
            "method": method,
            "path": mask.string(&path),
            "headers": headers
                .into_iter()
                .map(|(key, value)| {
                    let masked = if key == "x-wechat-uin" {
                        json!("<uin>")
                    } else {
                        json!(mask.string(&value))
                    };
                    (key, masked)
                })
                .collect::<serde_json::Map<String, Value>>(),
            "body": match &body_text {
                None => Value::Null,
                Some(text) => match serde_json::from_str::<Value>(text) {
                    Ok(value) => mask.value(&value),
                    Err(_) => json!({"raw": mask.string(text)}),
                },
            },
        });
        if method == "POST" && uri.path() == "/ilink/bot/getupdates" {
            let (sender, receiver) = oneshot::channel();
            self.held
                .lock()
                .expect("#1328 held mutex")
                .push_back(HeldPoll { record, sender });
            self.pump();
            let (status, body, content_type) = receiver.await.expect("#1328 held poll answered");
            return Ok(build_response(status, &content_type, &body));
        }
        self.requests
            .lock()
            .expect("#1328 fake requests mutex")
            .push(record);
        let item = self
            .scripts
            .lock()
            .expect("#1328 scripts mutex")
            .get_mut(uri.path())
            .and_then(|queue| queue.pop_front());
        let (status, body, content_type) = self.respond(uri.path(), item);
        Ok(build_response(status, &content_type, &body))
    }
}

fn build_response(
    status: u16,
    content_type: &str,
    body: &[u8],
) -> hyper::Response<http_body_util::Full<hyper::body::Bytes>> {
    hyper::Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(http_body_util::Full::new(hyper::body::Bytes::from(
            body.to_vec(),
        )))
        .expect("#1328 static response")
}

struct FakeIlink {
    shared: Arc<FakeShared>,
    task: tokio::task::JoinHandle<()>,
    port: u16,
}
impl FakeIlink {
    /// Listens on an ephemeral loopback port; requests never leave the host.
    async fn start(mask: Arc<Mask>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("#1328 loopback listen");
        let port = listener
            .local_addr()
            .expect("#1328 loopback address")
            .port();
        let shared = Arc::new(FakeShared::default());
        let server = shared.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let shared = server.clone();
                let mask = mask.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(
                        move |request: hyper::Request<hyper::body::Incoming>| {
                            let shared = shared.clone();
                            let mask = mask.clone();
                            async move { shared.handle(&mask, request).await }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self { shared, task, port }
    }
    fn destroy(self) {
        self.shared.release_held();
        self.task.abort();
    }
}

// ── the harness gateway port (generate.mts HarnessGateway) ─────────────────

struct HarnessPort {
    submits: Arc<Mutex<Vec<Value>>>,
    aborts: Arc<Mutex<Vec<Value>>>,
    fake: Arc<FakeShared>,
    mask: Arc<Mask>,
    turns: Vec<Value>,
    turn_index: Arc<AtomicUsize>,
    held_turns: Arc<Mutex<HashMap<usize, oneshot::Sender<Vec<Value>>>>>,
}

/// `until` gates cover real timers (the QR poll interval, retry sleeps, the
/// recovery watcher), so they are time-bounded like the generator's, not
/// iteration-bounded.
async fn until(predicate: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        crate::negative_assert!(
            tokio::time::Instant::now() < deadline,
            "#1328 weixin harness gate did not resolve"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn play_events(
    items: &[Value],
    sender: &tokio::sync::mpsc::UnboundedSender<WeixinStreamItem>,
    fake: &FakeShared,
) {
    for item in items {
        if let Some(gates) = item.get("waitRequests").and_then(Value::as_array) {
            for gate in gates {
                let path = gate["path"].as_str().expect("#1328 gate path");
                let count = gate["count"].as_u64().expect("#1328 gate count") as usize;
                until(|| fake.count_path(path) >= count).await;
            }
        } else {
            let _ = sender.send(WeixinStreamItem::Event(js_event(
                item.get("event").expect("#1328 turn event"),
            )));
        }
    }
}

impl crate::channel::weixin::WeixinGatewayPort for HarnessPort {
    fn submit_turn(&self, input: JsValue) -> WeixinTurnStream {
        self.submits
            .lock()
            .expect("#1328 submits mutex")
            .push(self.mask.value(&describe(&input)));
        let index = self.turn_index.fetch_add(1, Ordering::SeqCst);
        let script = self.turns.get(index).cloned().unwrap_or_else(|| json!({}));
        let events: Vec<Value> = script["events"].as_array().cloned().unwrap_or_default();
        let hold = script["hold"].as_bool().unwrap_or(false);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        // The held-turn marker records when the consumer asks for the value
        // after its last pre-hold event — the async-generator rendezvous the
        // TS harness gets for free from `yield`.
        let receipt = if hold {
            let pre_hold = events
                .iter()
                .filter(|item| item.get("event").is_some())
                .count();
            let fake = self.fake.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let pushed = Arc::new(AtomicBool::new(false));
            Some(Arc::new(move || {
                let seen = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if seen == pre_hold + 1 && !pushed.swap(true, Ordering::SeqCst) {
                    fake.push_synthetic(&format!("turn-{index}-held"));
                }
            }) as Arc<dyn Fn() + Send + Sync>)
        } else {
            None
        };
        let fake = self.fake.clone();
        let held_turns = self.held_turns.clone();
        tokio::spawn(async move {
            play_events(&events, &sender, &fake).await;
            if hold {
                let (release, released) = oneshot::channel();
                held_turns
                    .lock()
                    .expect("#1328 held turns mutex")
                    .insert(index, release);
                if let Ok(extra) = released.await {
                    play_events(&extra, &sender, &fake).await;
                }
            }
        });
        WeixinTurnStream { receiver, receipt }
    }

    fn abort_turn<'a>(
        &'a self,
        session_key: &'a JsString,
        run_id: Option<&'a JsString>,
        reason: Option<&'a str>,
    ) -> super::WeixinAbortFuture<'a> {
        let mut value = json!({"sessionKey": js_text(session_key)});
        if let Some(run_id) = run_id {
            value["runId"] = json!(js_text(run_id));
        }
        if let Some(reason) = reason {
            value["reason"] = json!(reason);
        }
        self.aborts
            .lock()
            .expect("#1328 aborts mutex")
            .push(self.mask.value(&value));
        Box::pin(async { Ok(()) })
    }
}

/// The generator's `wrapClient`: synthetic recoverable transport failures are
/// injected at the same client seam on both sides, with a request-order
/// marker, and never contact the server.
struct WrappedClient {
    real: IlinkHttpClient,
    fake: Arc<FakeShared>,
    failures: Arc<AtomicUsize>,
}
impl WeixinIlinkClient for WrappedClient {
    fn cursor(&self) -> String {
        self.real.cursor()
    }
    fn set_cursor(&self, cursor: &str) {
        self.real.set_cursor(cursor);
    }
    fn poll(&self) -> BoxFuture<Result<JsValue, WeixinClientError>> {
        if self
            .failures
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        {
            self.fake.push_synthetic("poll-failure");
            return Box::pin(async { Err(WeixinClientError::fetch_failed()) });
        }
        self.real.poll()
    }
    fn send_text_chunked(
        &self,
        to_user_id: &JsString,
        text: &JsString,
        context_token: &JsString,
        max_length: usize,
    ) -> BoxFuture<Result<usize, WeixinClientError>> {
        self.real
            .send_text_chunked(to_user_id, text, context_token, max_length)
    }
    fn send_typing(
        &self,
        user_id: &JsString,
        context_token: &JsString,
    ) -> BoxFuture<Result<(), WeixinClientError>> {
        self.real.send_typing(user_id, context_token)
    }
}

/// `settleStop` (generate.mts): a stop parks on the poll loop's held long
/// poll. The settle gives the stop a head start so its abort has landed
/// before the parked poll is answered — the loop then exits at its top check
/// and never re-parks, and the replacement channel's polls are ordinary long
/// polls that stay parked (answering either made the recorded request count
/// depend on scheduler granularity).
async fn settle_stop<T>(fake: &FakeShared, future: impl std::future::Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    match tokio::time::timeout(Duration::from_millis(25), future.as_mut()).await {
        Ok(value) => return value,
        Err(_) => fake.release_held(),
    }
    // Safety net only: a park the head start did not cover would otherwise
    // hang the scenario; retrying converts it into a loud, detectable
    // divergence instead.
    for _ in 0..3 {
        match tokio::time::timeout(Duration::from_secs(2), future.as_mut()).await {
            Ok(value) => return value,
            Err(_) => fake.release_held(),
        }
    }
    panic!("#1328 weixin settle_stop timed out");
}

/// The generator sorts logs by [level, text] before recording them (console
/// and channel logger interleave across streams — not part of the protocol).
fn log_key(entry: &Value) -> (&str, &str) {
    (
        entry[0].as_str().unwrap_or_default(),
        entry[1].as_str().unwrap_or_default(),
    )
}

fn read_json_file(path: &std::path::Path, mask: &Mask) -> Value {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) => mask.value(&value),
            Err(_) => Value::Null,
        },
        Err(_) => Value::Null,
    }
}

// ── the scenario runner ───────────────────────────────────────────────────

struct Root(PathBuf);
impl Root {
    fn new(tag: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("pdrs-1328-weixin-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).expect("#1328 replay scenario root");
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.clone()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn replay_case(case: &Value) -> Value {
    let name = case["name"].as_str().expect("#1328 case name");
    let root = Root::new(name);
    let scenario_dir = root.path();
    let pilot_home = scenario_dir.join("pilot-home");
    std::fs::create_dir_all(&pilot_home).expect("#1328 pilot home");
    let credentials_path = scenario_dir.join("weixin-credentials.json");
    let mask = Arc::new(Mask::new(&scenario_dir));
    let fake = FakeIlink::start(mask.clone()).await;
    let base_url = format!("http://127.0.0.1:{}", fake.port);

    let reports: Arc<Mutex<Vec<Value>>> = Arc::default();
    let logs: Arc<Mutex<Vec<Value>>> = Arc::default();
    let submits: Arc<Mutex<Vec<Value>>> = Arc::default();
    let aborts: Arc<Mutex<Vec<Value>>> = Arc::default();
    let results: Arc<Mutex<Vec<Value>>> = Arc::default();
    let host_closes = Arc::new(AtomicUsize::new(0));
    let turns: Vec<Value> = case["turns"].as_array().cloned().unwrap_or_default();
    let synthetic_failures = case["syntheticPollFailures"].as_u64().unwrap_or(0) as usize;
    let held_turns: Arc<Mutex<HashMap<usize, oneshot::Sender<Vec<Value>>>>> = Arc::default();

    // The step loop state: the channel assembly is constructed by the
    // `server` step (one per scenario, exactly like the generator's
    // `startPilotDeckServer`), and `prepare` before it answers the
    // no-serverRef guard (InProcessGateway.ts:2912-2921, pinned verbatim by
    // the methods tests).
    let mut server_ref: Option<Arc<NativeChannels>> = None;
    let capture = |value: Value| {
        results
            .lock()
            .expect("#1328 results mutex")
            .push(mask.value(&value));
    };

    for step in case["steps"].as_array().expect("#1328 scenario steps") {
        let op = step["op"].as_str().expect("#1328 step op");
        match op {
            "server" => {
                let config = js_event(&step["config"]);
                let weixin = super::WeixinChannelOptions {
                    credentials_path: Some(credentials_path.clone()),
                    // generate.mts points `HOME` at the scenario directory
                    // (`homedir()/.pilotdeck` = `<tmp>/.pilotdeck`, what the
                    // golden's im-attachments paths record); the Rust port's
                    // single home knob redirects the whole family instead,
                    // so a replay never touches the machine's real
                    // `~/.pilotdeck` (#1725 item 13).
                    credentials_home: Some(scenario_dir.join(".pilotdeck")),
                    mapper: Arc::new(super::WeixinSessionMapper::new()),
                    client_factory: {
                        let fake_shared = fake.shared.clone();
                        let failures = Arc::new(AtomicUsize::new(synthetic_failures));
                        Arc::new(move |credentials: &SavedCredentials| {
                            let real = IlinkHttpClient::new(
                                credentials.base_url.clone(),
                                credentials.bot_token.clone(),
                            );
                            Arc::new(WrappedClient {
                                real,
                                fake: fake_shared.clone(),
                                failures: failures.clone(),
                            }) as Arc<dyn WeixinIlinkClient>
                        })
                    },
                    login: {
                        let base_url = base_url.clone();
                        Arc::new(move |callbacks| {
                            let base_url = base_url.clone();
                            Box::pin(async move { login_with_qr(callbacks, &base_url).await })
                        })
                    },
                    on_state_change: None,
                    gateway: None,
                    runtime_status: None,
                    // The weixin defaults the generator's compiled channel
                    // uses (no watchdog inside the replay scenarios).
                    live_reply: super::weixin_live_reply_options(),
                };
                let channels = Arc::new(NativeChannels::new(NativeChannelsOptions {
                    pilot_home: pilot_home.clone(),
                    config: Arc::new(config),
                    weixin,
                    feishu: crate::channel::feishu::FeishuOptions::default(),
                    weixin_maker: None,
                    log: {
                        let logs = logs.clone();
                        let mask = mask.clone();
                        Some(Arc::new(move |level: &str, text: &str| {
                            logs.lock()
                                .expect("#1328 logs mutex")
                                .push(json!([level, mask.string(text)]));
                        }))
                    },
                    report_status: {
                        // The recorder wraps the real runtime-status writer,
                        // so the durable sink stays exercised while the
                        // reports are captured (the generator wraps the
                        // compiled module the same way).
                        let real = create_channel_runtime_status_reporter(pilot_home.clone());
                        let reports = reports.clone();
                        let mask = mask.clone();
                        Some(Arc::new(
                            move |key: &JsString, update: &ChannelRuntimeStatusUpdate| {
                                let mut value = json!({
                                    "channelKey": js_text(key),
                                    "state": update.state,
                                    "message": update.message.clone().unwrap_or_default(),
                                });
                                if let Some(account_id) = &update.account_id {
                                    value["accountId"] = json!(js_text(account_id));
                                }
                                if let Some(error) = &update.error {
                                    value["error"] = json!(mask.string(error));
                                }
                                if let Some(qr_url) = &update.qr_url {
                                    value["qrUrl"] = json!(js_text(qr_url));
                                }
                                reports
                                    .lock()
                                    .expect("#1328 reports mutex")
                                    .push(mask.value(&value));
                                real(key, update);
                            },
                        ))
                    },
                    close_host: {
                        let host_closes = host_closes.clone();
                        Some(Arc::new(move || {
                            host_closes.fetch_add(1, Ordering::SeqCst);
                            Box::pin(async { Ok(()) })
                        }))
                    },
                }));
                let port = HarnessPort {
                    submits: submits.clone(),
                    aborts: aborts.clone(),
                    fake: fake.shared.clone(),
                    mask: mask.clone(),
                    turns: turns.clone(),
                    turn_index: Arc::new(AtomicUsize::new(0)),
                    held_turns: held_turns.clone(),
                };
                crate::negative_assert!(
                    channels.set_gateway_port(Arc::new(port)),
                    "#1328 replay publishes its gateway port once"
                );
                if let Some(weixin) = channels.create_startup_weixin().await {
                    channels.server().start_channels(vec![weixin]);
                }
                server_ref = Some(channels);
            }
            "prepare" => {
                let outcome = match &server_ref {
                    None => json!({
                        "requested": false,
                        "requestedAt": String::from_utf16_lossy(SystemExecutionClock.now_iso().units()),
                        "reason": "unsupported",
                    }),
                    Some(channels) => {
                        let callback = channels.prepare_login_callback();
                        match settle_stop(&fake.shared, async { callback().await }).await {
                            Ok(value) => describe(&value),
                            Err(error) => json!({"error": js_text(&error.message)}),
                        }
                    }
                };
                capture(outcome);
            }
            "credentials" => {
                let text = serde_json::to_string(&step["json"]).expect("#1328 credentials JSON");
                let text = text.replace("$BASE", &base_url);
                let value: Value = serde_json::from_str(&text).expect("#1328 credentials parse");
                if let Some(parent) = credentials_path.parent() {
                    std::fs::create_dir_all(parent).expect("#1328 credentials parent");
                }
                std::fs::write(
                    &credentials_path,
                    serde_json::to_string_pretty(&value).expect("#1328 credentials pretty"),
                )
                .expect("#1328 credentials write");
            }
            "script" => {
                let endpoint = step["endpoint"].as_str().expect("#1328 script endpoint");
                // #1822: `$BASE` inside message items points at the loopback
                // CDN, exactly like the credentials step's substitution.
                let text = serde_json::to_string(&step["items"]).expect("#1328 script JSON");
                let text = text.replace("$BASE", &base_url);
                let items: Value = serde_json::from_str(&text).expect("#1328 script parse");
                let items = items.as_array().expect("#1328 script items").clone();
                fake.shared.push_script(endpoint, &items);
            }
            "wait-requests" => {
                let path = step["path"].as_str().expect("#1328 wait path");
                let count = step["count"].as_u64().expect("#1328 wait count") as usize;
                until(|| fake.shared.count_path(path) >= count).await;
            }
            "wait-reports" => {
                let state = step["state"].as_str().expect("#1328 wait state");
                until(|| {
                    reports
                        .lock()
                        .expect("#1328 reports mutex")
                        .last()
                        .and_then(|report| report.get("state"))
                        .and_then(Value::as_str)
                        == Some(state)
                })
                .await;
            }
            "wait-log" => {
                let text = step["text"].as_str().expect("#1328 wait log text");
                until(|| {
                    logs.lock()
                        .expect("#1328 logs mutex")
                        .iter()
                        .any(|entry| entry[1].as_str() == Some(text))
                })
                .await;
            }
            "wait-submits" => {
                let count = step["count"].as_u64().expect("#1328 wait submits") as usize;
                until(|| submits.lock().expect("#1328 submits mutex").len() >= count).await;
            }
            "wait-held" => {
                let turn = step["turn"].as_u64().expect("#1328 wait held turn") as usize;
                let marker = format!("turn-{turn}-held");
                until(|| {
                    fake.shared
                        .requests
                        .lock()
                        .expect("#1328 fake requests mutex")
                        .iter()
                        .any(|request| {
                            request.get("synthetic").and_then(Value::as_str) == Some(&marker)
                        })
                })
                .await;
            }
            "release-turn" => {
                let turn = step["turn"].as_u64().expect("#1328 release turn") as usize;
                let events = step["events"].as_array().cloned().unwrap_or_default();
                if let Some(release) = held_turns
                    .lock()
                    .expect("#1328 held turns mutex")
                    .remove(&turn)
                {
                    let _ = release.send(events);
                }
            }
            "reload" => {
                let channels = server_ref.clone().expect("#1328 reload has a server");
                let config = js_event(&step["config"]);
                let outcome = match settle_stop(&fake.shared, channels.reload(&config)).await {
                    Ok(result) => json!(
                        result
                            .started
                            .iter()
                            .map(|key| format!("{key}=started"))
                            .chain(result.stopped.iter().map(|key| format!("{key}=stopped")))
                            .collect::<Vec<String>>()
                    ),
                    Err(error) => json!({ "error": error.to_string() }),
                };
                capture(outcome);
            }
            "flush" => {
                let channels = server_ref.clone().expect("#1328 flush has a server");
                channels.flush().await.expect("#1328 replay flush resolves");
            }
            "close" => {
                let channels = server_ref.clone().expect("#1328 close has a server");
                match settle_stop(&fake.shared, channels.close()).await {
                    Ok(()) => capture(Value::Null),
                    Err(error) => capture(json!({"error": error.to_string()})),
                }
            }
            other => panic!("#1328 unknown step {other}"),
        }
    }
    tokio::task::yield_now().await;
    let mut sorted_logs = logs.lock().expect("#1328 logs mutex").clone();
    sorted_logs.sort_by(|a, b| log_key(a).cmp(&log_key(b)));
    let actual = json!({
        "requests": fake.shared.requests.lock().expect("#1328 fake requests mutex").clone(),
        "reports": reports.lock().expect("#1328 reports mutex").clone(),
        "logs": sorted_logs,
        "submits": submits.lock().expect("#1328 submits mutex").clone(),
        "aborts": aborts.lock().expect("#1328 aborts mutex").clone(),
        "results": results.lock().expect("#1328 results mutex").clone(),
        "hostCloses": host_closes.load(Ordering::SeqCst),
        "files": {
            "credentials": read_json_file(&credentials_path, &mask),
            "weixinState": read_json_file(&pilot_home.join("channels").join("weixin.state.json"), &mask),
            "runtimeStatus": read_json_file(&pilot_home.join("channels").join("runtime-status.json"), &mask),
        },
    });
    // Cleanup mirrors the generator's finally: close the server (bounded),
    // then destroy the mock before the scenario root is removed.
    if let Some(channels) = &server_ref {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            settle_stop(&fake.shared, channels.close()),
        )
        .await;
    }
    fake.destroy();
    actual
}

/// The terminal comparison reads `expected` through a checked accessor instead
/// of a bare index (#1808, the #1729 shape): a present-but-wrong-type
/// container fails with a `#1328` diagnostic instead of a generic object
/// mismatch, and an absent one cannot shrink the case to a blind compare.
///
/// Deliberately narrower than sms/webhook's `recorded_checked`: every case in
/// these oracles must carry an object `expected`, so absent is an error rather
/// than a `fail_open` switch, and the two-state `Result<&Value, _>` keeps the
/// call sites free of an `Option` hop. Sharing one helper across all five
/// harnesses is follow-up work, not this lane (#1813 owns the other files).
fn recorded_expected_checked(value: &Value) -> Result<&Value, String> {
    match value.get("expected") {
        None => Err("#1328 expected must be an object, got <missing>".into()),
        Some(expected) if expected.is_object() => Ok(expected),
        Some(expected) => Err(format!("#1328 expected must be an object, got {expected}")),
    }
}
fn recorded_expected(value: &Value) -> &Value {
    recorded_expected_checked(value).unwrap_or_else(|error| panic!("{error}"))
}
#[test]
fn recorded_expected_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_expected_checked(&json!({"expected": []})).is_err(),
        "#1328 #1808 rejects expected type drift"
    );
    crate::negative_assert!(
        recorded_expected_checked(&json!({})).is_err(),
        "#1328 #1808 rejects absent expected"
    );
}
#[test]
#[should_panic(expected = "#1328 expected must be an object")]
fn recorded_expected_panics_on_wrong_type() {
    recorded_expected(&json!({"expected": []}));
}
#[test]
#[should_panic(expected = "#1328 expected must be an object, got <missing>")]
fn recorded_expected_panics_when_absent() {
    recorded_expected(&json!({}));
}

/// The channel-scenario count the generator declares (`expectedCaseCount`);
/// the count pin and the declaration pin read this one constant, so the two
/// literals cannot drift apart (#1725 — the count moved 15→20 in #1822 and
/// both sites had to be found by hand).
const EXPECTED_CASE_COUNT: usize = 20;

fn oracle_cases(oracle: &Value) -> Vec<&Value> {
    let cases = oracle["cases"]
        .as_array()
        .expect("#1328 weixin oracle cases");
    // #1823 appended media cases (they carry a `kind`); the channel replay
    // owns the twenty scenario cases (#1822 added the five attachment ones),
    // and `channel::weixin::media::tests` replays the rest.
    let channel: Vec<&Value> = cases
        .iter()
        .filter(|case| case.get("kind").is_none())
        .collect();
    crate::negative_assert!(
        !channel.is_empty(),
        "#1328 weixin oracle cases must be non-empty"
    );
    crate::negative_assert_eq!(
        channel.len(),
        EXPECTED_CASE_COUNT,
        "#1328 weixin oracle case count"
    );
    crate::negative_assert_eq!(
        oracle["expectedCaseCount"],
        json!(EXPECTED_CASE_COUNT),
        "#1328 weixin generator declaration"
    );
    crate::negative_assert_eq!(
        oracle["expectedMediaCaseCount"],
        json!(cases.len() - channel.len()),
        "#1823 weixin generator media declaration"
    );
    channel
}

#[tokio::test]
async fn weixin_replays_the_ts_channel_oracle() {
    let oracle = oracle(ORACLE);
    for case in oracle_cases(&oracle) {
        let actual = replay_case(case).await;
        let expected = recorded_expected(case).clone();
        crate::negative_assert_eq!(actual, expected, "#1328 weixin replay oracle");
    }
}
