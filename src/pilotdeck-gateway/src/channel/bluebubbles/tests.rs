//! #1343: replay of the `channel-bluebubbles` golden fixture — the same step
//! scripts the TS generator ran against the real baseline channel (Node
//! v22.22.0, baseline cc7c4e6; no SDK, no stub, the channel's own global
//! `fetch` against a loopback mock), driven through the shared hyper client
//! against a loopback BlueBubbles fake with per-request scripted responses
//! and a scripted A7 gateway plus a scripted interaction responder — plus
//! the URL/serializer component pins.
//!
//! Registered diffs this replay is shaped by (gateway-parity ledger):
//! 002 production assembly installs `no_interaction`; the replay installs
//!     the real helper-backed `HelperInteraction` with a scripted responder,
//!     so the elicitation/permission cells still exercise the ported state
//!     machines;
//! 003 `stop` does not await in-flight turns (#1353 owns the drain entry):
//!     the stop-drains case pins the TS shape — the held turn finishes after
//!     stop and its reply is gated by `running === false` posting nothing;
//!     003-R1 corrects the Rust-side half: stop also aborts and bounded-reaps
//!     the driver task with two new `error` log paths, still without awaiting
//!     in-flight turns;
//! 004 a same-guid row arriving after a restart re-dispatches channel-side;
//!     the restart-clears-guids case records both dispatches, and the
//!     never-re-executes case pins the dedupe inside one epoch;
//! 005 a non-empty but invalid `serverUrl` logs and keeps polling instead of
//!     the TS unhandled rejection — no replay case (the fake's URL is always
//!     well-formed), the `Invalid URL` string is a component pin below;
//! 007 the #2285 deviations (drop-without-stop ends the driver once no poll
//!     is in flight — the silent-server in-flight window matches TS's
//!     leaked interval, 007's registered gap — and a panicking per-tick poll
//!     task is swallowed without the TS `poll error` log) do not shape this
//!     replay; their pins live in the #2285 section below.
use super::super::protocol::{
    BoxFuture, ChannelError, ChannelHandle, ChannelStartDeps, TurnEventStream, TurnGateway,
    TurnRequest,
};
use super::*;
use crate::channel::http_client::HttpClient;
use crate::test_baseline;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::service::service_fn;
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpListener, time::timeout};

const ORACLE: &str = include_str!("../../../tests/fixtures/channel-bluebubbles/oracle.json");
/// The loopback BlueBubbles fake the generator ran on (`MOCK_PORT` in
/// `generate.mts`); every case's `serverUrl` points here. The block sits
/// outside feishu's 18960-18999 and weixin's 18800-18921 reservations.
const MOCK_PORT: u16 = 19461;
/// Every blocking step is bounded so a regression fails instead of hanging
/// the suite; one poll interval is 2500 ms and no case waits out more than
/// two of them (the generator's own bound was 10 s).
const DEADLINE: Duration = Duration::from_secs(20);
/// The generator's post-case quiescence window (two `ticks(50)` rounds).
const SETTLE: Duration = Duration::from_millis(50);
/// The generator's default scripted responses, used once a queue runs dry.
const DEFAULT_POLL: &str = "{}";
const DEFAULT_SEND: &str = "{\"sent\":true}";

fn oracle() -> Value {
    test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1343 oracle JSON")
}

fn cases(oracle: &Value) -> &Vec<Value> {
    let cases = oracle["cases"].as_array().expect("#1343 oracle cases");
    crate::negative_assert!(!cases.is_empty(), "#1343 oracle cases non-empty");
    crate::negative_assert_eq!(
        cases.len() as u64,
        oracle["expectedCaseCount"]
            .as_u64()
            .expect("#1343 expectedCaseCount"),
        "#1343 oracle case count matches its declaration"
    );
    // The declaration alone is not a ruler: dropping a case and lowering
    // `expectedCaseCount` together would keep both assertions above green.
    // The generator's fixed count is pinned separately
    // (`EXPECTED_CASE_COUNT` in `generate.mts`), so coverage cannot shrink
    // silently.
    crate::negative_assert_eq!(
        cases.len() as u64,
        16,
        "#1343 oracle holds the generator's 16 cases"
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
            &format!("#1343 {context} matches the TS oracle"),
        ),
    }
}

/// Wait for `predicate`, emitting the receipt of the cell this wait stands in
/// for when the deadline passes (`assertion` must be a message a mutation row
/// can declare; `detail` only reaches stderr).
async fn until(assertion: &str, detail: &str, predicate: impl Fn() -> bool) {
    let started = Instant::now();
    while !predicate() {
        if started.elapsed() >= DEADLINE {
            eprintln!("#1343 timeout waiting for {detail}");
        }
        crate::negative_assert!(started.elapsed() < DEADLINE, "{assertion}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// The generator's `maskDynamic`: a UUID-shaped hex run becomes `<uuid>` (no
/// word boundaries — it must match inside the mapper's `s_<uuid>` session
/// keys). Same leftmost scan as the regex: a longer hex run still matches one
/// position in.
fn mask_dynamic(text: &str) -> String {
    let bytes = text.as_bytes();
    let hex = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
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

/// The generator's `maskQuery`: an `after=<digits>` within ±300 s of the
/// clock is start-seeded (it varies run to run); the scripted timestamps sit
/// in the 1600000000 / 1900000000 ranges and never trip the rule.
fn mask_query(query: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("#1343 unix epoch")
        .as_secs_f64();
    let bytes = query.as_bytes();
    let mut out = String::new();
    let mut last = 0usize;
    let mut i = 0usize;
    while i + 6 <= bytes.len() {
        if &query[i..i + 6] == "after=" {
            let digits = (i + 6..bytes.len())
                .take_while(|p| bytes[*p].is_ascii_digit())
                .count();
            let value: f64 = query[i + 6..i + 6 + digits].parse().unwrap_or(f64::NAN);
            if digits > 0 && (value - now).abs() <= 300.0 {
                out.push_str(&query[last..i + 6]);
                out.push_str("<now-s>");
                last = i + 6 + digits;
                i = last;
                continue;
            }
            i += 6 + digits.max(1);
            continue;
        }
        i += 1;
    }
    out.push_str(&query[last..]);
    out
}

// ---------------------------------------------------------------- recording

/// One case's six projections plus the two request counters the waits and the
/// scripted-served checks read (the counters are not part of the snapshot).
#[derive(Default)]
struct Record {
    polls: usize,
    sends: usize,
    poll_queries: Vec<Value>,
    sends_out: Vec<Value>,
    gateway: Vec<Value>,
    responds: Vec<Value>,
    decides: Vec<Value>,
    logs: Vec<Value>,
}

impl Record {
    fn snapshot(&self) -> Value {
        json!({
            "pollQueries": self.poll_queries,
            "sends": self.sends_out,
            "gateway": self.gateway,
            "responds": self.responds,
            "decides": self.decides,
            "logs": self.logs,
        })
    }
}

type Gate = Arc<tokio::sync::watch::Sender<bool>>;
type Gates = Arc<Mutex<HashMap<String, Gate>>>;

fn gate(gates: &Gates, name: &str) -> Gate {
    gates
        .lock()
        .expect("#1343 gate mutex")
        .entry(name.to_owned())
        .or_insert_with(|| Arc::new(tokio::sync::watch::Sender::new(false)))
        .clone()
}

async fn released(gate: Gate) {
    let mut rx = gate.subscribe();
    while !*rx.borrow_and_update() {
        rx.changed().await.expect("#1343 gate sender retained");
    }
}

#[derive(Clone, Default)]
struct TurnScript {
    gate_before: Option<String>,
    events: Vec<Value>,
    hold_for_respond: bool,
    resume: Vec<Value>,
    error: Option<String>,
}

/// The A7 `submitTurn` fake: records the three TS inputs, streams the
/// scripted events (optionally behind a gate, optionally holding for the
/// user's respond) and finally the scripted error or the `turnEnd` marker.
struct MockInbound {
    turns: Vec<TurnScript>,
    next: Mutex<usize>,
    record: Arc<Mutex<Record>>,
    gates: Gates,
    responded: Gate,
    /// Counts `submit` entries — bumped before the turn-index lock is
    /// taken, so a dispatch that panics on that (poisoned) lock still lands
    /// here (#2285 r2: an empty gateway projection alone cannot tell
    /// "panicked inside submit" from "never dispatched").
    submit_attempts: std::sync::atomic::AtomicUsize,
}

/// Adapts the scripted `mpsc` receiver to the shared `TurnEventStream`; the
/// generator's `turnEnd` marker is exactly the stream completing without an
/// error (the error arm never records one, matching the oracle).
struct MockStream {
    receiver: tokio::sync::mpsc::UnboundedReceiver<Result<JsValue, ChannelError>>,
    record: Arc<Mutex<Record>>,
}

impl TurnEventStream for MockStream {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move {
            match self.receiver.recv().await {
                Some(Ok(event)) => Ok(Some(event)),
                Some(Err(error)) => Err(error),
                None => {
                    self.record
                        .lock()
                        .expect("#1343 record mutex")
                        .gateway
                        .push(json!({ "turnEnd": true }));
                    Ok(None)
                }
            }
        })
    }
}

impl MockInbound {
    fn new(
        turns: Vec<TurnScript>,
        record: Arc<Mutex<Record>>,
        gates: Gates,
        responded: Gate,
    ) -> Arc<Self> {
        Arc::new(Self {
            turns,
            next: Mutex::new(0),
            record,
            gates,
            responded,
            submit_attempts: std::sync::atomic::AtomicUsize::new(0),
        })
    }
}

impl TurnGateway for MockInbound {
    fn submit(
        &self,
        input: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        // Before the turn-index lock: a poisoned dispatch must still count
        // as having reached `submit` (#2285 r2).
        self.submit_attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let turn = {
            let mut next = self.next.lock().expect("#1343 turn index");
            let index = *next;
            *next += 1;
            self.turns.get(index).cloned().unwrap_or_default()
        };
        self.record
            .lock()
            .expect("#1343 record mutex")
            .gateway
            .push(json!({
                "sessionKey": mask_dynamic(&String::from_utf16_lossy(input.session_key.units())),
                "channelKey": input.channel_key.to_string(),
                "message": String::from_utf16_lossy(input.message.units()),
            }));
        let gates = self.gates.clone();
        let responded = self.responded.clone();
        let (events, stream) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            if let Some(name) = turn.gate_before {
                released(gate(&gates, &name)).await;
            }
            for event in turn.events {
                let _ = events.send(Ok(parse_json(&event.to_string()).expect("#1343 event JSON")));
            }
            if turn.hold_for_respond {
                released(responded).await;
            }
            for event in turn.resume {
                let _ = events.send(Ok(parse_json(&event.to_string()).expect("#1343 event JSON")));
            }
            if let Some(error) = turn.error {
                let _ = events.send(Err(ChannelError::error(error)));
            }
        });
        let record = self.record.clone();
        Box::pin(async move {
            Ok(Box::new(MockStream {
                receiver: stream,
                record,
            }) as Box<dyn TurnEventStream>)
        })
    }
}

// ------------------------------------------------------- interaction responder

/// The `respondElicitation` / `permissionDecide` fake: records the payload as
/// the same `JSON.stringify` text the generator recorded and releases the
/// turn's hold-for-respond gate. `permissionDecide` answers `delivered: true`
/// exactly like the generator's fake.
struct MockResponder {
    record: Arc<Mutex<Record>>,
    responded: Gate,
}

fn payload_text(value: &pilotdeck_protocol::JsObject) -> String {
    stringify_json(&JsValue::Object(value.clone()))
        .expect("#1343 respond payload serialization")
        .unwrap_or_default()
}

impl InteractionResponder for MockResponder {
    fn respond_elicitation(
        &self,
        response: crate::channel::sms::ElicitationResponse,
    ) -> BoxFuture<Result<(), ChannelError>> {
        self.record
            .lock()
            .expect("#1343 record mutex")
            .responds
            .push(Value::String(payload_text(&response.to_js())));
        self.responded.send_replace(true);
        Box::pin(async { Ok(()) })
    }

    fn permission_decide(
        &self,
        decision: crate::channel::sms::PermissionDecision,
    ) -> BoxFuture<Result<crate::channel::sms::PermissionDecisionOutcome, ChannelError>> {
        self.record
            .lock()
            .expect("#1343 record mutex")
            .decides
            .push(Value::String(payload_text(&decision.to_js())));
        self.responded.send_replace(true);
        Box::pin(
            async move { Ok(crate::channel::sms::PermissionDecisionOutcome { delivered: true }) },
        )
    }
}

// ------------------------------------------------------- loopback server fake

/// The loopback BlueBubbles server: one listener for the whole replay, armed
/// per case (a disarmed request is a stray), per-method index-scripted
/// responses (the i-th GET/POST gets slot `i`, the defaults once dry), the
/// deduplicated poll-query projection and the send records.
struct Mock {
    armed: Mutex<Option<Arc<Mutex<Record>>>>,
    scripted_polls: Mutex<Vec<(u16, String)>>,
    scripted_sends: Mutex<Vec<(u16, String)>>,
    /// (case epoch, request) — the epoch is stamped when the server task
    /// reads the request, so a poll leaking past its case's end is charged
    /// to that case by name instead of being silently cleared by the next
    /// `arm()` (#2285 r2 ④; `arm()` increments the epoch and no longer
    /// clears).
    strays: Mutex<Vec<(u64, String)>>,
    epoch: std::sync::atomic::AtomicU64,
}

impl Mock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            armed: Mutex::new(None),
            scripted_polls: Mutex::new(Vec::new()),
            scripted_sends: Mutex::new(Vec::new()),
            strays: Mutex::new(Vec::new()),
            epoch: std::sync::atomic::AtomicU64::new(0),
        })
    }

    fn scripted(case: &Value, kind: &str) -> Vec<(u16, String)> {
        case["scripted"][kind]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        (
                            entry["status"].as_u64().expect("#1343 scripted status") as u16,
                            entry["json"].to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Arms the mock for one case and returns that case's epoch. The epoch
    /// increments here; strays are never cleared anymore — a leftover from
    /// an earlier case must fail the case that observes it, not vanish
    /// (#2285 r2 ④: the `arm()` clear traded r1's false FAIL for a silent
    /// false-PASS window that nothing tested).
    fn arm(&self, case: &Value, record: Arc<Mutex<Record>>) -> u64 {
        let epoch = self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        *self.armed.lock().expect("#1343 armed mutex") = Some(record);
        *self.scripted_polls.lock().expect("#1343 poll script") = Self::scripted(case, "polls");
        *self.scripted_sends.lock().expect("#1343 send script") = Self::scripted(case, "sends");
        epoch
    }

    fn stray(&self, text: String) {
        self.strays
            .lock()
            .expect("#1343 stray mutex")
            .push((self.epoch.load(std::sync::atomic::Ordering::SeqCst), text));
    }

    fn disarm(&self) {
        *self.armed.lock().expect("#1343 armed mutex") = None;
    }
}

async fn start_mock_on(mock: Arc<Mock>, port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("#1343 mock bind");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let mock = mock.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let mock = mock.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let bytes = match body.collect().await {
                            Ok(collected) => collected.to_bytes(),
                            Err(_) => Bytes::new(),
                        };
                        let method = parts.method.as_str().to_owned();
                        let path = parts.uri.path().to_owned();
                        let query = parts
                            .uri
                            .path_and_query()
                            .and_then(|pq| pq.query())
                            .map(str::to_owned);
                        let respond = |status: u16, payload: &str| {
                            hyper::Response::builder()
                                .status(
                                    hyper::StatusCode::from_u16(status).expect("#1343 mock status"),
                                )
                                .header(hyper::header::CONTENT_TYPE, "application/json")
                                .body(Full::new(Bytes::from(payload.to_owned())))
                                .expect("#1343 mock response")
                        };
                        let record = mock.armed.lock().expect("#1343 armed mutex").clone();
                        let Some(record) = record else {
                            mock.stray(format!("{method} {path}"));
                            return Ok::<_, std::io::Error>(respond(200, "{}"));
                        };
                        if method == "GET" && path == "/api/v1/message" {
                            let masked = mask_query(query.as_deref().unwrap_or(""));
                            let index = {
                                let mut record = record.lock().expect("#1343 record mutex");
                                if !record
                                    .poll_queries
                                    .iter()
                                    .any(|seen| seen.as_str() == Some(masked.as_str()))
                                {
                                    record.poll_queries.push(Value::String(masked.clone()));
                                }
                                let index = record.polls;
                                record.polls += 1;
                                index
                            };
                            let next = mock
                                .scripted_polls
                                .lock()
                                .expect("#1343 poll script")
                                .get(index)
                                .cloned()
                                .unwrap_or((200, DEFAULT_POLL.to_owned()));
                            return Ok(respond(next.0, &next.1));
                        }
                        if method == "POST" && path == "/api/v1/message/text" {
                            let index = {
                                let mut record = record.lock().expect("#1343 record mutex");
                                record.sends_out.push(json!({
                                    "method": method,
                                    "path": path,
                                    "auth": parts
                                        .headers
                                        .get(hyper::header::AUTHORIZATION)
                                        .and_then(|value| value.to_str().ok())
                                        .map(str::to_owned),
                                    "contentType": parts
                                        .headers
                                        .get(hyper::header::CONTENT_TYPE)
                                        .and_then(|value| value.to_str().ok())
                                        .map(str::to_owned),
                                    "body": mask_dynamic(&String::from_utf8_lossy(&bytes)),
                                }));
                                let index = record.sends;
                                record.sends += 1;
                                index
                            };
                            let next = mock
                                .scripted_sends
                                .lock()
                                .expect("#1343 send script")
                                .get(index)
                                .cloned()
                                .unwrap_or((200, DEFAULT_SEND.to_owned()));
                            return Ok(respond(next.0, &next.1));
                        }
                        // An armed request off the two paths the channel
                        // ever uses is a divergence the strays check
                        // reports loudly (the generator's handler asserts
                        // instead, failing its own run).
                        mock.stray(format!("unexpected {method} {path}"));
                        Ok(respond(200, "{}"))
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
            });
        }
    });
}

// ---------------------------------------------------------------- replay

fn turn_scripts(case: &Value) -> Vec<TurnScript> {
    case["turns"]
        .as_array()
        .map(|turns| {
            turns
                .iter()
                .map(|turn| TurnScript {
                    gate_before: turn["gateBefore"].as_str().map(str::to_owned),
                    events: turn["events"].as_array().cloned().unwrap_or_default(),
                    hold_for_respond: turn["holdForRespond"].as_bool().unwrap_or(false),
                    resume: turn["resume"].as_array().cloned().unwrap_or_default(),
                    error: turn["error"].as_str().map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The generator's post-case quiescence window (`ticks(50)`), made
/// self-extending for the disarm drain (#2285 r2 ⑤, fix-r1 boundary #2): a
/// single silent 50 ms round no longer ends the drain — a loopback peer
/// starved past the first window may still be holding in-flight bytes, and
/// that first-window exit was exactly the r1 false-FAIL shape. The drain now
/// leaves only after two consecutive silent rounds, and repeats while the
/// arrival counters still move, up to six rounds (300 ms), so bytes a slow
/// or loaded peer has already written still land inside the case's own
/// armed window instead of turning into a late stray. Still a heuristic
/// (two starved windows in a row still cut it off); polls beyond the
/// scripted slots only ever draw the dry default and stay invisible in the
/// deduplicated `pollQueries` projection, so the extra rounds cannot change
/// a green snapshot; the whole wait stays far inside `DEADLINE`. The
/// termination semantics are pinned by
/// `drained_holds_through_a_single_silent_window` below.
async fn drained(record: &Arc<Mutex<Record>>) {
    let snapshot = |record: &Arc<Mutex<Record>>| {
        let record = record.lock().expect("#1343 record mutex");
        (
            record.polls,
            record.sends,
            record.gateway.len(),
            record.responds.len(),
            record.decides.len(),
        )
    };
    let mut last = snapshot(record);
    let mut quiet = 0usize;
    for _ in 0..6 {
        tokio::time::sleep(SETTLE).await;
        let now = snapshot(record);
        if now == last {
            quiet += 1;
            if quiet >= 2 {
                break;
            }
        } else {
            quiet = 0;
            last = now;
        }
    }
}

async fn replay(case: &Value, mock: &Arc<Mock>) -> Value {
    let record = Arc::new(Mutex::new(Record::default()));
    let gates = Gates::default();
    // The respond gate the held turns wait on and the responder releases.
    let responded: Gate = Arc::new(tokio::sync::watch::Sender::new(false));
    let name = case["name"].as_str().expect("#1343 case name");
    let context = format!("case {name}");
    let case_epoch = mock.arm(case, record.clone());

    let responder = Arc::new(MockResponder {
        record: record.clone(),
        responded: responded.clone(),
    });
    let gateway = MockInbound::new(turn_scripts(case), record.clone(), gates.clone(), responded);
    let server_url = case["serverUrl"].as_str().map(JsString::from);
    let password = JsString::from(case["password"].as_str().expect("#1343 case password"));
    let make_channel = || {
        BlueBubblesChannel::new(
            BlueBubblesOptions {
                server_url: server_url.clone(),
                password: Some(password.clone()),
                mapper: None,
                env: None,
            },
            Arc::new(HelperInteraction::new(responder.clone())),
        )
    };
    let logger = record.clone();
    let deps = ChannelStartDeps {
        config: None,
        report_status: None,
        log: Some(Arc::new(move |level, message| {
            logger
                .lock()
                .expect("#1343 record mutex")
                .logs
                .push(json!([level, mask_dynamic(message)]));
        })),
        turn_gateway: Some(gateway),
    };

    let mut channel = make_channel();
    let mut handle: Option<Arc<dyn ChannelHandle>> = None;

    for step in case["steps"].as_array().expect("#1343 case steps") {
        match step["op"].as_str().expect("#1343 step op") {
            "start" => {
                // `fresh` is the hot-reload shape: a new instance with its
                // own mapper and helper state, started after the old one
                // stopped.
                if step["fresh"].as_bool().unwrap_or(false) {
                    channel = make_channel();
                }
                handle = Some(
                    bounded(&context, channel.start(deps.clone()))
                        .await
                        .expect("#1343 start"),
                );
            }
            "stop" => {
                let reason = step["reason"].as_str().expect("#1343 stop reason");
                bounded(
                    &context,
                    handle.as_ref().expect("#1343 started").stop(reason),
                )
                .await
                .expect("#1343 stop result");
            }
            "release" => {
                gate(&gates, step["gate"].as_str().expect("#1343 gate name")).send_replace(true);
            }
            "wait" => {
                // The wait stands in for this case's cell comparison, so a
                // timeout must emit exactly that cell's receipt.
                let cell = format!("#1343 case {name} matches the TS oracle");
                if let Some(count) = step["polls"].as_u64() {
                    let record = record.clone();
                    until(&cell, &format!("{context} polls {count}"), move || {
                        record.lock().expect("#1343 record mutex").polls >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["sends"].as_u64() {
                    let record = record.clone();
                    until(&cell, &format!("{context} sends {count}"), move || {
                        record.lock().expect("#1343 record mutex").sends >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["gateway"].as_u64() {
                    let record = record.clone();
                    until(&cell, &format!("{context} gateway {count}"), move || {
                        record.lock().expect("#1343 record mutex").gateway.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["responds"].as_u64() {
                    let record = record.clone();
                    until(&cell, &format!("{context} responds {count}"), move || {
                        record.lock().expect("#1343 record mutex").responds.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["decides"].as_u64() {
                    let record = record.clone();
                    until(&cell, &format!("{context} decides {count}"), move || {
                        record.lock().expect("#1343 record mutex").decides.len() >= count as usize
                    })
                    .await;
                } else if let Some(count) = step["logs"].as_u64() {
                    let record = record.clone();
                    until(&cell, &format!("{context} logs {count}"), move || {
                        record.lock().expect("#1343 record mutex").logs.len() >= count as usize
                    })
                    .await;
                } else if let Some(ticks) = step["ticks"].as_u64() {
                    for _ in 0..ticks {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                } else {
                    panic!("#1343 wait without condition");
                }
            }
            other => panic!("#1343 unknown op {other}"),
        }
    }
    // The case's `stop` aborts an in-flight poll mid-exchange, but the
    // request bytes may already sit in the loopback socket; process them
    // while the case is still armed (they land in its own record) instead of
    // letting the server task see them after `disarm` and charge the
    // next-late stray to this case (review r1 P2 arbitration: three lanes
    // flagged the disarm/strays race as a false-FAIL window).
    drained(&record).await;
    mock.disarm();

    // Every scripted slot must have been served; polls beyond the scripted
    // ones only ever see the dry default and are invisible in the
    // deduplicated pollQueries projection.
    {
        let record = record.lock().expect("#1343 record mutex");
        let scripted_polls = Mock::scripted(case, "polls").len();
        let scripted_sends = Mock::scripted(case, "sends").len();
        crate::negative_assert!(
            record.polls >= scripted_polls,
            "#1343 case {name} serves every scripted poll ({}/{scripted_polls})",
            record.polls
        );
        crate::negative_assert!(
            record.sends >= scripted_sends,
            "#1343 case {name} serves every scripted send ({}/{scripted_sends})",
            record.sends
        );
    }
    // Quiescence: an in-flight poll aborted by the case's stop may still be
    // recording server-side, but the reply projections must stop arriving.
    tokio::time::sleep(SETTLE).await;
    let settled = {
        let record = record.lock().expect("#1343 record mutex");
        (record.sends_out.len(), record.gateway.len())
    };
    tokio::time::sleep(SETTLE).await;
    {
        let record = record.lock().expect("#1343 record mutex");
        crate::negative_assert_eq!(
            record.sends_out.len(),
            settled.0,
            "#1343 case {name} sends stop arriving after the case ends"
        );
        crate::negative_assert_eq!(
            record.gateway.len(),
            settled.1,
            "#1343 case {name} gateway records stop arriving after the case ends"
        );
    }
    let strays = mock.strays.lock().expect("#1343 stray mutex");
    // #2285 r2 ④: `arm()` no longer clears strays, so the list keeps every
    // stray since the replay began. Each entry carries the epoch of the case
    // whose window it was read in: a leftover from an earlier case fails
    // this case by name (the leak is visible, not silently dropped by the
    // next `arm()`), and a stray of this case's own epoch is the same
    // out-of-armed-window failure it always was. The strays themselves go
    // to stderr (the assertion messages stay byte-stable for the receipt
    // protocol).
    if !strays.is_empty() {
        eprintln!("#1343 case {name} strays: {strays:?}");
    }
    crate::negative_assert!(
        strays.iter().all(|(epoch, _)| *epoch == case_epoch),
        "#2285 every stray lands inside the case that armed it (case {name})"
    );
    crate::negative_assert_eq!(
        strays
            .iter()
            .filter(|(epoch, _)| *epoch == case_epoch)
            .count(),
        0,
        "#1343 case {name} records no request outside an armed case"
    );
    drop(strays);
    record.lock().expect("#1343 record mutex").snapshot()
}

#[tokio::test]
async fn channel_matches_real_ts() {
    let oracle = oracle();
    let mock = Mock::new();
    start_mock_on(mock.clone(), MOCK_PORT).await;
    for case in cases(&oracle) {
        let name = case["name"].as_str().expect("#1343 case name");
        let actual = replay(case, &mock).await;
        crate::negative_assert_eq!(
            actual,
            case["expected"],
            "#1343 case {name} matches the TS oracle"
        );
    }
}

// ---------------------------------------------------------------- pins

/// Diff 003's second half: `stop` does not cancel a turn already dispatched
/// (it finishes channel-side), but `sendReply`'s `running === false` gate
/// keeps its reply off the wire. The golden's stop-drains case pins the turn
/// finishing with empty events; this pin holds the gate itself, with a turn
/// whose reply text would otherwise be delivered.
#[tokio::test]
async fn send_reply_after_stop_is_gated_by_running() {
    const CELL: &str = "#1343 a reply after stop never reaches the wire";
    const PIN_PORT: u16 = 19462;
    let record = Arc::new(Mutex::new(Record::default()));
    let mock = Mock::new();
    mock.arm(
        &json!({
            "scripted": {
                "polls": [
                    { "status": 200, "json": [
                        { "guid": "pin-1", "chatGuid": "BB-PIN", "text": "停后回复" }
                    ] }
                ],
                "sends": []
            }
        }),
        record.clone(),
    );
    start_mock_on(mock.clone(), PIN_PORT).await;
    let gates = Gates::default();
    let responded: Gate = Arc::new(tokio::sync::watch::Sender::new(false));
    let responder = Arc::new(MockResponder {
        record: record.clone(),
        responded: responded.clone(),
    });
    let turns = vec![TurnScript {
        gate_before: Some("turn-1".to_owned()),
        events: vec![json!({ "type": "assistant_text_delta", "text": "迟到的回复。" })],
        ..TurnScript::default()
    }];
    let gateway = MockInbound::new(turns, record.clone(), gates.clone(), responded);
    let logger = record.clone();
    let channel = BlueBubblesChannel::new(
        BlueBubblesOptions {
            server_url: Some(JsString::from(format!("http://127.0.0.1:{PIN_PORT}"))),
            password: Some(JsString::from("pw-pin")),
            mapper: None,
            env: None,
        },
        Arc::new(HelperInteraction::new(responder)),
    );
    let deps = ChannelStartDeps {
        config: None,
        report_status: None,
        log: Some(Arc::new(move |level, message| {
            logger
                .lock()
                .expect("#1343 record mutex")
                .logs
                .push(json!([level, mask_dynamic(message)]));
        })),
        turn_gateway: Some(gateway),
    };
    let handle = bounded("pin", channel.start(deps))
        .await
        .expect("#1343 pin start");
    // The turn is dispatched and now sits at its gate; stop runs first.
    {
        let record = record.clone();
        until(CELL, "pin gateway 1", move || {
            let record = record.lock().expect("#1343 record mutex");
            !record.gateway.is_empty()
        })
        .await;
    }
    bounded("pin", handle.stop("done"))
        .await
        .expect("#1343 pin stop");
    gate(&gates, "turn-1").send_replace(true);
    {
        let record = record.clone();
        until(CELL, "pin gateway 2", move || {
            record.lock().expect("#1343 record mutex").gateway.len() >= 2
        })
        .await;
    }
    // The turn finished (turnEnd is the second gateway record); its reply
    // text never became a POST — the wire saw zero sends.
    tokio::time::sleep(SETTLE).await;
    {
        let record = record.lock().expect("#1343 record mutex");
        crate::negative_assert_eq!(
            record
                .gateway
                .last()
                .and_then(|cell| cell.as_object())
                .and_then(|cell| cell.get("turnEnd")),
            Some(&json!(true)),
            "#1343 the held turn still finishes after stop"
        );
        crate::negative_assert_eq!(record.sends_out.len(), 0, "{CELL}");
    }
}

#[test]
#[ignore = "regenerates from the real TS baseline; set PDRS_BASELINE and the pinned Node v22.22.0 runtime"]
fn bluebubbles_fixture_regenerates_identically() {
    crate::test_baseline::regenerates_identically("channel-bluebubbles");
}

// ------------------------------------------------- #2285 poll-driver pins

/// `sendReply`'s `AbortSignal.timeout(120_000)` (Node v22.22.0 实测, see
/// `_tmp/20261108-issue-2285-r1/probe-2285.output`: a fresh 120 000 ms
/// signal stays pending — a real deadline, not an immediate abort — and
/// the mechanism fires with `TimeoutError` at its own deadline; the 120 000
/// literal itself is the TS source). The reply POST deadline is a semantic
/// sibling of `MESSAGE_LIMIT` (both ride the request bytes) and gets the
/// same constant-pin + negative-control treatment (#2285 ①).
#[test]
fn send_timeout_is_the_ts_abort_deadline() {
    crate::negative_assert_eq!(
        SEND_TIMEOUT,
        Duration::from_millis(120_000),
        "#2285 the reply POST keeps the TS AbortSignal.timeout(120_000) deadline"
    );
}

/// #2285 ②: dropping a started channel without `stop` must end the poll
/// driver. TS leaks its interval on that flow (the registered unsupported
/// start-without-stop shape); here the driver holds only a `Weak<Runtime>`,
/// so the next tick's upgrade failure breaks the loop. Two observable
/// proofs, both bounded by `DEADLINE`: the loopback server stops seeing
/// polls once a full poll interval has had every chance to fire, and the
/// `Runtime` itself is freed (the driver's `Weak` no longer upgrades).
/// This pin's mock answers every request; a server whose responses
/// outlast `POLL_MS` keeps an in-flight poll holding a strong `Arc`, so
/// that in-flight window is not covered here — the registered TS-leak
/// shape (007, known gap, no dedicated pin).
#[tokio::test]
async fn poll_driver_exits_when_runtime_is_dropped() {
    const CELL: &str = "#2285 dropping a started channel without stop ends the poll driver";
    const DROP_PORT: u16 = 19463;
    let record = Arc::new(Mutex::new(Record::default()));
    let mock = Mock::new();
    mock.arm(
        &json!({ "scripted": { "polls": [], "sends": [] } }),
        record.clone(),
    );
    start_mock_on(mock.clone(), DROP_PORT).await;
    let channel = BlueBubblesChannel::new(
        BlueBubblesOptions {
            server_url: Some(JsString::from(format!("http://127.0.0.1:{DROP_PORT}"))),
            password: Some(JsString::from("pw-drop")),
            mapper: None,
            env: None,
        },
        no_interaction(),
    );
    let deps = ChannelStartDeps {
        config: None,
        report_status: None,
        log: None,
        turn_gateway: None,
    };
    let handle = bounded("drop", channel.start(deps))
        .await
        .expect("#2285 drop start");
    {
        let record = record.clone();
        until(CELL, "drop first poll", move || {
            record.lock().expect("#1343 record mutex").polls >= 1
        })
        .await;
    }
    let weak = Arc::downgrade(&channel.runtime);
    drop(handle);
    drop(channel);
    // One full poll interval plus the settle margin: a live driver would
    // have fired at least one tick in that window and the loopback server
    // would have seen it (the immediate poll already landed above).
    tokio::time::sleep(SETTLE).await;
    let before = record.lock().expect("#1343 record mutex").polls;
    tokio::time::sleep(POLL_MS + SETTLE * 4).await;
    tokio::time::sleep(SETTLE).await;
    {
        let record = record.lock().expect("#1343 record mutex");
        crate::negative_assert_eq!(
            record.polls,
            before,
            "{CELL} (the server keeps seeing polls after the drop)"
        );
    }
    // The interval task itself is gone: its `Weak` no longer upgrades once
    // every strong reference is dropped. A leaked driver (the TS shape)
    // parks on its timer forever and keeps this upgrade succeeding.
    let started = Instant::now();
    while weak.upgrade().is_some() {
        crate::negative_assert!(started.elapsed() < DEADLINE, "{CELL}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// #2285 ③: a panicking per-tick poll task must not stop the interval
/// driver — TS's `pollMessages` catch keeps the channel polling through a
/// failing exchange, and `void this.pollMessages()` gives TS no panic
/// detection either (007 registers the Rust deviation: the panic is
/// swallowed by the runtime instead of logged). The injection poisons the
/// scripted gateway's turn-index mutex; the dispatchable row sits in slot
/// one, so it lands in the driver's first tick's spawned poll task (the
/// immediate poll draws the row-less slot zero) — its `submit` panics after
/// the GET already landed server-side, the tick after it (the dry default,
/// no rows) completes cleanly, and `stop` still returns `Ok` — the driver's
/// handle is reaped, not stalled. The third-poll wait is what the
/// `poll-task-inlined-into-driver` negative control kills: inlining the
/// per-tick poll moves the panic into the driver task itself, no third
/// poll ever arrives, and `until` fires the cell.
#[tokio::test]
async fn poll_task_panic_does_not_stop_the_driver() {
    const CELL: &str = "#2285 a panicking poll task does not stop the poll driver";
    const PANIC_PORT: u16 = 19464;
    let record = Arc::new(Mutex::new(Record::default()));
    let mock = Mock::new();
    mock.arm(
        &json!({
            "scripted": {
                // Slot zero (the immediate poll) is row-less; the poison row
                // lands in the driver's first tick, and every later poll
                // draws the dry default and never touches a lock the
                // poisoned path could poison.
                "polls": [
                    { "status": 200, "json": [] },
                    { "status": 200, "json": [
                        { "guid": "panic-1", "chatGuid": "BB-PANIC", "text": "boom" }
                    ] }
                ],
                "sends": []
            }
        }),
        record.clone(),
    );
    start_mock_on(mock.clone(), PANIC_PORT).await;
    let gates = Gates::default();
    let responded: Gate = Arc::new(tokio::sync::watch::Sender::new(false));
    let responder = Arc::new(MockResponder {
        record: record.clone(),
        responded: responded.clone(),
    });
    let gateway = MockInbound::new(Vec::new(), record.clone(), gates.clone(), responded);
    // Poison the turn-index mutex before any dispatch can use it.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = gateway.next.lock().expect("#2285 turn index");
        panic!("#2285 poison the turn index");
    }));
    let channel = BlueBubblesChannel::new(
        BlueBubblesOptions {
            server_url: Some(JsString::from(format!("http://127.0.0.1:{PANIC_PORT}"))),
            password: Some(JsString::from("pw-panic")),
            mapper: None,
            env: None,
        },
        Arc::new(HelperInteraction::new(responder)),
    );
    let logger = record.clone();
    let deps = ChannelStartDeps {
        config: None,
        report_status: None,
        log: Some(Arc::new(move |level, message| {
            logger
                .lock()
                .expect("#1343 record mutex")
                .logs
                .push(json!([level, mask_dynamic(message)]));
        })),
        turn_gateway: Some(gateway.clone()),
    };
    let handle = bounded("panic", channel.start(deps.clone()))
        .await
        .expect("#2285 panic start");
    // The immediate poll (row-less) lands first.
    {
        let record = record.clone();
        until(CELL, "panic first poll", move || {
            record.lock().expect("#1343 record mutex").polls >= 1
        })
        .await;
    }
    // The driver's first tick served the poison row's GET; its dispatch
    // panicked on the poisoned turn index right after (invisibly — that is
    // the registered deviation).
    {
        let record = record.clone();
        until(CELL, "panic second poll", move || {
            record.lock().expect("#1343 record mutex").polls >= 2
        })
        .await;
    }
    // The interval survived the panicking poll task: its next tick still
    // polls (a clean, row-less exchange).
    {
        let record = record.clone();
        until(CELL, "panic third poll", move || {
            record.lock().expect("#1343 record mutex").polls >= 3
        })
        .await;
    }
    // Judged only now, after the third poll: the dispatch spawned at the
    // first tick has certainly run, so an empty gateway can no longer pass
    // on a dispatch that simply has not executed yet (the r2 review's
    // false-PASS window — the server counts the poll before the spawned
    // dispatch runs). Exactly one submit attempt proves the injection path
    // was walked (`submit` was reached); the empty projection proves the
    // panic beat the recording push.
    crate::negative_assert_eq!(
        gateway
            .submit_attempts
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "{CELL} (the poisoned dispatch reached submit exactly once)"
    );
    crate::negative_assert!(
        record
            .lock()
            .expect("#1343 record mutex")
            .gateway
            .is_empty(),
        "{CELL} (the poisoned dispatch panicked before recording)"
    );
    bounded("panic", handle.stop("done"))
        .await
        .expect("#2285 stop still succeeds after a poll task panicked");
    // Weak judgment only, per 003-R1's registered abort/reap no-criterion
    // gap: `stop` already returned `Ok` above, and this window checks the
    // server-side count stops growing afterwards. It deliberately does not
    // claim to probe a surviving driver — `running === false` returns any
    // post-stop `poll_messages` before the GET, so the count freezes with
    // or without one (the drop pin can make that claim because it never
    // stops; this scenario cannot).
    tokio::time::sleep(POLL_MS + SETTLE * 4).await;
    let settled = record.lock().expect("#1343 record mutex").polls;
    tokio::time::sleep(POLL_MS + SETTLE).await;
    {
        let record = record.lock().expect("#1343 record mutex");
        crate::negative_assert_eq!(
            record.polls,
            settled,
            "{CELL} (polls stop arriving after stop)"
        );
    }
}

/// The disarm drain's termination condition (#2285 fix-r1, boundary #2): a
/// single silent 50 ms window must not end the drain. An arrival landing
/// between the first and the second window — the starved-server shape the
/// old single-window check mistook for quiescence, which was r1's false
/// FAIL window — must still be observed and extend the wait. The paused
/// clock makes the trace exact: the mover's deadline sits determinately
/// between the two windows, so the drain runs 50 → 75 (arrival) → 100
/// (movement seen, extension) → 150 → 200 ms (two consecutive silent
/// windows) and never earlier — the pin below asserts that exact total, so
/// a drain that terminated on *cumulative* silence instead (150 ms, the
/// `drained-quiet-not-reset` negative control) cannot pass it either.
#[tokio::test(start_paused = true)]
async fn drained_holds_through_a_single_silent_window() {
    const CELL: &str = "#2285 the disarm drain holds through a single silent window";
    let record = Arc::new(Mutex::new(Record::default()));
    let mover = {
        let record = record.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SETTLE + SETTLE / 2).await;
            record.lock().expect("#1343 record mutex").polls += 1;
        })
    };
    let started = tokio::time::Instant::now();
    drained(&record).await;
    mover.await.expect("#2285 drained mover");
    let elapsed = started.elapsed();
    crate::negative_assert_eq!(
        record.lock().expect("#1343 record mutex").polls,
        1,
        "{CELL} (the in-window arrival was observed)"
    );
    crate::negative_assert_eq!(
        elapsed,
        SETTLE * 4,
        "{CELL} (two consecutive silent windows end the drain at exactly 200 ms)"
    );
}

/// #2285 r2 ④: `arm()` no longer clears the strays list. A request the
/// previous case leaked past its armed window — here a real one that walks
/// the loopback socket into the disarmed server task, which charges it
/// through `mock.stray()` exactly like a leaked poll — must survive the
/// next `arm()` and keep the epoch that read it, so the case that observes
/// it fails by name instead of the leak being silently dropped (the r1
/// false-PASS window the old `arm()` clear opened). The
/// `arm-clear-reinserted` negative control re-inserts the clear and dies on
/// the survival assertion; `late-stray-real-path` injects the same real
/// late request inside the golden replay and dies on the first case's
/// no-stray cell.
#[tokio::test]
async fn late_stray_survives_the_next_arm() {
    const CELL: &str = "#2285 a late stray survives the next arm charged to its own epoch";
    const STRAY_PORT: u16 = 19465;
    let record = Arc::new(Mutex::new(Record::default()));
    let mock = Mock::new();
    let first = mock.arm(
        &json!({ "scripted": { "polls": [], "sends": [] } }),
        record.clone(),
    );
    start_mock_on(mock.clone(), STRAY_PORT).await;
    mock.disarm();
    // The real late request: it walks the socket into the server task,
    // which reads it disarmed and charges the stray to `first`'s epoch
    // before answering (so the stray is already recorded once the response
    // completes).
    let client = HttpClient::new();
    let late = client
        .request_unbounded(
            "GET",
            &format!("http://127.0.0.1:{STRAY_PORT}/api/v1/message?late=1"),
            None,
            "",
        )
        .await;
    crate::negative_assert!(late.is_ok(), "{CELL} (the late request settles)");
    {
        let strays = mock.strays.lock().expect("#1343 stray mutex");
        crate::negative_assert_eq!(
            strays.len(),
            1,
            "{CELL} (the disarmed server recorded the late request)"
        );
        crate::negative_assert_eq!(
            strays[0].0,
            first,
            "{CELL} (the stray is charged to the epoch that read it)"
        );
    }
    // The next case's arm must not clear it: the entry keeps its original
    // epoch — foreign to the new epoch — which is exactly how the next
    // case's stray check sees it.
    let second = mock.arm(
        &json!({ "scripted": { "polls": [], "sends": [] } }),
        record.clone(),
    );
    {
        let strays = mock.strays.lock().expect("#1343 stray mutex");
        crate::negative_assert_eq!(
            strays.iter().filter(|(epoch, _)| *epoch == first).count(),
            1,
            "{CELL} (the next arm must not clear the late stray)"
        );
        crate::negative_assert!(second != first, "{CELL} (the arm increments the epoch)");
    }
}

// ---------------------------------------------------------------- pins

#[test]
fn query_encode_matches_urlsearchparams() {
    // Node v22.22.0: `new URL("http://x/"); u.searchParams.set("password",
    // "a b+c!~*()'中"); u.toString()` (measured 2026-11-07, this host).
    assert_eq!(
        query_encode(&JsString::from("a b+c!~*()'中")),
        "a+b%2Bc%21%7E*%28%29%27%E4%B8%AD",
        "#1343 URLSearchParams serializer set"
    );
    assert_eq!(query_encode(&JsString::from("pw-1")), "pw-1");
}

#[test]
fn url_with_path_resolves_against_the_origin() {
    // Node v22.22.0: `new URL("/api/v1/message", "http://h:99/ab?x=1")`
    // → "http://h:99/api/v1/message"; the default port drops.
    assert_eq!(
        url_with_path("http://h:99/ab?x=1", "/api/v1/message").unwrap(),
        "http://h:99/api/v1/message",
        "#1343 absolute reference replaces the base path"
    );
    assert_eq!(
        url_with_path("http://h:80/base/", "/api/v1/message/text").unwrap(),
        "http://h/api/v1/message/text",
        "#1343 default port is normalized away"
    );
    assert_eq!(
        url_with_path("HTTP://H", "/api/v1/message").unwrap(),
        "http://h/api/v1/message",
        "#1343 scheme and host are lowercased"
    );
    // WHATWG special-scheme tolerances, each measured on Node v22.22.0
    // (review r1 P1 arbitration): missing slashes and an empty port still
    // give a working origin, IDNA punycodes the host, an IPv6 literal keeps
    // its brackets.
    for (base, origin) in [
        ("http:/h", "http://h"),
        ("http:h", "http://h"),
        ("http://h:", "http://h"),
        ("https://host:443", "https://host"),
        (
            "http://bücher.example:8080",
            "http://xn--bcher-kva.example:8080",
        ),
        ("http://[::1]:8080", "http://[::1]:8080"),
    ] {
        assert_eq!(
            url_with_path(base, "/api/v1/message").unwrap(),
            format!("{origin}/api/v1/message"),
            "#1343 {base:?} resolves like the Node reference"
        );
    }
    // Node v22.22.0 `fetch` refuses a URL that still carries credentials
    // (TypeError: Request cannot be constructed from a URL that includes
    // credentials) and rejects a non-http(s) scheme (TypeError: fetch
    // failed, cause: unknown scheme) — both are 实测, and both keep the same
    // `poll error` log shape as every other parse failure.
    assert_eq!(
        url_with_path("http://user:pass@h", "/api/v1/message").unwrap_err(),
        "Request cannot be constructed from a URL that includes credentials",
        "#1343 credentials in the URL are the fetch TypeError"
    );
    for base in ["ftp:/h", "myapp://h"] {
        assert_eq!(
            url_with_path(base, "/api/v1/message").unwrap_err(),
            "fetch failed",
            "#1343 {base:?} is the TS fetch unknown-scheme failure"
        );
    }
    for base in ["not a url", "http://", "://x"] {
        assert_eq!(
            url_with_path(base, "/api/v1/message").unwrap_err(),
            "Invalid URL",
            "#1343 {base:?} is the TS TypeError: Invalid URL"
        );
    }
}

#[test]
fn normalize_base_url_strips_trailing_slashes() {
    assert_eq!(normalize_base_url("http://h:1///"), "http://h:1");
    assert_eq!(normalize_base_url("http://h:1"), "http://h:1");
}

// ---------------------------------------------------------------- loader

// The loader mount is the construction seam this PR ships: it resolves
// `extra.serverUrl` / `token ?? extra.password` (`loader.rs` `build_options`)
// and installs `no_interaction()` (A8.3-CH-BLUEBUBBLES-002). The production
// caller of `load_enabled_channels` is a separate assembly unit; this pins
// the mount itself.
#[test]
fn loader_mount_builds_bluebubbles_channel() {
    let mut mounts = crate::channel::loader::ChannelLoaderMounts::default();
    register(&mut mounts);
    assert!(
        mounts.is_registered("bluebubbles"),
        "#1343 loader mount key"
    );
    let adapters = parse_json(
        &json!({
            "bluebubbles": {
                "enabled": true,
                "token": "tok-1",
                "extra": {"serverUrl": "http://127.0.0.1:19461"},
            },
            "telegram": {"enabled": false},
        })
        .to_string(),
    )
    .expect("#1343 adapters JSON");
    let mut errors = Vec::new();
    let channels = crate::channel::loader::load_enabled_channels(
        Some(&adapters),
        &crate::channel::loader::LoadEnabledChannelsOptions { pilot_home: None },
        &mounts,
        &mut |message| errors.push(message),
    );
    assert!(errors.is_empty(), "#1343 loader errors: {errors:?}");
    assert_eq!(
        channels.len(),
        1,
        "#1343 exactly the bluebubbles channel loads"
    );
    assert_eq!(channels[0].channel_key().to_string(), "bluebubbles");
}

/// #2433, loader level: a present non-nullish non-string `extra.serverUrl`
/// makes the TS constructor throw `TypeError: url.replace is not a function`
/// (`normalizeBaseUrl`'s `url.replace`), so `loadEnabledChannels` records
/// exactly one console.error line for the key and continues — the channel
/// never enters the returned array. Falsy non-strings throw too (`??` keeps
/// them; Node v22.22.0 probes in `_tmp/20261026-issue-p2batch-f/s5-probe/`);
/// the string shape keeps constructing (the sibling test above pins that).
#[test]
fn loader_records_server_url_type_errors_and_continues() {
    for shape in ["42", "0", "false", r#"{"host": 1}"#] {
        let mut mounts = crate::channel::loader::ChannelLoaderMounts::default();
        register(&mut mounts);
        let server_url: Value = serde_json::from_str(shape).expect("#2433 serverUrl shape parses");
        let raw = json!({
            "bluebubbles": {
                "enabled": true,
                "token": "tok-1",
                "extra": {"serverUrl": server_url},
            },
            "telegram": {"enabled": false},
        });
        let adapters = parse_json(&raw.to_string()).expect("#2433 loader fixture adapters parse");
        let mut errors = Vec::new();
        let channels = crate::channel::loader::load_enabled_channels(
            Some(&adapters),
            &crate::channel::loader::LoadEnabledChannelsOptions { pilot_home: None },
            &mounts,
            &mut |message| errors.push(message),
        );
        crate::negative_assert_eq!(
            errors,
            vec![
                "[adapters] Failed to load channel \"bluebubbles\": TypeError: url.replace is not a function"
                    .to_owned()
            ],
            "#2433 the loader records the serverUrl TypeError and skips the key ({shape})"
        );
        crate::negative_assert!(
            channels.is_empty(),
            "#2433 the failed key constructs no channel ({shape})"
        );
    }
}
