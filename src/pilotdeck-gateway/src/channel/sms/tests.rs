//! #1339 gold-master replay: the 47 scenarios recorded by
//! `tests/fixtures/channel-sms/generate.mts` against the real TS baseline,
//! re-driven through the Rust port with loopback mocks only.
use super::*;
use base64::Engine as _;
use serde_json::{Map as JsonMap, Value, json};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Mutex as SyncMutex;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::watch;

use super::super::loader::{
    ChannelLoaderMounts, LoadEnabledChannelsOptions, load_enabled_channels,
};
use crate::channel::protocol::{ChannelLog, ChannelStartDeps};

const ORACLE: &str = include_str!("../../../tests/fixtures/channel-sms/oracle.json");
const EXPECTED_CASE_COUNT: usize = 47;
/// The fixed mapper uuid the generator injects, so `/new` session keys match.
const MAPPER_UUID: &str = "aaaaaaaa-1111-2222-3333-444444444444";

// ---------------------------------------------------------------------------
// oracle access and JS value conversion
// ---------------------------------------------------------------------------

fn oracle_cases() -> Vec<Value> {
    crate::test_baseline::oracle(ORACLE);
    let value: Value = serde_json::from_str(ORACLE).expect("#1339 oracle JSON");
    let cases = value["cases"]
        .as_array()
        .cloned()
        .expect("#1339 oracle cases");
    assert_eq!(cases.len(), EXPECTED_CASE_COUNT, "#1339 oracle case count");
    cases
}

fn case_by_name(name: &str) -> Value {
    oracle_cases()
        .into_iter()
        .find(|case| case["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("#1339 missing oracle case {name}"))
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
// shared record, gates and bounded waits
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Record {
    logs: Vec<(String, String)>,
    sends: Vec<Value>,
    rpcs: Vec<Value>,
    responses: Vec<Value>,
    unhandled: Vec<String>,
    cron_results: Vec<bool>,
}

type Gate = Arc<watch::Sender<bool>>;
type Gates = Arc<SyncMutex<HashMap<String, Gate>>>;

fn gate(gates: &Gates, name: &str) -> Gate {
    gates
        .lock()
        .expect("#1339 gates mutex")
        .entry(name.to_owned())
        .or_insert_with(|| Arc::new(watch::Sender::new(false)))
        .clone()
}

async fn released(gate: Gate) {
    let mut receiver = gate.subscribe();
    while !*receiver.borrow_and_update() {
        receiver
            .changed()
            .await
            .expect("#1339 gate sender retained");
    }
}

/// The TS mock's `gates.get(name)?.release()` — a no-op when the gate was
/// never awaited into existence.
fn release_if_exists(gates: &Gates, name: &str) {
    if let Some(gate) = gates.lock().expect("#1339 gates mutex").get(name) {
        let _ = gate.send(true);
    }
}

async fn bounded<T>(label: &str, future: impl Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(5), future).await {
        Ok(value) => value,
        Err(_) => panic!("#1339 wait timeout: {label}"),
    }
}

async fn until(label: &str, predicate: impl Fn() -> bool) {
    bounded(label, async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

/// The generator's settle: a macrotask boundary plus a 25 ms sleep.
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

// ---------------------------------------------------------------------------
// mocks (`makeTwilio` / `makeGateway` from the generator)
// ---------------------------------------------------------------------------

struct MockSender {
    case: Value,
    record: Arc<SyncMutex<Record>>,
    gates: Gates,
}

impl SmsSender for MockSender {
    fn send(&self, message: SmsOutgoingMessage) -> BoxFuture<Result<(), ChannelError>> {
        let case = self.case.clone();
        let record = self.record.clone();
        let gates = self.gates.clone();
        Box::pin(async move {
            let count = {
                let mut record = record.lock().expect("#1339 record mutex");
                record.sends.push(json!({
                    "body": String::from_utf16_lossy(message.body.units()),
                    "from": String::from_utf16_lossy(message.from.units()),
                    "to": String::from_utf16_lossy(message.to.units()),
                }));
                record.sends.len()
            };
            if case["sendBlockAt"].as_u64() == Some(count as u64) {
                released(gate(&gates, "send-block")).await;
            }
            if case["sendFailAt"]
                .as_array()
                .is_some_and(|list| list.iter().any(|at| at.as_u64() == Some(count as u64)))
            {
                let message = case["sendError"].as_str().unwrap_or("mock send failure");
                return Err(ChannelError::Error(message.to_owned()));
            }
            Ok(())
        })
    }
}

struct MockGateway {
    case: Value,
    record: Arc<SyncMutex<Record>>,
    gates: Gates,
}

impl SmsGateway for MockGateway {
    fn submit_turn(
        &self,
        request: SubmitTurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let case = self.case.clone();
        let record = self.record.clone();
        let gates = self.gates.clone();
        Box::pin(async move {
            {
                let mut record = record.lock().expect("#1339 record mutex");
                record.rpcs.push(json!({
                    "rpc": "submitTurn",
                    "request": js_to_json(&JsValue::Object(request.to_js())),
                }));
            }
            let turn = &case["turn"];
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
            let await_error_settled = turn["holdAfter"].as_str() == Some("decide")
                && case["permissionError"].as_str().is_some();
            Ok(Box::new(MockTurnStream {
                events,
                hold,
                await_error_settled,
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
                let mut record = record.lock().expect("#1339 record mutex");
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
        let case = self.case.clone();
        let record = self.record.clone();
        let gates = self.gates.clone();
        Box::pin(async move {
            {
                let mut record = record.lock().expect("#1339 record mutex");
                record.rpcs.push(json!({
                    "rpc": "permissionDecide",
                    "decision": js_to_json(&JsValue::Object(decision.to_js())),
                }));
            }
            if let Some(gate_name) = case["permissionGate"].as_str() {
                released(gate(&gates, gate_name)).await;
            }
            // The `await setImmediate` boundary before the release, then one
            // more yield so the resumed turn (and its tail send) lands before
            // this reply's caller sends the confirmation.
            tokio::task::yield_now().await;
            release_if_exists(&gates, "decided");
            tokio::task::yield_now().await;
            if let Some(error) = case["permissionError"].as_str() {
                return Err(ChannelError::Error(error.to_owned()));
            }
            Ok(PermissionDecisionOutcome {
                delivered: case["permissionDelivered"].as_bool().unwrap_or(true),
            })
        })
    }
}

/// The generator's `submitTurn` async generator: events first, then the
/// `holdAfter` park before completion.
struct MockTurnStream {
    events: VecDeque<JsValue>,
    hold: Option<String>,
    /// The error case parks past the hold until the rejected decide has been
    /// processed by the channel (see the logger handshake in `replay_named`).
    await_error_settled: bool,
    gates: Gates,
}

impl TurnEventStream for MockTurnStream {
    fn next_event(&mut self) -> BoxFuture<Option<Result<JsValue, ChannelError>>> {
        if let Some(event) = self.events.pop_front() {
            return Box::pin(async move { Some(Ok(event)) });
        }
        let hold = self.hold.take();
        let await_error_settled = self.await_error_settled;
        let gates = self.gates.clone();
        Box::pin(async move {
            match hold.as_deref() {
                Some("elicit") => released(gate(&gates, "elicited")).await,
                Some("decide") => released(gate(&gates, "decided")).await,
                _ => {}
            }
            if await_error_settled {
                released(gate(&gates, "decide-error-settled")).await;
            }
            None
        })
    }
}

// ---------------------------------------------------------------------------
// raw probe client request building (the generator's `REQUEST` + signing)
// ---------------------------------------------------------------------------

fn decode_form_body(body: &str) -> String {
    let Some(at) = body.find("Body=") else {
        return String::new();
    };
    let raw = body[at + "Body=".len()..].split('&').next().unwrap_or("");
    String::from_utf8_lossy(&decode_form_component(raw.replace('+', " ").as_bytes())).into_owned()
}

fn signed_headers(step: &Value, case: &Value, port: u16) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = step["headers"]
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_owned()))
                .collect()
        })
        .unwrap_or_default();
    let name = case["name"].as_str().unwrap_or_default();
    if name != "valid-signature" && name != "auth-token-apikey" {
        return headers;
    }
    let body = step["body"].as_str().unwrap_or("");
    let message = if body.contains("Body=") {
        decode_form_body(body)
    } else {
        String::new()
    };
    let extra = case["extra"].as_object().cloned().unwrap_or_default();
    let env = case["env"].as_object().cloned().unwrap_or_default();
    let token = [
        extra.get("authToken"),
        extra.get("apiKey"),
        env.get("TWILIO_AUTH_TOKEN"),
        extra.get("token"),
    ]
    .into_iter()
    .flatten()
    .find(|value| !value.is_null())
    .and_then(Value::as_str)
    .unwrap_or_default();
    let full_url = format!("http://127.0.0.1:{port}/sms/incoming");
    // `Object.keys({From, Body}).sort()` — Body sorts before From.
    let data = format!("{full_url}Body{message}From+15550001");
    let signature = base64::engine::general_purpose::STANDARD
        .encode(hmac_sha1(token.as_bytes(), data.as_bytes()));
    headers.push(("X-Twilio-Signature".to_owned(), signature));
    headers
}

fn build_raw(step: &Value, case: &Value, port: u16) -> Vec<u8> {
    let body: Vec<u8> = match &step["body"] {
        Value::String(text) => text.clone().into_bytes(),
        Value::Object(spec) => spec["repeat"]
            .as_str()
            .unwrap_or_default()
            .repeat(spec["count"].as_u64().unwrap_or_default() as usize)
            .into_bytes(),
        _ => Vec::new(),
    };
    let mut raw = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n",
        step["method"].as_str().unwrap_or("POST"),
        step["path"].as_str().unwrap_or("/sms/incoming"),
    )
    .into_bytes();
    for (key, value) in signed_headers(step, case, port) {
        raw.extend_from_slice(format!("{key}: {value}\r\n").as_bytes());
    }
    raw.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    raw.extend_from_slice(&body);
    raw
}

fn cron_delivery(value: &Value) -> CronResultDelivery {
    CronResultDelivery {
        session_key: value["sessionKey"].as_str().map(JsString::from),
        origin_session_key: value["originSessionKey"].as_str().map(JsString::from),
        text: JsString::from(value["text"].as_str().unwrap_or_default()),
        origin_channel_key: value["originChannelKey"].as_str().map(JsString::from),
    }
}

// ---------------------------------------------------------------------------
// replay driver
// ---------------------------------------------------------------------------

async fn replay_named(name: &str) {
    let case = case_by_name(name);
    let port = case["port"].as_u64().expect("#1339 case port") as u16;
    let record: Arc<SyncMutex<Record>> = Arc::new(SyncMutex::new(Record::default()));
    let gates: Gates = Arc::new(SyncMutex::new(HashMap::new()));

    let log_record = record.clone();
    let log_gates = gates.clone();
    let logger: ChannelLog = Arc::new(move |level, message| {
        log_record
            .lock()
            .expect("#1339 record mutex")
            .logs
            .push((level.to_owned(), message.to_owned()));
        // Microtask-order handshake for the rejected-decide scenario: the TS
        // helper's catch (the restore) runs as microtask M2, before the
        // resumed turn's tail (M1b, `clearAfterTurn`). The channel logs the
        // error right after the restore, so the log line is the point where
        // the turn may continue.
        if message.starts_with("sms: permission answer error: ") {
            release_if_exists(&log_gates, "decide-error-settled");
        }
    });
    let env_map: HashMap<String, String> = case["env"]
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_owned()))
                .collect()
        })
        .unwrap_or_default();
    let env: EnvLookup = Arc::new(move |name| env_map.get(name).cloned().map(JsString::from));
    let unhandled_record = record.clone();
    let record_unhandled: UnhandledRecorder = Arc::new(move |message| {
        unhandled_record
            .lock()
            .expect("#1339 record mutex")
            .unhandled
            .push(message.to_owned());
    });

    // The oracle writer does not serialize the generator's `blockPort` flag;
    // `start-port-busy` is the only definition that sets it, so the blocker is
    // reconstructed by name here.
    let blocker = if name == "start-port-busy" {
        Some(
            tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .expect("#1339 blocker bind"),
        )
    } else {
        None
    };

    let gateway: GatewayRef = Arc::new(MockGateway {
        case: case.clone(),
        record: record.clone(),
        gates: gates.clone(),
    });
    let sender_case = case.clone();
    let sender_record = record.clone();
    let sender_gates = gates.clone();
    let twilio_factory: TwilioFactory = Arc::new(move |account_sid, _auth_token| {
        if account_sid.equals_str("THROW") {
            return Err(ChannelError::Error("twilio init boom".to_owned()));
        }
        Ok(Arc::new(MockSender {
            case: sender_case.clone(),
            record: sender_record.clone(),
            gates: sender_gates.clone(),
        }) as Arc<dyn SmsSender>)
    });
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(&case["extra"])),
        mapper: Some(SmsSessionMapper::new(
            SmsSessionMapperState::default(),
            Box::new(|| JsString::from(MAPPER_UUID)),
        )),
        gateway: Some(gateway),
        env: Some(env),
        twilio_factory: Some(twilio_factory),
        record_unhandled: Some(record_unhandled),
    });
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: Some(logger),
        })
        .await
        .expect("#1339 start");

    for step in case["steps"].as_array().cloned().unwrap_or_default() {
        match step["op"].as_str().expect("#1339 step op") {
            "request" => {
                let raw = build_raw(&step, &case, port);
                let outcome = bounded("probe response", raw_request(port, &raw)).await;
                let mut record = record.lock().expect("#1339 record mutex");
                record.responses.push(match outcome {
                    Ok(value) => value,
                    // The generator's connect refused mapping; the probe client
                    // reports ECONNREFUSED the same way for raw requests.
                    Err(_) => json!({"refused": true}),
                });
            }
            "connect" => {
                let refused = bounded("connect probe", TcpStream::connect(("127.0.0.1", port)))
                    .await
                    .is_err();
                record
                    .lock()
                    .expect("#1339 record mutex")
                    .responses
                    .push(json!({"refused": refused}));
            }
            "wait-log" => {
                let level = step["level"].as_str().unwrap_or_default().to_owned();
                let text = step["text"].as_str().unwrap_or_default().to_owned();
                let label = format!("log {text}");
                until(&label, || {
                    record
                        .lock()
                        .expect("#1339 record mutex")
                        .logs
                        .iter()
                        .any(|(candidate, message)| *candidate == level && message == &text)
                })
                .await;
            }
            "wait-sends" => {
                let count = step["count"].as_u64().unwrap_or_default() as usize;
                until(&format!("sends {count}"), || {
                    record.lock().expect("#1339 record mutex").sends.len() >= count
                })
                .await;
            }
            "wait-rpcs" => {
                let count = step["count"].as_u64().unwrap_or_default() as usize;
                until(&format!("rpcs {count}"), || {
                    record.lock().expect("#1339 record mutex").rpcs.len() >= count
                })
                .await;
            }
            "release" => {
                release_if_exists(&gates, step["gate"].as_str().expect("#1339 gate name"));
            }
            "settle" => settle().await,
            "stop" => {
                // TS `handle.stop(reason)` — an absent reason logs "no reason".
                let reason = step["reason"].as_str().unwrap_or("no reason");
                bounded("stop", handle.stop(reason))
                    .await
                    .expect("#1339 stop");
            }
            "cron" => {
                let delivery = cron_delivery(&step["delivery"]);
                let delivered = bounded("cron delivery", channel.deliver_cron_result(&delivery))
                    .await
                    .expect("#1339 cron delivery");
                record
                    .lock()
                    .expect("#1339 record mutex")
                    .cron_results
                    .push(delivered);
            }
            other => panic!("#1339 unknown step {other}"),
        }
    }

    // The generator's finally block: cleanup stop, blocker close, settle.
    bounded("cleanup stop", handle.stop("cleanup"))
        .await
        .expect("#1339 cleanup stop");
    drop(blocker);
    settle().await;

    let snapshot = {
        let record = record.lock().expect("#1339 record mutex");
        json!({
            "logs": record.logs.iter().map(|(level, text)| json!([level, text])).collect::<Vec<_>>(),
            "sends": record.sends,
            "rpcs": record.rpcs,
            "responses": record.responses,
            "unhandled": record.unhandled,
            "cronResults": record.cron_results,
        })
    };
    crate::negative_assert_eq!(snapshot, case["expected"], "#1339 sms oracle: {name}");
}

macro_rules! replay_cases {
    ($($case:literal => $test:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $test() {
                replay_named($case).await;
            }
        )*
    };
}

replay_cases! {
    "start-missing-config" => start_missing_config,
    "start-twilio-init-failed" => start_twilio_init_failed,
    "start-port-busy" => start_port_busy,
    "start-port-out-of-range" => start_port_out_of_range,
    "start-port-nan" => start_port_nan,
    "start-ok" => start_ok,
    "start-ok-public-url" => start_ok_public_url,
    "start-env-config" => start_env_config,
    "start-extra-coercion" => start_extra_coercion,
    "auth-token-chain" => auth_token_chain,
    "wrong-path-and-method" => wrong_path_and_method,
    "query-ignored" => query_ignored,
    "payload-too-large" => payload_too_large,
    "payload-exactly-1mib" => payload_exactly_1mib,
    "bad-json" => bad_json,
    "content-type-json-charset" => content_type_json_charset,
    "content-type-uppercase-not-json" => content_type_uppercase_not_json,
    "bad-signature" => bad_signature,
    "valid-signature" => valid_signature,
    "empty-from-and-whitespace-body" => empty_from_and_whitespace_body,
    "form-plus-and-percent" => form_plus_and_percent,
    "dup-keys-last-wins" => dup_keys_last_wins,
    "non-string-json-body" => non_string_json_body,
    "turn-reply-fragments" => turn_reply_fragments,
    "turn-error" => turn_error,
    "send-fail" => send_fail,
    "active-dedupe" => active_dedupe,
    "new-session" => new_session,
    "elicitation-cancel" => elicitation_cancel,
    "elicitation-answer" => elicitation_answer,
    "permission-allow-once" => permission_allow_once,
    "permission-deny" => permission_deny,
    "permission-allow-session" => permission_allow_session,
    "permission-invalid-reply" => permission_invalid_reply,
    "permission-decide-in-flight-status" => permission_decide_in_flight_status,
    "permission-prompt-sending-status" => permission_prompt_sending_status,
    "permission-prompt-fail-retry" => permission_prompt_fail_retry,
    "permission-decide-not-delivered" => permission_decide_not_delivered,
    "permission-decide-error" => permission_decide_error,
    "permission-confirmation-send-fail" => permission_confirmation_send_fail,
    "elicitation-multiselect" => elicitation_multiselect,
    "elicitation-answer-fallback" => elicitation_answer_fallback,
    "cron-delivery" => cron_delivery_case,
    "cron-after-stop" => cron_after_stop,
    "auth-token-apikey" => auth_token_apikey,
    "start-ok-custom-path" => start_ok_custom_path,
    "stop-during-turn" => stop_during_turn,
}

#[test]
fn oracle_case_count_is_recorded() {
    assert_eq!(oracle_cases().len(), EXPECTED_CASE_COUNT);
}

#[test]
fn loader_mount_builds_sms_channel() {
    let mut mounts = ChannelLoaderMounts::default();
    register(&mut mounts);
    assert!(mounts.is_registered("sms"));
    let adapters = to_js(&json!({
        "sms": {"enabled": true, "extra": {"accountSid": "AC1"}},
        "telegram": {"enabled": false},
    }));
    let mut errors = Vec::new();
    let channels = load_enabled_channels(
        Some(&adapters),
        &LoadEnabledChannelsOptions { pilot_home: None },
        &mounts,
        &mut |message| errors.push(message),
    );
    assert!(errors.is_empty(), "#1339 loader errors: {errors:?}");
    assert_eq!(channels.len(), 1, "#1339 exactly the sms channel loads");
    assert_eq!(channels[0].channel_key().to_string(), "sms");
}

#[test]
#[ignore = "regenerates from the real TS baseline; set PDRS_BASELINE and the pinned Node v22.22.0 runtime"]
fn channel_sms_fixture_regenerates_identically() {
    crate::test_baseline::regenerates_identically("channel-sms");
}
