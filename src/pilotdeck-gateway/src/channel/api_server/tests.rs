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
        atomic::{AtomicBool, AtomicUsize, Ordering},
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

/// `<N bytes>` stands for `N` repeated `x` bytes: the oracle records the
/// request *length*, not a megabyte of filler, so a replay that sends the
/// placeholder literally would exercise a 15-byte body instead of the cap.
/// Any string that is not exactly that shape is the request body itself.
///
/// Expansion is load-bearing, so it is fail-closed (#1606 r1/r2). The
/// `body-limit-exact` cell expects the same `400 invalid_json` for a 15-byte
/// body as for the full megabyte, so a placeholder that quietly failed to
/// expand would leave the cell green while testing nothing. A `<... bytes>`
/// whose middle does not parse is a fixture bug and panics.
fn recorded_body(text: &str) -> Vec<u8> {
    let Some(count) = text
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix(" bytes>"))
    else {
        return text.as_bytes().to_vec();
    };
    // #1606 r4: `usize::from_str` also accepts a leading `+`, so "plain
    // decimal" was a promise the code did not quite keep. Reject the quirk
    // instead of documenting it — the placeholder is fixture protocol, so only
    // the canonical spelling should reach the byte count.
    if count.is_empty() || !count.bytes().all(|byte| byte.is_ascii_digit()) {
        panic!("#1333 request placeholder {text:?} is not a plain decimal byte count");
    }
    let count = count.parse::<usize>().unwrap_or_else(|error| {
        panic!("#1333 request placeholder {text:?} does not expand to a byte count: {error}")
    });
    assert!(
        count <= REPLAY_BODY_BUDGET,
        "#1333 request placeholder {text:?} is beyond the replay budget"
    );
    vec![b'x'; count]
}

/// Allocation ceiling for a fixture-supplied byte count (#1606 r2). The
/// largest placeholder the oracle legitimately records is the over-cap probing
/// body, `MAX_REQUEST_BYTES + 1`; anything past that is a fixture bug, so it is
/// refused *before* `vec![b'x'; count]` can abort the whole test binary on
/// allocation instead of failing one assertion. It is derived from the product
/// cap on purpose: a fixture that stops tracking the cap is the drift this
/// guard exists to catch.
const REPLAY_BODY_BUDGET: usize = MAX_REQUEST_BYTES + 1;

/// Pin each boundary cell's *expanded* body to the cap it exists to probe
/// (#1606 r2). The placeholder's syntax was already fail-closed, but its count
/// was unconstrained: `body-limit-exact` and the `oversized-*` cells all answer
/// exactly as a short body would (`400 invalid_json` / a zero-byte close), so
/// an oracle edited to `<15 bytes>` — or a generator that recorded the wrong
/// count — would leave every one of them green while proving nothing. Each
/// boundary cell must therefore replay the length it claims to test.
fn assert_boundary_body_len(id: &str, case: &Value) {
    let expected = match id {
        "body-limit-exact" => MAX_REQUEST_BYTES,
        _ if id.starts_with("oversized-") => MAX_REQUEST_BYTES + 1,
        _ => return,
    };
    let requests = case["requests"].as_array().expect("#1333 requests");
    assert_eq!(
        requests.len(),
        1,
        "#1333 {id}: a boundary cell replays exactly one request"
    );
    let text = requests[0]["body"]
        .as_str()
        .unwrap_or_else(|| panic!("#1333 {id}: a boundary cell must record a body"));
    let len = recorded_body(text).len();
    assert_eq!(
        len, expected,
        "#1333 {id}: boundary cell must replay {expected} bytes (the product cap), not {len}; \
         the fixture and `MAX_REQUEST_BYTES` have drifted apart"
    );
}

#[test]
fn recorded_body_expands_the_length_placeholder() {
    assert_eq!(recorded_body("<5 bytes>"), b"xxxxx");
    // A literal, not `MAX_REQUEST_BYTES`: this test owns the placeholder
    // syntax. That the fixture's cap *is* the product cap is asserted where it
    // matters, by `assert_boundary_body_len` over the real oracle cells.
    assert_eq!(recorded_body("<1000000 bytes>").len(), 1_000_000);
    // Anything that is not the placeholder syntax is the body verbatim.
    assert_eq!(recorded_body("{}"), b"{}");
    assert_eq!(recorded_body(""), b"");
}

#[test]
#[should_panic(expected = "request body must be a string")]
fn req_spec_refuses_a_non_string_body() {
    // `unwrap_or("")` here would replay an empty body against a cell that only
    // distinguishes bodies by "is this valid JSON".
    ReqSpec::from_json(&json!({
        "method": "POST",
        "path": "/v1/chat/completions",
        "body": 15,
    }));
}

#[test]
#[should_panic(expected = "must be a string")]
fn req_spec_refuses_a_non_string_header() {
    // `unwrap_or("")` here would send an empty `connection` and quietly turn a
    // keep-alive cell into a close cell.
    ReqSpec::from_json(&json!({
        "method": "GET",
        "path": "/health",
        "headers": { "connection": true },
    }));
}

#[test]
#[should_panic(expected = "request headers must be an object")]
fn req_spec_refuses_a_non_object_header_container() {
    // #1606 r4: r3 caught non-string header *values*; a `headers` that is not
    // an object at all still slipped through to "no headers", which silently
    // dropped a recorded `connection: keep-alive`.
    ReqSpec::from_json(&json!({
        "method": "GET",
        "path": "/health",
        "headers": [],
    }));
}

#[test]
#[should_panic(expected = "is not a plain decimal byte count")]
fn recorded_body_refuses_a_malformed_placeholder() {
    // A near-miss placeholder must not fall back to replaying 15 literal
    // bytes: that is the fake-PASS shape this cell exists to rule out.
    recorded_body("<1_000_000 bytes>");
}

#[test]
#[should_panic(expected = "is not a plain decimal byte count")]
fn recorded_body_refuses_a_signed_count() {
    // #1606 r4: `"+5".parse::<usize>()` is `Ok(5)`, so the old reader accepted
    // a spelling no generator writes.
    recorded_body("<+5 bytes>");
}

#[test]
#[should_panic(expected = "does not expand to a byte count")]
fn recorded_body_refuses_an_overflowing_count() {
    // All digits, so it reaches `parse` and fails there — the branch the
    // strict digit check above does not take.
    recorded_body("<99999999999999999999999999 bytes>");
}

#[test]
#[should_panic(expected = "is beyond the replay budget")]
fn recorded_body_refuses_an_unbounded_count() {
    // Parseable by a 32-bit `usize` too, so the test depends on the budget
    // rule and not on the target's pointer width.
    recorded_body("<9000000 bytes>");
}

// #1606 r6: the container-level tri-state, which the 14 oracle cells cannot
// exercise — every one of them records an `options` object, and the 11 cells
// without a gateway record none at all, so a regression to `unwrap_or_default`
// would still replay green.
#[test]
#[should_panic(expected = "options must be an object")]
fn options_refuses_a_non_object_container() {
    options(&json!({ "options": [] }), None);
}

#[test]
fn absent_containers_stay_legal() {
    // Absent means "the cell recorded no options / no gateway", which every
    // cell without those keys relies on.
    let options = options(&json!({}), None);
    assert_eq!(options.model_name.as_deref(), Some("parity-model"));
    assert_eq!(options.api_key.as_deref(), Some(""));
    gateway_from(&json!({}), None, Arc::new(AtomicBool::new(false)));
}

#[test]
#[should_panic(expected = "gateway must be an object")]
fn gateway_from_refuses_a_non_object_gateway() {
    gateway_from(
        &json!({ "gateway": 5 }),
        None,
        Arc::new(AtomicBool::new(false)),
    );
}

#[test]
#[should_panic(expected = "gateway.turns must be an array")]
fn gateway_from_refuses_non_array_turns() {
    gateway_from(
        &json!({ "gateway": { "turns": "x" } }),
        None,
        Arc::new(AtomicBool::new(false)),
    );
}

#[test]
#[should_panic(expected = "gateway turn events must be an array")]
fn gateway_from_refuses_non_array_events() {
    gateway_from(
        &json!({ "gateway": { "turns": [{ "events": "x" }] } }),
        None,
        Arc::new(AtomicBool::new(false)),
    );
}

#[test]
#[should_panic(expected = "gateway turn error must be a string")]
fn gateway_from_refuses_a_non_string_error() {
    gateway_from(
        &json!({ "gateway": { "turns": [{ "events": [], "error": 7 }] } }),
        None,
        Arc::new(AtomicBool::new(false)),
    );
}

// #1606 r6 (MiniMax sibling): r4/r5 added three more fail-closed readers with
// no test of their own. Reverting any of them to `unwrap_or` keeps all 14
// cells green — the three cells that record CORS / an api key record the
// correct types — so nothing else would notice the ruler going quiet.
#[test]
#[should_panic(expected = "options.apiKey must be a string")]
fn case_option_refuses_a_non_string_api_key() {
    options(&json!({ "options": { "apiKey": 7 } }), None);
}

#[test]
#[should_panic(expected = "options.modelName must be a string")]
fn case_option_refuses_a_non_string_model_name() {
    options(
        &json!({ "options": { "modelName": ["parity-model"] } }),
        None,
    );
}

#[test]
#[should_panic(expected = "options.corsOrigins must be an array")]
fn case_origins_refuses_a_non_array() {
    options(&json!({ "options": { "corsOrigins": "*" } }), None);
}

#[test]
#[should_panic(expected = "corsOrigins entries must be strings")]
fn case_origins_refuses_a_non_string_entry() {
    options(
        &json!({ "options": { "corsOrigins": ["http://a", 5] } }),
        None,
    );
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
        // #1606 r3: headers were the last field still parsed with
        // `unwrap_or("")` while `body` / `method` / `path` had already been
        // made fail-closed. A non-string header value (say
        // `"connection": true`) silently replayed as an empty header and
        // flipped `keep_alive` to false, so the request that reached the
        // server was not the one the oracle recorded.
        // #1606 r4 (both families): r3 tightened the header *values* but left
        // the *container* fail-open — `value["headers"].as_object()` on a
        // `[]` / `"x"` / `null` returned `None` and `unwrap_or_default()`
        // silently replayed a request with no headers at all, the same
        // "the request that reached the server is not the recorded one" shape.
        // Absent stays legitimate (GET cells may record none); anything else
        // must be an object of strings.
        let headers: Vec<(String, String)> = match value.get("headers") {
            None => Vec::new(),
            Some(Value::Object(headers)) => headers
                .iter()
                .map(|(name, value)| {
                    let value = value.as_str().unwrap_or_else(|| {
                        panic!("#1333 header {name:?} must be a string, found {value:?}")
                    });
                    (name.clone(), value.to_owned())
                })
                .collect(),
            Some(other) => {
                panic!("#1333 request headers must be an object, found {other:?}")
            }
        };
        // #1606 r2: a missing `body` is legitimate (GET/HEAD cells), but a
        // present non-string one is a fixture bug: `unwrap_or("")` would
        // replay an empty body and a boundary cell would stay green.
        let body = match value.get("body") {
            Some(Value::String(text)) => recorded_body(text),
            Some(other) => panic!("#1333 request body must be a string, found {other:?}"),
            None => Vec::new(),
        };
        // Read back off the parsed headers rather than the raw JSON, so the
        // two can never disagree about which connection mode was requested.
        let keep_alive = headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("connection") && value.eq_ignore_ascii_case("keep-alive")
        });
        // #1606 r2: every oracle request carries both fields, so a missing one
        // is fixture drift, not a defaultable cell.
        let method = value["method"]
            .as_str()
            .unwrap_or_else(|| panic!("#1333 request is missing a string `method`: {value}"));
        let path = value["path"]
            .as_str()
            .unwrap_or_else(|| panic!("#1333 request is missing a string `path`: {value}"));
        Self {
            method: method.to_owned(),
            path: path.to_owned(),
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
        // A hung connection also ends with `error`/no status once the 10 s
        // client timeout fires; without this the ruler would pass on a hang,
        // which the TS generator (no timeout) can never do.
        assert_ne!(
            actual.error.as_deref(),
            Some("timeout"),
            "#1333 {case} request {index}: the connection must close, not hang"
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
            // #1606 r4: the expectation side gets the same fail-closed rule as
            // the request side. `unwrap_or("")` compared a malformed recorded
            // header as an empty one, so a drifted oracle could still match.
            let value = value.as_str().unwrap_or_else(|| {
                panic!("#1333 oracle header {name:?} must be a string, found {value:?}")
            });
            (name.to_ascii_lowercase(), value.to_owned())
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

/// #1737: a gateway whose `submit` rejects outright. No scripted turn can
/// reach the `Err` arms of `stream_turn` / `buffered_turn`: `ScriptedGateway`
/// always returns a stream and `ScriptedTurn.error` only fails the stream's
/// `next()`, so the submit-rejection path gets its own mock. The counter is
/// the no-retry assertion: both call sites must submit exactly once.
struct FailingGateway {
    calls: AtomicUsize,
}

impl TurnGateway for FailingGateway {
    fn submit(
        &self,
        _request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(ChannelError::Error(
                "#1737 scripted submit rejection".to_owned(),
            ))
        })
    }
}

fn js_string_array(values: &[Value]) -> JsValue {
    JsValue::Array(
        values
            .iter()
            .map(|value| match value.as_str() {
                // #1606 r5: an element that is not a string used to become an
                // empty origin, i.e. an allow-list entry nobody wrote.
                Some(origin) => JsValue::String(JsString::from(origin)),
                None => panic!("#1333 corsOrigins entries must be strings, found {value:?}"),
            })
            .collect(),
    )
}

/// #1606 r5: `corsOrigins` is the last field in this struct that was still
/// two-state. Present-but-not-an-array silently started a server with no CORS
/// at all — the same "absent vs. wrong type" conflation this file removes
/// everywhere else.
fn case_origins(options: &Value) -> Option<JsValue> {
    match options.get("corsOrigins") {
        None => None,
        Some(Value::Array(values)) => Some(js_string_array(values)),
        Some(other) => panic!("#1333 options.corsOrigins must be an array, found {other:?}"),
    }
}

/// #1606 r4: the same tri-state rule the request fields follow. A recorded
/// option that is present but not a string is fixture drift: `unwrap_or`
/// would quietly start a server with different options than the cell was
/// captured under, and the cell would still be green (the recorded default
/// happens to equal the fallback).
fn case_option(options: &Value, key: &str, fallback: &str) -> String {
    match options.get(key) {
        Some(Value::String(text)) => text.to_owned(),
        Some(other) => panic!("#1333 options.{key} must be a string, found {other:?}"),
        None => fallback.to_owned(),
    }
}

fn options(case: &Value, timeout: Option<Duration>) -> ApiServerOptions {
    // #1606 r6 (both zai roles, independently): r5 tightened every key *inside*
    // `options` and claimed this file no longer conflates absent with
    // wrong-type — but the container itself was still two-state. With
    // `"options": []` / `"x"` / `5` every key below reads as absent through
    // `Value::get`, so the cell starts under the recorded defaults while the
    // oracle recorded something else, and stays green whenever that option is
    // not visible in the compared response. Same shape as the `headers`
    // container fixed in r4. Absent stays legitimate (`&case["options"]` is
    // `Null` and every key falls back); present-but-not-an-object is drift.
    if let Some(other) = case.get("options").filter(|value| !value.is_object()) {
        panic!("#1333 options must be an object, found {other:?}");
    }
    let options = &case["options"];
    ApiServerOptions {
        port: Some(JsValue::Number(0.0)),
        api_key: Some(case_option(options, "apiKey", "")),
        host: Some("127.0.0.1".to_owned()),
        model_name: Some(case_option(options, "modelName", "parity-model")),
        cors_origins: case_origins(options),
        mapper: None,
        request_timeout: timeout,
        body_read_timeout: None,
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
    // #1606 r6 (zai boundary P3, same family as the `options` container above):
    // `as_array().unwrap_or_default()` made a present-but-wrong-type `gateway`
    // or `turns` read as "this cell scripts no turns", so a drifted oracle
    // started a gateway with an empty script. Most such cells would still fail
    // loudly on the response, but the shape is the one r2–r5 removed
    // everywhere else, so the container gets the same tri-state rule. Absent
    // (`Null`) stays legitimate — most cells record no gateway at all.
    let gateway = &case["gateway"];
    let turns = match gateway {
        Value::Null => Vec::new(),
        Value::Object(_) => match &gateway["turns"] {
            Value::Null => Vec::new(),
            Value::Array(turns) => turns
                .iter()
                .map(|turn| ScriptedTurn {
                    events: match turn.get("events") {
                        None | Some(Value::Null) => Vec::new(),
                        Some(Value::Array(events)) => events.clone(),
                        Some(other) => {
                            panic!("#1333 gateway turn events must be an array, found {other:?}")
                        }
                    },
                    error: match turn.get("error") {
                        None | Some(Value::Null) => None,
                        Some(Value::String(text)) => Some(text.clone()),
                        Some(other) => {
                            panic!("#1333 gateway turn error must be a string, found {other:?}")
                        }
                    },
                })
                .collect(),
            other => panic!("#1333 gateway.turns must be an array, found {other:?}"),
        },
        other => panic!("#1333 gateway must be an object, found {other:?}"),
    };
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
    // Exact ids, not a count: a regenerated oracle that renames or swaps one
    // cell would keep `len == 14` while silently dropping a surface.
    let ids: Vec<&str> = cases
        .iter()
        .map(|case| case["id"].as_str().expect("#1333 case id"))
        .collect();
    assert_eq!(
        ids,
        [
            "method-dispatch",
            "cors-exact",
            "cors-star",
            "auth",
            "chat-validation",
            "buffered",
            "stream",
            "new-command",
            "gateway-missing",
            "oversized-0",
            "oversized-1",
            "oversized-2",
            "body-limit-exact",
            "body-read-timeout",
            "keep-alive",
        ],
        "#1333 oracle must keep every TS surface cell, in order"
    );
    for case in cases {
        let id = case["id"].as_str().expect("#1333 case id");
        // Runs for the boundary cells only; a no-op everywhere else.
        assert_boundary_body_len(id, case);
        // #1606 r3: `compare_response` walks `expect` by request index, so a
        // cell that records *more* responses than requests would leave the
        // surplus never compared — a TS surface silently skipped while the
        // cell stays green. Fewer responses already panics on the index; this
        // closes the other direction for every cell, not just boundary ones.
        let recorded_requests = case["requests"].as_array().expect("#1333 requests");
        // #1606 r4: the pairing below is 0 == 0 for a cell that records
        // neither, and such a cell would replay nothing while still counting
        // as covered.
        assert!(
            !recorded_requests.is_empty(),
            "#1333 {id}: a cell must record at least one request"
        );
        assert_eq!(
            case["expect"].as_array().map(Vec::len),
            Some(recorded_requests.len()),
            "#1333 {id}: every recorded TS response must pair with a recorded request"
        );
        if id == "body-read-timeout" {
            // #1620: a client that declares `Content-Length` and then stops
            // sending is answered by Node's `requestTimeout` (300 s) with the
            // 408 and a close. Not replayed here because the real cell waits
            // out that full 300 s and hyper frames the same 408 with a
            // `Transfer-Encoding` header the oracle's parsed view does not
            // carry. `partial_body_is_answered_at_the_body_read_deadline`
            // pins the Rust side with a shortened injected deadline instead.
            //
            // Pin the TS side so the skip is not blind: the baseline must
            // deliver the 408, not reset the connection, and it must record
            // how much of the declared body it sent before stalling.
            let expected = &case["expect"][0];
            assert_eq!(
                expected["status"].as_u64(),
                Some(408),
                "#1333 {id}: the baseline answers a stalled body with 408, got {expected}"
            );
            assert_eq!(
                expected["body"].as_str(),
                Some(""),
                "#1333 {id}: the requestTimeout body is empty, got {expected}"
            );
            assert!(
                expected.get("error").is_none(),
                "#1333 {id}: the baseline must deliver the 408, not reset the connection, got {expected}"
            );
            // The Rust side asserts this 408 carries no `Date`
            // (`partial_body_is_answered_at_the_body_read_deadline`). Pin the
            // same contract on the recorded baseline answer, so the two sides
            // cannot drift into opposite conventions without a red test.
            assert!(
                expected["headers"].get("date").is_none(),
                "#1333 {id}: the baseline requestTimeout 408 has no Date header, got {expected}"
            );
            let request = &recorded_requests[0];
            let declared: u64 = request["headers"]["content-length"]
                .as_str()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| {
                    panic!(
                        "#1333 {id}: the request must declare a numeric Content-Length: {request}"
                    )
                });
            let stalled = request["stalledAfter"].as_u64().unwrap_or_else(|| {
                panic!("#1333 {id}: the request must record how many bytes it sent: {request}")
            });
            assert!(
                stalled < declared,
                "#1333 {id}: the request must stall before the declared body is complete \
                 (sent {stalled} of {declared})"
            );
            continue;
        }
        if id.starts_with("oversized-") {
            // The three oversized cells record the same request; one of them is
            // replayed for real by `oversized_body_closes_without_response`
            // (`assert_boundary_body_len` above keeps the other two honest
            // about their length). Not replayed here because the server
            // destroys the connection mid-write, and the exact error string
            // (ECONNRESET / EPIPE / a clean EOF) depends on write progress.
            // `body-limit-exact` stays in this loop: a body at the cap is read
            // in full and answered, so its response is deterministic.
            //
            // #1606 r4 (both families): skipping the replay also skipped the
            // recorded response, so nothing compared the TS side of these
            // cells at all. If the baseline cap ever rises above the recorded
            // `OVER_CAP_BYTES`, the generator would record `400 invalid_json`
            // here while the placeholder and the Rust cap stay at 1_000_001 —
            // the whole surface green and the two sides disagreeing. Pin the
            // shape: a close with zero bytes and no status line.
            let expected = &case["expect"][0];
            assert_eq!(
                expected["bytes"].as_u64(),
                Some(0),
                "#1333 {id}: an oversized body must close without delivering bytes, got {expected}"
            );
            assert!(
                expected.get("status").is_none(),
                "#1333 {id}: an oversized body must not receive a status line, got {expected}"
            );
            assert!(
                expected["error"].is_string(),
                "#1333 {id}: an oversized body must record the connection error, got {expected}"
            );
            continue;
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let gateway = case
            .get("gateway")
            .map(|_| gateway_from(case, None, dropped.clone()) as Arc<dyn TurnGateway>);
        let started = start(options(case, None), gateway.clone()).await;
        for (index, request) in recorded_requests.iter().enumerate() {
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
    // #1606 r5: `submits` is optional, so a regenerated oracle that lost (or
    // renamed) every `submits` key would leave this test green while comparing
    // nothing — the same "0 == 0 counts as covered" shape as the replay loop.
    // The cells that carry `submitTurn` inputs are part of the fixture
    // contract, so pin them.
    let mut with_submits = Vec::new();
    for case in oracle["cases"].as_array().expect("#1333 oracle cases") {
        if case.get("submits").is_some() {
            with_submits.push(case["id"].as_str().expect("#1333 case id"));
        }
    }
    assert_eq!(
        with_submits,
        ["buffered", "stream", "new-command"],
        "#1333 the cells that record `submitTurn` inputs changed; \
         `submit_inputs_match_real_ts` must be updated with the oracle"
    );
    for case in oracle["cases"].as_array().expect("#1333 oracle cases") {
        let Some(expected) = case.get("submits") else {
            continue;
        };
        let id = case["id"].as_str().expect("#1333 case id");
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
async fn rejected_new_while_busy_never_reroutes_the_chat() {
    // TS has no await between `activeChats.has` and `mapper.resolve`, so a
    // rejected duplicate can never touch the routing table. Here the admission
    // must be authoritative before `mapper.resolve` runs (#1333 concurrency).
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
    let addr = started.addr;
    let first = tokio::spawn(async move {
        exchange(
            addr,
            &ReqSpec::post(
                "/v1/chat/completions",
                "chat-new",
                "{\"messages\":[{\"role\":\"user\",\"content\":\"/new first\"}]}",
            ),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), latch.wait_started())
        .await
        .expect("#1333 first turn started");
    let busy = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-new",
            "{\"messages\":[{\"role\":\"user\",\"content\":\"/new hijack\"}]}",
        ),
    )
    .await;
    assert_eq!(
        busy.status,
        Some(429),
        "#1333 a duplicate /new while busy is rejected"
    );
    latch.release();
    let first = first.await.expect("#1333 first request task");
    assert_eq!(first.status, Some(200), "#1333 first turn completes");
    let follow = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-new",
            "{\"messages\":[{\"role\":\"user\",\"content\":\"again\"}]}",
        ),
    )
    .await;
    assert_eq!(follow.status, Some(200), "#1333 follow-up completes");
    let calls = gateway.calls.lock().expect("#1333 calls");
    assert_eq!(calls.len(), 2, "#1333 the rejected /new is never submitted");
    assert_eq!(
        calls[0]["sessionKey"], calls[1]["sessionKey"],
        "#1333 a rejected /new must not reroute the chat"
    );
}

#[tokio::test]
async fn falsy_text_parts_match_the_ts_empty_message() {
    // TS returns `item.text` verbatim and `.filter(Boolean)` drops it, so a
    // numeric/bool `text` is an empty part -> 400 empty_message, while a
    // truthy non-string `text` is kept and stringified by `join`.
    let started = start(options(&json!({"options":{}}), None), None).await;
    for (index, literal) in ["0", "false"].into_iter().enumerate() {
        let body = format!(
            "{{\"messages\":[{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":{literal}}}]}}]}}"
        );
        let chat = format!("chat-falsy-{index}");
        let reply = exchange(
            started.addr,
            &ReqSpec::post("/v1/chat/completions", &chat, &body),
        )
        .await;
        assert_eq!(
            reply.status,
            Some(400),
            "#1333 a {literal} text part is an empty message"
        );
        assert!(
            String::from_utf8_lossy(&reply.body).contains("\"code\":\"empty_message\""),
            "#1333 falsy text part error body"
        );
    }
    let body = "{\"messages\":[{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":5}]}]}";
    let reply = exchange(
        started.addr,
        &ReqSpec::post("/v1/chat/completions", "chat-truthy-5", body),
    )
    .await;
    assert_eq!(
        reply.status,
        Some(503),
        "#1333 a truthy non-string text part still becomes user text"
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
    // Deliberately a literal, not `MAX_REQUEST_BYTES + 1`: the `body-limit`
    // negative control rewrites the cap in `mod.rs` to 10,000,000 and expects
    // *this* test to go red. Deriving the body from the cap would move the
    // body with the mutation and let it survive, so the two stay independent
    // and `assert_boundary_body_len` is what ties the fixture to the cap.
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
    // #1606 r2: the TS baseline sends *nothing* back, so a truncated response
    // head before the reset must not pass as "closed with zero bytes".
    assert!(
        response.body.is_empty(),
        "#1333 oversized body must close without sending any bytes, got {}",
        response.body.len()
    );
    assert_ne!(
        response.error.as_deref(),
        Some("timeout"),
        "#1333 oversized body must close promptly, not hang until the client timeout"
    );
    started.handle.stop("done").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn body_at_the_exact_limit_is_read_then_rejected_as_json() {
    // The cap is exclusive (`> MAX_REQUEST_BYTES`, mirroring TS
    // `ApiServerChannel.ts:409` `if (size > max)`), so a body of exactly
    // 1,000,000 bytes is read in full and then fails JSON parsing: 400
    // invalid_json, *not* the zero-byte close. The `body-limit-exact` oracle
    // cell now replays this boundary too (#1606), but keep this test: it is
    // the target the `body-limit-exact` negative-control mutation binds to,
    // and it builds the 1 MB body directly instead of through the oracle's
    // `<N bytes>` placeholder.
    let started = start(options(&json!({"options":{}}), None), None).await;
    let body = "x".repeat(1_000_000);
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
    assert_eq!(
        response.status,
        Some(400),
        "#1333 a body of exactly MAX_REQUEST_BYTES is read, got error {:?}",
        response.error
    );
    assert!(
        String::from_utf8_lossy(&response.body).contains("\"code\":\"invalid_json\""),
        "#1333 a body of exactly MAX_REQUEST_BYTES fails as invalid_json, got {:?}",
        String::from_utf8_lossy(&response.body)
    );
    started.handle.stop("done").await.unwrap();
}

/// #1620: a client that declares `Content-Length` and then stops sending —
/// without closing — used to park `read_body` forever, and `stop()` drains the
/// connection tasks, so it hung too. Node's `requestTimeout` answers at the
/// deadline (webhook lane: `shutdown_with_partial_body_finishes_at_request_timeout`);
/// this pins the api-server lane to the same behavior with a shortened,
/// injected deadline. This is the test the `api-server-body-read-no-timeout`
/// negative control removes the deadline from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_body_is_answered_at_the_body_read_deadline() {
    const DEADLINE: Duration = Duration::from_secs(5);
    let mut case_options = options(&json!({"options":{}}), None);
    case_options.body_read_timeout = Some(Duration::from_millis(200));
    let started = start(case_options, None).await;
    let port = started.addr.port();
    let handle = started.handle.clone();
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1333 connect to the stalled-body listener");
    stream
        .write_all(
            b"POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\n\
             Content-Length: 64\r\nConnection: keep-alive\r\n\r\nxxxxxxxx",
        )
        .await
        .expect("#1333 send the partial body");
    // Start `stop()` while the body is still stalled: without the deadline the
    // connection task never finishes and this join never resolves.
    let stopping = tokio::spawn(async move { handle.stop("partial-body").await });
    let mut response = Vec::new();
    tokio::time::timeout(DEADLINE, stream.read_to_end(&mut response))
        .await
        .expect("#1333 a stalled body is answered at the body-read deadline")
        .expect("#1333 read the stalled-body response bytes");
    let (end, status, headers) =
        parse_head(&response).expect("#1333 the stalled-body answer carries a response head");
    assert_eq!(
        status, 408,
        "#1333 a stalled body is answered with Node's requestTimeout 408"
    );
    assert_eq!(
        headers.get("connection").map(String::as_str),
        Some("close"),
        "#1333 the requestTimeout 408 closes the connection"
    );
    // Node's `requestTimeoutResponse` is 51 bytes with no `Date` line, and the
    // registered divergence specs (A8.3-API-SERVER-003-R1 / -004) state that
    // the Rust answer must not add one. `handle` skips `finish` (the only
    // place this lane adds `Date`) exactly for that reason, so pin its absence:
    // without this the whole `BodyReadTimeout` marker could be simplified away
    // and every test would stay green while the header regressed.
    assert!(
        !headers.contains_key("date"),
        "#1333 the requestTimeout 408 must not gain a Date header, got {:?}",
        headers
    );
    // Compare the raw framing, not `dechunk(..).is_empty()`: `dechunk` returns
    // an empty vector for anything that does not start with a valid hex
    // chunk-size line, so "could not de-frame" would pass as "empty body"
    // (GLM boundary P2). Node's 51 bytes carry no body and hyper frames an
    // unknown-length empty body as the bare terminator, which
    // `A8.3-API-SERVER-004` registers as this lane's on-the-wire shape.
    assert_eq!(
        &response[end..],
        b"0\r\n\r\n".as_slice(),
        "#1333 the requestTimeout 408 body is the empty chunked terminator, got {:?}",
        String::from_utf8_lossy(&response[end..])
    );
    tokio::time::timeout(DEADLINE, stopping)
        .await
        .expect("#1333 stop finishes after the body-read deadline")
        .expect("#1333 stop join")
        .expect("#1333 stop ok after the body-read deadline");
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

#[test]
fn render_visible_failure_status_names_match_ts() {
    // `isVisibleFailureAgentStatusEvent` (api-server-render.ts cc7c4e6) treats a
    // listed `VISIBLE_FAILURE_STATUS_EVENTS` name as visible even when its
    // `detail` is not error-severity. Every `agent_status` detail the oracle
    // streams is error-severity, so `replays_real_ts_api_server_cells` would
    // stay green if the name list were emptied; this pins the name branch.
    fn render_json(value: Value) -> Option<String> {
        let event = pilotdeck_protocol::parse_json(&value.to_string()).expect("#1333 event JSON");
        render::render(&event).map(|text| text.to_utf8().expect("#1333 rendered text"))
    }

    assert_eq!(
        render_json(json!({
            "type": "agent_status",
            "event": "turn_failed",
            "detail": {"message": "boom"}
        })),
        Some("\nError: boom\n".to_owned()),
        "#1333 a listed status name is visible without an error severity"
    );
    assert_eq!(
        render_json(json!({
            "type": "agent_status",
            "event": "some_other_event",
            "detail": {"message": "boom"}
        })),
        None,
        "#1333 an unlisted status name stays hidden"
    );
    assert_eq!(
        render_json(json!({
            "type": "agent_status",
            "event": "turn_failed",
            "detail": {"visible": false}
        })),
        None,
        "#1333 explicit visible:false still hides a listed name"
    );
}

#[test]
fn admit_chat_claims_a_chat_id_exactly_once() {
    // A `contains` check followed by a later `insert` lets two requests with the
    // same chat id both pass; the admission must be one atomic insert, so exactly
    // one of the racing callers wins (#1333 concurrency review).
    use std::collections::HashSet;
    let active = Arc::new(Mutex::new(HashSet::new()));
    let winners = Arc::new(Mutex::new(0usize));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let active = active.clone();
        let winners = winners.clone();
        workers.push(std::thread::spawn(move || {
            if server::admit_chat(&active, "chat-race") {
                *winners.lock().expect("#1333 winners") += 1;
            }
        }));
    }
    for worker in workers {
        worker.join().expect("#1333 admission thread");
    }
    assert_eq!(
        *winners.lock().expect("#1333 winners"),
        1,
        "#1333 concurrent duplicates must admit exactly one request"
    );
    assert!(
        !server::admit_chat(&active, "chat-race"),
        "#1333 an admitted chat stays busy until it is released"
    );
    active.lock().expect("#1333 active").remove("chat-race");
    assert!(
        server::admit_chat(&active, "chat-race"),
        "#1333 a released chat id is reusable"
    );
}

#[test]
fn body_read_deadline_defaults_to_the_node_request_timeout() {
    // The baseline calls `http.createServer(...)` without setting
    // `server.requestTimeout`, so Node v22.22.0's default 300 s is the whole
    // contract (#1620). Nothing else pins the constant: the Rust test injects
    // a 200 ms deadline and the oracle replay skips this cell, so the default
    // could drift to 30 s or 1 h with every check still green.
    assert_eq!(
        DEFAULT_BODY_READ_TIMEOUT,
        Duration::from_secs(300),
        "#1620 the body-read deadline must stay at Node's default requestTimeout"
    );
    assert_eq!(
        ApiServerOptions::default().body_read_timeout,
        None,
        "#1620 an unset option means the Node default, not a shorter deadline"
    );
}

#[test]
fn unconfigured_port_falls_back_to_the_ts_default() {
    // TS `Number(options.port ?? env.API_SERVER_PORT ?? DEFAULT_PORT)`: a port
    // always resolves, so an adapter with no configuration still binds 8642.
    // Resolving to "no port" left the channel silently unbound (#1333 boundary
    // review: DEFAULT_PORT was declared but never read).
    let channel = ApiServerChannel::with_env(ApiServerOptions::default(), HashMap::new());
    assert_eq!(
        channel.resolve(None).port,
        DEFAULT_PORT,
        "#1333 an unconfigured port must default to 8642"
    );

    let explicit = ApiServerOptions {
        port: Some(JsValue::Number(0.0)),
        ..ApiServerOptions::default()
    };
    assert_eq!(
        ApiServerChannel::with_env(explicit, HashMap::new())
            .resolve(None)
            .port,
        0.0,
        "#1333 an explicit port still wins"
    );

    let env = HashMap::from([("API_SERVER_PORT".to_owned(), "9100".to_owned())]);
    assert_eq!(
        ApiServerChannel::with_env(ApiServerOptions::default(), env)
            .resolve(None)
            .port,
        9100.0,
        "#1333 the env port wins over the default"
    );
}

/// #1737: `stream_turn`'s submit-rejection arm — the SSE head is already on
/// the wire, so the client must still get a `channel_submit_failed` frame
/// (not a hang, not `[DONE]`), the chat guard must be released, and the
/// submit is not retried. This is the test the `stream-submit-err-arm`
/// negative control binds to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_error_stream_turn_emits_channel_submit_failed() {
    let gateway = Arc::new(FailingGateway {
        calls: AtomicUsize::new(0),
    });
    let started = start(
        options(&json!({"options": {}}), None),
        Some(gateway.clone() as Arc<dyn TurnGateway>),
    )
    .await;
    let response = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-submit-err-stream",
            "{\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
        ),
    )
    .await;
    // The SSE head is sent before `submit` runs (by design), so a rejected
    // submit still streams — a 500 here would mean the head moved after submit.
    assert_eq!(
        response.status,
        Some(200),
        "#1737 a rejected stream submit keeps the already-sent SSE head, got {:?}",
        response.status
    );
    assert_eq!(
        response.headers.get("content-type").map(String::as_str),
        Some("text/event-stream"),
        "#1737 a rejected stream submit still answers as SSE, got {:?}",
        response.headers
    );
    let body = String::from_utf8_lossy(&response.body);
    assert!(
        body.contains("data: {"),
        "#1737 the rejected submit frames its error as SSE data, got {body:?}"
    );
    assert!(
        body.contains("\"event\":\"channel_submit_failed\""),
        "#1737 a rejected stream submit emits channel_submit_failed, got {body:?}"
    );
    assert!(
        // The trailing comma pins `error.status` (the last `detail.status` has
        // `}` after it) and keeps `"status":5000` from matching the prefix.
        body.contains("\"status\":500,"),
        "#1737 the channel_submit_failed frame carries status 500, got {body:?}"
    );
    assert!(
        body.contains("\"scope\":\"channel\""),
        "#1737 the channel_submit_failed frame carries scope channel, got {body:?}"
    );
    assert!(
        body.contains("\"type\":\"server_error\""),
        "#1737 the channel_submit_failed frame carries type server_error, got {body:?}"
    );
    assert!(
        !body.contains("[DONE]"),
        "#1737 a rejected submit must not close the stream with [DONE], got {body:?}"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !started
            .channel
            .0
            .active
            .lock()
            .expect("#1737 active chats")
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1737 a rejected submit releases the active chat guard");
    assert_eq!(
        gateway.calls.load(Ordering::SeqCst),
        1,
        "#1737 a rejected stream submit is not retried"
    );
    started
        .handle
        .stop("submit-error-stream")
        .await
        .expect("#1737 stop after the submit-error stream");
}

/// #1737: `buffered_turn`'s submit-rejection arm — the 500 `submit_failed`
/// body (`error` + `agent_status`, `error_body.rs` field set), the released
/// chat guard and the single submit. This is the test the
/// `buffered-submit-err-arm` negative control binds to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_error_buffered_turn_returns_submit_failed() {
    let gateway = Arc::new(FailingGateway {
        calls: AtomicUsize::new(0),
    });
    let started = start(
        options(&json!({"options": {}}), None),
        Some(gateway.clone() as Arc<dyn TurnGateway>),
    )
    .await;
    let response = exchange(
        started.addr,
        &ReqSpec::post(
            "/v1/chat/completions",
            "chat-submit-err-buffered",
            "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
        ),
    )
    .await;
    assert_eq!(
        response.status,
        Some(500),
        "#1737 a rejected buffered submit answers 500, got error {:?}",
        response.error
    );
    let body: Value = serde_json::from_slice(&response.body).unwrap_or_else(|error| {
        panic!(
            "#1737 the submit_failed body is JSON: {error}, got {:?}",
            String::from_utf8_lossy(&response.body)
        )
    });
    assert_eq!(
        body["error"]["code"],
        json!("channel_submit_failed"),
        "#1737 the submit_failed error code is channel_submit_failed, got {body}"
    );
    assert_eq!(
        body["error"]["event"],
        json!("channel_submit_failed"),
        "#1737 the submit_failed error event is channel_submit_failed, got {body}"
    );
    assert_eq!(
        body["error"]["status"],
        json!(500),
        "#1737 the submit_failed error status is 500, got {body}"
    );
    assert_eq!(
        body["error"]["scope"],
        json!("channel"),
        "#1737 the submit_failed error scope is channel, got {body}"
    );
    assert_eq!(
        body["error"]["type"],
        json!("server_error"),
        "#1737 the submit_failed error type is server_error, got {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("#1737 scripted submit rejection"),
        "#1737 the submit_failed error message carries the rejection, got {body}"
    );
    assert_eq!(
        body["agent_status"]["event"],
        json!("channel_submit_failed"),
        "#1737 the submit_failed agent_status event is channel_submit_failed, got {body}"
    );
    assert_eq!(
        body["agent_status"]["detail"]["code"],
        json!("channel_submit_failed"),
        "#1737 the submit_failed agent_status detail code is channel_submit_failed, got {body}"
    );
    assert_eq!(
        body["agent_status"]["detail"]["status"],
        json!(500),
        "#1737 the submit_failed agent_status detail status is 500, got {body}"
    );
    assert_eq!(
        body["agent_status"]["detail"]["scope"],
        json!("channel"),
        "#1737 the submit_failed agent_status detail scope is channel, got {body}"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !started
            .channel
            .0
            .active
            .lock()
            .expect("#1737 active chats")
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#1737 a rejected buffered submit releases the active chat guard");
    assert_eq!(
        gateway.calls.load(Ordering::SeqCst),
        1,
        "#1737 a rejected buffered submit is not retried"
    );
    started
        .handle
        .stop("submit-error-buffered")
        .await
        .expect("#1737 stop after the submit-error buffered turn");
}

#[test]
fn port_numbers_follow_the_es_string_numeric_literal() {
    // TS `Number(value)` (ApiServerChannel.ts:51): a sign may only precede a
    // decimal literal or `Infinity`, so `Number("+0x10")` is NaN and
    // `server.listen(NaN)` fails the start. A lone surrogate cannot be spelled
    // in UTF-8 and is NaN too, never `Number("") = 0` (which would bind an
    // ephemeral port). #1333 boundary review.
    let number = |text: &str| server::js_number(&JsValue::String(JsString::from(text)));
    assert_eq!(number(" 0x10 "), 16.0, "#1333 unsigned hex still parses");
    assert_eq!(number("0b101"), 5.0, "#1333 unsigned binary still parses");
    assert_eq!(number(""), 0.0, "#1333 the empty string is zero");
    assert!(
        number("+0x10").is_nan(),
        "#1333 a signed hex literal is NaN"
    );
    assert!(
        number("-0xFF").is_nan(),
        "#1333 a negative hex literal is NaN"
    );
    assert_eq!(number("-Infinity"), f64::NEG_INFINITY);
    let lone = JsString::from_utf16(vec![0xd800]);
    assert!(
        server::js_number(&JsValue::String(lone)).is_nan(),
        "#1333 a lone-surrogate port string is NaN, not the empty string"
    );
}
