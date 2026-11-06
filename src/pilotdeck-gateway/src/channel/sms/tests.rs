//! #1339 gold-master replay: the scenarios recorded by
//! `tests/fixtures/channel-sms/generate.mts` against the real TS baseline,
//! re-driven through the Rust port with loopback mocks only. The
//! `port-busy-preflight` case has no replay cell of its own: it exists so
//! `port_busy_preflight_fails_fast` can hold its port busy.
use super::*;
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
const EXPECTED_CASE_COUNT: usize = 55;
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
    // #1339 fix-r1 (P2 #10): pin both counts — the loaded array and the
    // generator's own `expectedCaseCount` declaration — so a regenerated
    // oracle that adds a case without updating the harness fails loudly.
    crate::negative_assert_eq!(
        cases.len(),
        EXPECTED_CASE_COUNT,
        "#1339 oracle case array length"
    );
    let declared_expected =
        EXPECTED_CASE_COUNT + usize::from(crate::mutants::on("sms-oracle-declaration-count"));
    let declared = value["expectedCaseCount"]
        .as_u64()
        .expect("#1339 oracle expectedCaseCount field");
    crate::negative_assert_eq!(
        declared as usize,
        declared_expected,
        "#1339 oracle declared case count"
    );
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
            if recorded_send_block(&case) == Some(count as u64) {
                released(gate(&gates, "send-block")).await;
            }
            if recorded_send_fail_at(&case).contains(&(count as u64)) {
                let message = recorded_send_error(&case).unwrap_or("mock send failure");
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
            let turn = recorded_turn(&case).unwrap_or(&Value::Null);
            if let Some(gate_name) = recorded_turn_gate(turn) {
                released(gate(&gates, gate_name)).await;
            }
            if let Some(error) = recorded_turn_error(turn) {
                return Err(ChannelError::Error(error.to_owned()));
            }
            let events: VecDeque<JsValue> = recorded_turn_events(turn)
                .map(|events| events.iter().map(to_js).collect())
                .unwrap_or_default();
            let hold = recorded_turn_hold(turn).map(str::to_owned);
            let await_error_settled = recorded_turn_hold(turn) == Some("decide")
                && recorded_permission_error(&case).is_some();
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
            if let Some(gate_name) = recorded_permission_gate(&case) {
                released(gate(&gates, gate_name)).await;
            }
            // The `await setImmediate` boundary before the release, then one
            // more yield so the resumed turn (and its tail send) lands before
            // this reply's caller sends the confirmation.
            tokio::task::yield_now().await;
            release_if_exists(&gates, "decided");
            tokio::task::yield_now().await;
            if let Some(error) = recorded_permission_error(&case) {
                return Err(ChannelError::Error(error.to_owned()));
            }
            Ok(PermissionDecisionOutcome {
                delivered: recorded_permission_delivered(&case).unwrap_or(true),
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
// raw probe client request building (the generator's `REQUEST`)
// ---------------------------------------------------------------------------

/// The effective headers the generator recorded per step — including the
/// `X-Twilio-Signature` values the TS side itself computed over its signing
/// URL. #1339 fix-r1 (P1 #1): the harness replays those recorded bytes
/// instead of re-signing with the port's own HMAC, so the oracle pins the
/// signature algorithm through the generator's implementation.
// #1729: distinguish omitted recording fields from present values of the
// wrong type. Each switch restores only its own field's old fail-open read.
fn recorded_checked<'a, T>(
    value: Option<&'a Value>,
    field: &str,
    kind: &str,
    parse: impl FnOnce(&'a Value) -> Option<T>,
    required: bool,
    fail_open: bool,
) -> Result<Option<T>, String> {
    match value {
        None if required && !fail_open => {
            Err(format!("#1339 {field} must be {kind}, got <missing>"))
        }
        None => Ok(None),
        Some(value) => match parse(value) {
            Some(parsed) => Ok(Some(parsed)),
            None if fail_open => Ok(None),
            None => Err(format!("#1339 {field} must be {kind}, got {value}")),
        },
    }
}

fn recorded_header_map_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        value.get("headers"),
        "headers",
        "an object",
        |value| value.is_object().then_some(value),
        false,
        crate::mutants::on("sms-oracle-recorded-header-map"),
    )
}

fn recorded_header_map(value: &Value) -> Option<&Value> {
    recorded_header_map_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_header_map_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_header_map_checked(&json!({"headers": []})).is_err(),
        "#1339 #1729 rejects headers type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 headers must be an object")]
fn recorded_header_map_panics_on_wrong_type() {
    recorded_header_map(&json!({"headers": []}));
}

fn recorded_method_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("method"),
        "method",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-method"),
    )
}

fn recorded_method(value: &Value) -> Option<&str> {
    recorded_method_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_method_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_method_checked(&json!({"method": []})).is_err(),
        "#1339 #1729 rejects method type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 method must be a string")]
fn recorded_method_panics_on_wrong_type() {
    recorded_method(&json!({"method": []}));
}

fn recorded_path_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("path"),
        "path",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-path"),
    )
}

fn recorded_path(value: &Value) -> Option<&str> {
    recorded_path_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_path_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_path_checked(&json!({"path": []})).is_err(),
        "#1339 #1729 rejects path type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 path must be a string")]
fn recorded_path_panics_on_wrong_type() {
    recorded_path(&json!({"path": []}));
}

fn recorded_body_repeat_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("repeat"),
        "body.repeat",
        "a nonempty string",
        |value| value.as_str().filter(|text| !text.is_empty()),
        true,
        crate::mutants::on("sms-oracle-recorded-body-repeat"),
    )
}

fn recorded_body_repeat(value: &Value) -> Option<&str> {
    recorded_body_repeat_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_body_repeat_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_body_repeat_checked(&json!({"repeat": []})).is_err(),
        "#1339 #1729 rejects body.repeat type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 body.repeat must be a nonempty string")]
fn recorded_body_repeat_panics_on_wrong_type() {
    recorded_body_repeat(&json!({"repeat": []}));
}

fn recorded_body_count_checked(value: &Value) -> Result<Option<u64>, String> {
    recorded_checked(
        value.get("count"),
        "body.count",
        "a u64",
        Value::as_u64,
        true,
        crate::mutants::on("sms-oracle-recorded-body-count"),
    )
}

fn recorded_body_count(value: &Value) -> Option<u64> {
    recorded_body_count_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_body_count_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_body_count_checked(&json!({"count": []})).is_err(),
        "#1339 #1729 rejects body.count type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 body.count must be a u64")]
fn recorded_body_count_panics_on_wrong_type() {
    recorded_body_count(&json!({"count": []}));
}

fn recorded_truncate_checked(value: &Value) -> Result<Option<u64>, String> {
    recorded_checked(
        value.get("truncate"),
        "truncate",
        "a u64",
        Value::as_u64,
        false,
        crate::mutants::on("sms-oracle-recorded-truncate"),
    )
}

fn recorded_truncate(value: &Value) -> Option<u64> {
    recorded_truncate_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_truncate_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_truncate_checked(&json!({"truncate": []})).is_err(),
        "#1339 #1729 rejects truncate type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 truncate must be a u64")]
fn recorded_truncate_panics_on_wrong_type() {
    recorded_truncate(&json!({"truncate": []}));
}

fn recorded_delivery_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        value.get("delivery"),
        "delivery",
        "an object",
        |value| value.is_object().then_some(value),
        false,
        crate::mutants::on("sms-oracle-recorded-delivery"),
    )
}

fn recorded_delivery(value: &Value) -> Option<&Value> {
    recorded_delivery_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_delivery_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_delivery_checked(&json!({"delivery": []})).is_err(),
        "#1339 #1729 rejects delivery type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 delivery must be an object")]
fn recorded_delivery_panics_on_wrong_type() {
    recorded_delivery(&json!({"delivery": []}));
}

fn recorded_cron_session_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("sessionKey"),
        "delivery.sessionKey",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-cron-session"),
    )
}

fn recorded_cron_session(value: &Value) -> Option<&str> {
    recorded_cron_session_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_cron_session_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_cron_session_checked(&json!({"sessionKey": []})).is_err(),
        "#1339 #1729 rejects delivery.sessionKey type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 delivery.sessionKey must be a string")]
fn recorded_cron_session_panics_on_wrong_type() {
    recorded_cron_session(&json!({"sessionKey": []}));
}

fn recorded_cron_origin_session_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("originSessionKey"),
        "delivery.originSessionKey",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-cron-origin-session"),
    )
}

fn recorded_cron_origin_session(value: &Value) -> Option<&str> {
    recorded_cron_origin_session_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_cron_origin_session_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_cron_origin_session_checked(&json!({"originSessionKey": []})).is_err(),
        "#1339 #1729 rejects delivery.originSessionKey type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 delivery.originSessionKey must be a string")]
fn recorded_cron_origin_session_panics_on_wrong_type() {
    recorded_cron_origin_session(&json!({"originSessionKey": []}));
}

fn recorded_cron_origin_channel_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("originChannelKey"),
        "delivery.originChannelKey",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-cron-origin-channel"),
    )
}

fn recorded_cron_origin_channel(value: &Value) -> Option<&str> {
    recorded_cron_origin_channel_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_cron_origin_channel_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_cron_origin_channel_checked(&json!({"originChannelKey": []})).is_err(),
        "#1339 #1729 rejects delivery.originChannelKey type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 delivery.originChannelKey must be a string")]
fn recorded_cron_origin_channel_panics_on_wrong_type() {
    recorded_cron_origin_channel(&json!({"originChannelKey": []}));
}

fn recorded_cron_text_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("text"),
        "delivery.text",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-cron-text"),
    )
}

fn recorded_cron_text(value: &Value) -> Option<&str> {
    recorded_cron_text_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_cron_text_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_cron_text_checked(&json!({"text": []})).is_err(),
        "#1339 #1729 rejects delivery.text type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 delivery.text must be a string")]
fn recorded_cron_text_panics_on_wrong_type() {
    recorded_cron_text(&json!({"text": []}));
}

fn recorded_turn_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        value.get("turn").filter(|value| !value.is_null()),
        "turn",
        "an object",
        |value| value.is_object().then_some(value),
        false,
        crate::mutants::on("sms-oracle-recorded-turn"),
    )
}

fn recorded_turn(value: &Value) -> Option<&Value> {
    recorded_turn_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_turn_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_turn_checked(&json!({"turn": []})).is_err(),
        "#1339 #1729 rejects turn type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 turn must be an object")]
fn recorded_turn_panics_on_wrong_type() {
    recorded_turn(&json!({"turn": []}));
}

fn recorded_turn_gate_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("gate"),
        "turn.gate",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-turn-gate"),
    )
}

fn recorded_turn_gate(value: &Value) -> Option<&str> {
    recorded_turn_gate_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_turn_gate_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_turn_gate_checked(&json!({"gate": []})).is_err(),
        "#1339 #1729 rejects turn.gate type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 turn.gate must be a string")]
fn recorded_turn_gate_panics_on_wrong_type() {
    recorded_turn_gate(&json!({"gate": []}));
}

fn recorded_turn_error_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("error"),
        "turn.error",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-turn-error"),
    )
}

fn recorded_turn_error(value: &Value) -> Option<&str> {
    recorded_turn_error_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_turn_error_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_turn_error_checked(&json!({"error": []})).is_err(),
        "#1339 #1729 rejects turn.error type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 turn.error must be a string")]
fn recorded_turn_error_panics_on_wrong_type() {
    recorded_turn_error(&json!({"error": []}));
}

fn recorded_turn_events_checked(value: &Value) -> Result<Option<&Vec<Value>>, String> {
    recorded_checked(
        value.get("events"),
        "turn.events",
        "an array",
        Value::as_array,
        false,
        crate::mutants::on("sms-oracle-recorded-turn-events"),
    )
}

fn recorded_turn_events(value: &Value) -> Option<&Vec<Value>> {
    recorded_turn_events_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_turn_events_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_turn_events_checked(&json!({"events": false})).is_err(),
        "#1339 #1729 rejects turn.events type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 turn.events must be an array")]
fn recorded_turn_events_panics_on_wrong_type() {
    recorded_turn_events(&json!({"events": false}));
}

fn recorded_turn_hold_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("holdAfter"),
        "turn.holdAfter",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-turn-hold"),
    )
}

fn recorded_turn_hold(value: &Value) -> Option<&str> {
    recorded_turn_hold_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_turn_hold_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_turn_hold_checked(&json!({"holdAfter": []})).is_err(),
        "#1339 #1729 rejects turn.holdAfter type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 turn.holdAfter must be a string")]
fn recorded_turn_hold_panics_on_wrong_type() {
    recorded_turn_hold(&json!({"holdAfter": []}));
}

fn recorded_permission_error_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value
            .get("permissionError")
            .filter(|value| !value.is_null()),
        "permissionError",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-permission-error"),
    )
}

fn recorded_permission_error(value: &Value) -> Option<&str> {
    recorded_permission_error_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_permission_error_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_permission_error_checked(&json!({"permissionError": []})).is_err(),
        "#1339 #1729 rejects permissionError type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 permissionError must be a string")]
fn recorded_permission_error_panics_on_wrong_type() {
    recorded_permission_error(&json!({"permissionError": []}));
}

fn recorded_permission_gate_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("permissionGate").filter(|value| !value.is_null()),
        "permissionGate",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-permission-gate"),
    )
}

fn recorded_permission_gate(value: &Value) -> Option<&str> {
    recorded_permission_gate_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_permission_gate_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_permission_gate_checked(&json!({"permissionGate": []})).is_err(),
        "#1339 #1729 rejects permissionGate type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 permissionGate must be a string")]
fn recorded_permission_gate_panics_on_wrong_type() {
    recorded_permission_gate(&json!({"permissionGate": []}));
}

fn recorded_permission_delivered_checked(value: &Value) -> Result<Option<bool>, String> {
    recorded_checked(
        value.get("permissionDelivered"),
        "permissionDelivered",
        "a boolean",
        Value::as_bool,
        false,
        crate::mutants::on("sms-oracle-recorded-permission-delivered"),
    )
}

fn recorded_permission_delivered(value: &Value) -> Option<bool> {
    recorded_permission_delivered_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_permission_delivered_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_permission_delivered_checked(&json!({"permissionDelivered": []})).is_err(),
        "#1339 #1729 rejects permissionDelivered type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 permissionDelivered must be a boolean")]
fn recorded_permission_delivered_panics_on_wrong_type() {
    recorded_permission_delivered(&json!({"permissionDelivered": []}));
}

fn recorded_expected_checked(value: &Value) -> Result<Option<&Value>, String> {
    // #1729 fix-r6: the terminal comparison container is required, same as the
    // webhook harness. Absence must fail here with a field-level diagnostic
    // instead of becoming `Value::Null` in `assert_responses_so_far`.
    recorded_checked(
        value.get("expected"),
        "expected",
        "an object",
        |value| value.is_object().then_some(value),
        true,
        crate::mutants::on("sms-oracle-recorded-expected"),
    )
}

fn recorded_expected(value: &Value) -> Option<&Value> {
    recorded_expected_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_expected_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_expected_checked(&json!({"expected": []})).is_err(),
        "#1339 #1729 rejects expected type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 expected must be an object")]
fn recorded_expected_panics_on_wrong_type() {
    recorded_expected(&json!({"expected": []}));
}

#[test]
#[should_panic(expected = "#1339 expected must be an object, got <missing>")]
fn recorded_expected_panics_when_absent() {
    recorded_expected(&json!({}));
}

fn recorded_responses_checked(value: &Value) -> Result<Option<&Vec<Value>>, String> {
    recorded_checked(
        value.get("responses"),
        "expected.responses",
        "an array",
        Value::as_array,
        false,
        crate::mutants::on("sms-oracle-recorded-responses"),
    )
}

fn recorded_responses(value: &Value) -> Option<&Vec<Value>> {
    recorded_responses_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_responses_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_responses_checked(&json!({"responses": false})).is_err(),
        "#1339 #1729 rejects expected.responses type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 expected.responses must be an array")]
fn recorded_responses_panics_on_wrong_type() {
    recorded_responses(&json!({"responses": false}));
}

fn recorded_env_map_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        value.get("env"),
        "env",
        "an object",
        |value| value.is_object().then_some(value),
        false,
        crate::mutants::on("sms-oracle-recorded-env-map"),
    )
}

fn recorded_env_map(value: &Value) -> Option<&Value> {
    recorded_env_map_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_env_map_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_env_map_checked(&json!({"env": []})).is_err(),
        "#1339 #1729 rejects env type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 env must be an object")]
fn recorded_env_map_panics_on_wrong_type() {
    recorded_env_map(&json!({"env": []}));
}

fn recorded_steps_checked(value: &Value) -> Result<Option<&Vec<Value>>, String> {
    let steps = recorded_checked(
        value.get("steps"),
        "steps",
        "an array",
        Value::as_array,
        true,
        crate::mutants::on("sms-oracle-recorded-steps"),
    )?;
    // #1729 fix-r3: an empty array is as blind as an absent one — the replay
    // still runs the start/cleanup envelope and would compare that bare
    // snapshot, so a case that lost its steps must fail here instead of
    // shrinking to zero-coverage.
    if matches!(steps, Some(steps) if steps.is_empty()) {
        return Err(
            "#1339 steps must not be empty (a zero-step replay proves nothing)".to_string(),
        );
    }
    Ok(steps)
}

fn recorded_steps(value: &Value) -> Option<&Vec<Value>> {
    recorded_steps_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_steps_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_steps_checked(&json!({"steps": false})).is_err(),
        "#1339 #1729 rejects steps type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 steps must be an array")]
fn recorded_steps_panics_on_wrong_type() {
    recorded_steps(&json!({"steps": false}));
}

fn recorded_half_close_checked(value: &Value) -> Result<Option<bool>, String> {
    recorded_checked(
        value.get("halfClose"),
        "halfClose",
        "a boolean",
        Value::as_bool,
        false,
        crate::mutants::on("sms-oracle-recorded-half-close"),
    )
}

fn recorded_half_close(value: &Value) -> Option<bool> {
    recorded_half_close_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_half_close_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_half_close_checked(&json!({"halfClose": []})).is_err(),
        "#1339 #1729 rejects halfClose type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 halfClose must be a boolean")]
fn recorded_half_close_panics_on_wrong_type() {
    recorded_half_close(&json!({"halfClose": []}));
}

fn recorded_log_level_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("level"),
        "wait-log.level",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-log-level"),
    )
}

fn recorded_log_level(value: &Value) -> Option<&str> {
    recorded_log_level_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_log_level_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_log_level_checked(&json!({"level": []})).is_err(),
        "#1339 #1729 rejects wait-log.level type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 wait-log.level must be a string")]
fn recorded_log_level_panics_on_wrong_type() {
    recorded_log_level(&json!({"level": []}));
}

fn recorded_log_text_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("text"),
        "wait-log.text",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-log-text"),
    )
}

fn recorded_log_text(value: &Value) -> Option<&str> {
    recorded_log_text_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_log_text_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_log_text_checked(&json!({"text": []})).is_err(),
        "#1339 #1729 rejects wait-log.text type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 wait-log.text must be a string")]
fn recorded_log_text_panics_on_wrong_type() {
    recorded_log_text(&json!({"text": []}));
}

fn recorded_sends_count_checked(value: &Value) -> Result<Option<u64>, String> {
    recorded_checked(
        value.get("count"),
        "wait-sends.count",
        "a u64",
        Value::as_u64,
        false,
        crate::mutants::on("sms-oracle-recorded-sends-count"),
    )
}

fn recorded_sends_count(value: &Value) -> Option<u64> {
    recorded_sends_count_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_sends_count_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_sends_count_checked(&json!({"count": []})).is_err(),
        "#1339 #1729 rejects wait-sends.count type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 wait-sends.count must be a u64")]
fn recorded_sends_count_panics_on_wrong_type() {
    recorded_sends_count(&json!({"count": []}));
}

fn recorded_rpcs_count_checked(value: &Value) -> Result<Option<u64>, String> {
    recorded_checked(
        value.get("count"),
        "wait-rpcs.count",
        "a u64",
        Value::as_u64,
        false,
        crate::mutants::on("sms-oracle-recorded-rpcs-count"),
    )
}

fn recorded_rpcs_count(value: &Value) -> Option<u64> {
    recorded_rpcs_count_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_rpcs_count_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_rpcs_count_checked(&json!({"count": []})).is_err(),
        "#1339 #1729 rejects wait-rpcs.count type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 wait-rpcs.count must be a u64")]
fn recorded_rpcs_count_panics_on_wrong_type() {
    recorded_rpcs_count(&json!({"count": []}));
}

fn recorded_stop_reason_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("reason"),
        "stop.reason",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-stop-reason"),
    )
}

fn recorded_stop_reason(value: &Value) -> Option<&str> {
    recorded_stop_reason_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_stop_reason_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_stop_reason_checked(&json!({"reason": []})).is_err(),
        "#1339 #1729 rejects stop.reason type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 stop.reason must be a string")]
fn recorded_stop_reason_panics_on_wrong_type() {
    recorded_stop_reason(&json!({"reason": []}));
}

fn recorded_send_block_checked(value: &Value) -> Result<Option<u64>, String> {
    recorded_checked(
        value.get("sendBlockAt").filter(|value| !value.is_null()),
        "sendBlockAt",
        "a u64",
        Value::as_u64,
        false,
        crate::mutants::on("sms-oracle-recorded-send-block"),
    )
}

fn recorded_send_block(value: &Value) -> Option<u64> {
    recorded_send_block_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_send_block_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_send_block_checked(&json!({"sendBlockAt": []})).is_err(),
        "#1339 #1729 rejects sendBlockAt type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 sendBlockAt must be a u64")]
fn recorded_send_block_panics_on_wrong_type() {
    recorded_send_block(&json!({"sendBlockAt": []}));
}

fn recorded_send_fail_checked(value: &Value) -> Result<Option<&Vec<Value>>, String> {
    recorded_checked(
        value.get("sendFailAt"),
        "sendFailAt",
        "an array",
        Value::as_array,
        false,
        crate::mutants::on("sms-oracle-recorded-send-fail"),
    )
}

fn recorded_send_fail(value: &Value) -> Option<&Vec<Value>> {
    recorded_send_fail_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_send_fail_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_send_fail_checked(&json!({"sendFailAt": false})).is_err(),
        "#1339 #1729 rejects sendFailAt type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 sendFailAt must be an array")]
fn recorded_send_fail_panics_on_wrong_type() {
    recorded_send_fail(&json!({"sendFailAt": false}));
}

fn recorded_send_error_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("sendError").filter(|value| !value.is_null()),
        "sendError",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-send-error"),
    )
}

fn recorded_send_error(value: &Value) -> Option<&str> {
    recorded_send_error_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_send_error_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_send_error_checked(&json!({"sendError": []})).is_err(),
        "#1339 #1729 rejects sendError type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 sendError must be a string")]
fn recorded_send_error_panics_on_wrong_type() {
    recorded_send_error(&json!({"sendError": []}));
}

fn recorded_extra_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        value.get("extra"),
        "extra",
        "an object",
        |value| value.is_object().then_some(value),
        false,
        crate::mutants::on("sms-oracle-recorded-extra"),
    )
}

fn recorded_extra(value: &Value) -> Option<&Value> {
    recorded_extra_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_extra_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_extra_checked(&json!({"extra": []})).is_err(),
        "#1339 #1729 rejects extra type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 extra must be an object")]
fn recorded_extra_panics_on_wrong_type() {
    recorded_extra(&json!({"extra": []}));
}

fn recorded_headers_checked(step: &Value) -> Result<Vec<(String, String)>, String> {
    let mut headers = Vec::new();
    if let Some(object) = recorded_header_map_checked(step)?.and_then(Value::as_object) {
        for (key, value) in object {
            let text = recorded_checked(
                Some(value),
                &format!("headers.{key}"),
                "a string",
                Value::as_str,
                false,
                crate::mutants::on("sms-oracle-header-value"),
            )?
            .unwrap_or_default();
            headers.push((key.clone(), text.to_owned()));
        }
    }
    Ok(headers)
}

fn recorded_headers(step: &Value) -> Vec<(String, String)> {
    recorded_headers_checked(step).unwrap_or_else(|error| panic!("{error}"))
}

const REPLAY_BODY_BUDGET: usize = MAX_BODY_BYTES + 1;

fn recorded_body_checked(step: &Value) -> Result<Vec<u8>, String> {
    let (text, count) = match step.get("body") {
        None => return Ok(Vec::new()),
        Some(Value::String(text)) => (text.as_str(), 1),
        Some(spec @ Value::Object(_)) => (
            recorded_body_repeat_checked(spec)?.unwrap_or_default(),
            recorded_body_count_checked(spec)?.unwrap_or_default(),
        ),
        Some(value) => {
            if crate::mutants::on("sms-oracle-body-type") {
                return Ok(Vec::new());
            }
            return Err(format!(
                "#1339 body must be a string or repeat object, got {value}"
            ));
        }
    };
    // Validate UTF-8 bytes and multiplication before either cloning or repeating.
    let count = usize::try_from(count)
        .map_err(|_| format!("#1339 body.count does not fit usize, got {count}"))?;
    let length = text.len().checked_mul(count).ok_or_else(|| {
        format!("#1339 body byte length overflow: repeat={text:?}, count={count}")
    })?;
    if length > REPLAY_BODY_BUDGET && !crate::mutants::on("sms-oracle-body-budget") {
        return Err(format!(
            "#1339 body exceeds replay budget {REPLAY_BODY_BUDGET}: repeat={text:?}, count={count}, bytes={length}"
        ));
    }
    Ok(text.repeat(count).into_bytes())
}

fn recorded_body(step: &Value) -> Vec<u8> {
    recorded_body_checked(step).unwrap_or_else(|error| panic!("{error}"))
}

fn assert_boundary_body_len_checked(name: &str, case: &Value) -> Result<(), String> {
    let expected = match name {
        "payload-exactly-1mib" => MAX_BODY_BYTES,
        "payload-too-large" => MAX_BODY_BYTES + 1,
        _ => return Ok(()),
    };
    let lengths = recorded_steps_checked(case)?
        .into_iter()
        .flatten()
        .filter(|step| step.get("op").and_then(Value::as_str) == Some("request"))
        .map(|step| recorded_body_checked(step).map(|body| body.len()))
        .collect::<Result<Vec<_>, _>>()?;
    if lengths != [expected] && !crate::mutants::on("sms-oracle-boundary-body-length") {
        return Err(format!(
            "#1339 {name} body boundary must be [{expected}], got {lengths:?}"
        ));
    }
    Ok(())
}

fn assert_boundary_body_len(name: &str, case: &Value) {
    assert_boundary_body_len_checked(name, case).unwrap_or_else(|error| panic!("{error}"));
}

fn build_raw(step: &Value) -> Vec<u8> {
    let body = recorded_body(step);
    let mut raw = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n",
        recorded_method(step).unwrap_or("POST"),
        recorded_path(step).unwrap_or("/sms/incoming"),
    )
    .into_bytes();
    for (key, value) in recorded_headers(step) {
        raw.extend_from_slice(format!("{key}: {value}\r\n").as_bytes());
    }
    raw.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    let head_len = raw.len();
    raw.extend_from_slice(&body);
    // Truncate body bytes only, preserving the recorded Content-Length.
    let truncate =
        usize::try_from(recorded_truncate(step).unwrap_or_default()).unwrap_or(usize::MAX);
    raw.truncate(head_len + body.len().saturating_sub(truncate));
    raw
}

fn cron_delivery(value: &Value) -> CronResultDelivery {
    CronResultDelivery {
        session_key: recorded_cron_session(value).map(JsString::from),
        origin_session_key: recorded_cron_origin_session(value).map(JsString::from),
        text: JsString::from(recorded_cron_text(value).unwrap_or_default()),
        origin_channel_key: recorded_cron_origin_channel(value).map(JsString::from),
    }
}

fn recorded_env_checked(case: &Value) -> Result<HashMap<String, String>, String> {
    let mut env = HashMap::new();
    if let Some(object) = recorded_env_map_checked(case)?.and_then(Value::as_object) {
        for (key, value) in object {
            let text = recorded_checked(
                Some(value),
                &format!("env.{key}"),
                "a string",
                Value::as_str,
                false,
                crate::mutants::on("sms-oracle-env-value"),
            )?
            .unwrap_or_default();
            env.insert(key.clone(), text.to_owned());
        }
    }
    Ok(env)
}

fn recorded_env(case: &Value) -> HashMap<String, String> {
    recorded_env_checked(case).unwrap_or_else(|error| panic!("{error}"))
}

fn recorded_send_fail_at_checked(case: &Value) -> Result<Vec<u64>, String> {
    recorded_send_fail_checked(case)?
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, value)| {
            recorded_checked(
                Some(value),
                &format!("sendFailAt[{index}]"),
                "a u64",
                Value::as_u64,
                false,
                crate::mutants::on("sms-oracle-send-fail-entry"),
            )
            .map(|at| at.unwrap_or_default())
        })
        .collect()
}

fn recorded_send_fail_at(case: &Value) -> Vec<u64> {
    recorded_send_fail_at_checked(case).unwrap_or_else(|error| panic!("{error}"))
}

// Validate mock configuration before spawning channel tasks: a malformed
// recording must fail this test, not only panic in a detached mock future.
fn validate_mock_recording(case: &Value) {
    let turn = recorded_turn(case).unwrap_or(&Value::Null);
    recorded_turn_gate(turn);
    recorded_turn_error(turn);
    recorded_turn_events(turn);
    recorded_turn_hold(turn);
    recorded_permission_error(case);
    recorded_permission_gate(case);
    recorded_permission_delivered(case);
    recorded_send_block(case);
    recorded_send_fail_at(case);
    recorded_send_error(case);
}

#[test]
fn recorded_header_value_rejects_drift() {
    crate::negative_assert!(
        recorded_headers_checked(&json!({"headers":{"x-test":7}})).is_err(),
        "#1339 #1729 rejects header_value drift"
    );
}

#[test]
#[should_panic(expected = "#1339 headers.x-test must be a string")]
fn recorded_header_value_panics_on_drift() {
    recorded_headers(&json!({"headers":{"x-test":7}}));
}

#[test]
fn recorded_body_type_rejects_drift() {
    crate::negative_assert!(
        recorded_body_checked(&json!({"body":null})).is_err(),
        "#1339 #1729 rejects body_type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 body must be a string or repeat object")]
fn recorded_body_type_panics_on_drift() {
    recorded_body(&json!({"body":null}));
}

#[test]
fn recorded_body_budget_rejects_drift() {
    crate::negative_assert!(
        recorded_body_checked(&json!({"body":{"repeat":"x","count":REPLAY_BODY_BUDGET+1}}))
            .is_err(),
        "#1339 #1729 rejects body_budget drift"
    );
}

#[test]
#[should_panic(expected = "#1339 body exceeds replay budget")]
fn recorded_body_budget_panics_on_drift() {
    recorded_body(&json!({"body":{"repeat":"x","count":REPLAY_BODY_BUDGET+1}}));
}

#[test]
fn recorded_boundary_body_length_rejects_drift() {
    crate::negative_assert!(
        assert_boundary_body_len_checked(
            "payload-exactly-1mib",
            &json!({"steps":[{"op":"request","body":"x"}]})
        )
        .is_err(),
        "#1339 #1729 rejects boundary_body_length drift"
    );
}

#[test]
#[should_panic(expected = "#1339 payload-exactly-1mib body boundary")]
fn recorded_boundary_body_length_panics_on_drift() {
    assert_boundary_body_len(
        "payload-exactly-1mib",
        &json!({"steps":[{"op":"request","body":"x"}]}),
    );
}

#[test]
fn recorded_env_value_rejects_drift() {
    crate::negative_assert!(
        recorded_env_checked(&json!({"env":{"TOKEN":7}})).is_err(),
        "#1339 #1729 rejects env_value drift"
    );
}

#[test]
#[should_panic(expected = "#1339 env.TOKEN must be a string")]
fn recorded_env_value_panics_on_drift() {
    recorded_env(&json!({"env":{"TOKEN":7}}));
}

#[test]
fn recorded_send_fail_entry_rejects_drift() {
    crate::negative_assert!(
        recorded_send_fail_at_checked(&json!({"sendFailAt":[null]})).is_err(),
        "#1339 #1729 rejects send_fail_entry drift"
    );
}

#[test]
#[should_panic(expected = "#1339 sendFailAt[0] must be a u64")]
fn recorded_send_fail_entry_panics_on_drift() {
    recorded_send_fail_at(&json!({"sendFailAt":[null]}));
}

#[test]
fn absent_recording_fields_keep_defaults() {
    let empty = json!({});
    assert!(recorded_headers(&empty).is_empty(), "#1339 absent headers");
    assert!(recorded_env(&empty).is_empty(), "#1339 absent env");
    assert!(recorded_body(&empty).is_empty(), "#1339 absent body");
    assert_eq!(build_raw(&empty), b"POST /sms/incoming HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n", "#1339 absent request fields");
    validate_mock_recording(&empty);
    // These five nulls are explicit absence sentinels emitted by generate.mts.
    validate_mock_recording(
        &json!({"turn":null,"permissionError":null,"permissionGate":null,"sendBlockAt":null,"sendError":null}),
    );
    assert_responses_so_far(&[], &empty, "absent expected");
    let delivery = cron_delivery(&empty);
    assert!(
        delivery.session_key.is_none()
            && delivery.origin_session_key.is_none()
            && delivery.origin_channel_key.is_none(),
        "#1339 absent cron keys"
    );
    assert!(delivery.text.equals_str(""), "#1339 absent cron text");
    assert!(
        recorded_header_map(&empty).is_none(),
        "#1339 absent headers"
    );
    assert!(
        recorded_header_map(&json!({"headers":{}})).is_some(),
        "#1339 valid headers"
    );
    assert!(recorded_method(&empty).is_none(), "#1339 absent method");
    assert!(
        recorded_method(&json!({"method":"value"})).is_some(),
        "#1339 valid method"
    );
    assert!(recorded_path(&empty).is_none(), "#1339 absent path");
    assert!(
        recorded_path(&json!({"path":"value"})).is_some(),
        "#1339 valid path"
    );
    assert!(
        recorded_body_repeat_checked(&empty).is_err(),
        "#1339 missing body.repeat"
    );
    assert!(
        recorded_body_repeat(&json!({"repeat":"x"})).is_some(),
        "#1339 valid body.repeat"
    );
    assert!(
        recorded_body_count_checked(&empty).is_err(),
        "#1339 missing body.count"
    );
    assert!(
        recorded_body_count(&json!({"count":1})).is_some(),
        "#1339 valid body.count"
    );
    assert!(recorded_truncate(&empty).is_none(), "#1339 absent truncate");
    assert!(
        recorded_truncate(&json!({"truncate":1})).is_some(),
        "#1339 valid truncate"
    );
    assert!(recorded_delivery(&empty).is_none(), "#1339 absent delivery");
    assert!(
        recorded_delivery(&json!({"delivery":{}})).is_some(),
        "#1339 valid delivery"
    );
    assert!(
        recorded_cron_session(&empty).is_none(),
        "#1339 absent delivery.sessionKey"
    );
    assert!(
        recorded_cron_session(&json!({"sessionKey":"value"})).is_some(),
        "#1339 valid delivery.sessionKey"
    );
    assert!(
        recorded_cron_origin_session(&empty).is_none(),
        "#1339 absent delivery.originSessionKey"
    );
    assert!(
        recorded_cron_origin_session(&json!({"originSessionKey":"value"})).is_some(),
        "#1339 valid delivery.originSessionKey"
    );
    assert!(
        recorded_cron_origin_channel(&empty).is_none(),
        "#1339 absent delivery.originChannelKey"
    );
    assert!(
        recorded_cron_origin_channel(&json!({"originChannelKey":"value"})).is_some(),
        "#1339 valid delivery.originChannelKey"
    );
    assert!(
        recorded_cron_text(&empty).is_none(),
        "#1339 absent delivery.text"
    );
    assert!(
        recorded_cron_text(&json!({"text":"value"})).is_some(),
        "#1339 valid delivery.text"
    );
    assert!(recorded_turn(&empty).is_none(), "#1339 absent turn");
    assert!(
        recorded_turn(&json!({"turn":{}})).is_some(),
        "#1339 valid turn"
    );
    assert!(
        recorded_turn_gate(&empty).is_none(),
        "#1339 absent turn.gate"
    );
    assert!(
        recorded_turn_gate(&json!({"gate":"value"})).is_some(),
        "#1339 valid turn.gate"
    );
    assert!(
        recorded_turn_error(&empty).is_none(),
        "#1339 absent turn.error"
    );
    assert!(
        recorded_turn_error(&json!({"error":"value"})).is_some(),
        "#1339 valid turn.error"
    );
    assert!(
        recorded_turn_events(&empty).is_none(),
        "#1339 absent turn.events"
    );
    assert!(
        recorded_turn_events(&json!({"events":[]})).is_some(),
        "#1339 valid turn.events"
    );
    assert!(
        recorded_turn_hold(&empty).is_none(),
        "#1339 absent turn.holdAfter"
    );
    assert!(
        recorded_turn_hold(&json!({"holdAfter":"value"})).is_some(),
        "#1339 valid turn.holdAfter"
    );
    assert!(
        recorded_permission_error(&empty).is_none(),
        "#1339 absent permissionError"
    );
    assert!(
        recorded_permission_error(&json!({"permissionError":"value"})).is_some(),
        "#1339 valid permissionError"
    );
    assert!(
        recorded_permission_gate(&empty).is_none(),
        "#1339 absent permissionGate"
    );
    assert!(
        recorded_permission_gate(&json!({"permissionGate":"value"})).is_some(),
        "#1339 valid permissionGate"
    );
    assert!(
        recorded_permission_delivered(&empty).is_none(),
        "#1339 absent permissionDelivered"
    );
    assert!(
        recorded_permission_delivered(&json!({"permissionDelivered":true})).is_some(),
        "#1339 valid permissionDelivered"
    );
    // #1729 fix-r6: `expected` is required (absent = recording drift); the
    // wrapper panics with the field-level diagnostic.
    assert!(
        recorded_expected_checked(&empty).is_err(),
        "#1339 expected is a required recording field"
    );
    assert!(
        recorded_expected(&json!({"expected":{}})).is_some(),
        "#1339 valid expected"
    );
    assert!(
        recorded_responses(&empty).is_none(),
        "#1339 absent expected.responses"
    );
    assert!(
        recorded_responses(&json!({"responses":[]})).is_some(),
        "#1339 valid expected.responses"
    );
    assert!(recorded_env_map(&empty).is_none(), "#1339 absent env");
    assert!(
        recorded_env_map(&json!({"env":{}})).is_some(),
        "#1339 valid env"
    );
    assert!(
        recorded_steps_checked(&empty).is_err(),
        "#1339 missing steps"
    );
    assert!(
        recorded_steps_checked(&json!({"steps":[]})).is_err(),
        "#1339 empty steps cannot replay"
    );
    assert!(
        recorded_half_close(&empty).is_none(),
        "#1339 absent halfClose"
    );
    assert!(
        recorded_half_close(&json!({"halfClose":true})).is_some(),
        "#1339 valid halfClose"
    );
    assert!(
        recorded_log_level(&empty).is_none(),
        "#1339 absent wait-log.level"
    );
    assert!(
        recorded_log_level(&json!({"level":"value"})).is_some(),
        "#1339 valid wait-log.level"
    );
    assert!(
        recorded_log_text(&empty).is_none(),
        "#1339 absent wait-log.text"
    );
    assert!(
        recorded_log_text(&json!({"text":"value"})).is_some(),
        "#1339 valid wait-log.text"
    );
    assert!(
        recorded_sends_count(&empty).is_none(),
        "#1339 absent wait-sends.count"
    );
    assert!(
        recorded_sends_count(&json!({"count":1})).is_some(),
        "#1339 valid wait-sends.count"
    );
    assert!(
        recorded_rpcs_count(&empty).is_none(),
        "#1339 absent wait-rpcs.count"
    );
    assert!(
        recorded_rpcs_count(&json!({"count":1})).is_some(),
        "#1339 valid wait-rpcs.count"
    );
    assert!(
        recorded_stop_reason(&empty).is_none(),
        "#1339 absent stop.reason"
    );
    assert!(
        recorded_stop_reason(&json!({"reason":"value"})).is_some(),
        "#1339 valid stop.reason"
    );
    assert!(
        recorded_send_block(&empty).is_none(),
        "#1339 absent sendBlockAt"
    );
    assert!(
        recorded_send_block(&json!({"sendBlockAt":1})).is_some(),
        "#1339 valid sendBlockAt"
    );
    assert!(
        recorded_send_fail(&empty).is_none(),
        "#1339 absent sendFailAt"
    );
    assert!(
        recorded_send_fail(&json!({"sendFailAt":[]})).is_some(),
        "#1339 valid sendFailAt"
    );
    assert!(
        recorded_send_error(&empty).is_none(),
        "#1339 absent sendError"
    );
    assert!(
        recorded_send_error(&json!({"sendError":"value"})).is_some(),
        "#1339 valid sendError"
    );
    assert!(recorded_extra(&empty).is_none(), "#1339 absent extra");
    assert!(
        recorded_extra(&json!({"extra":{}})).is_some(),
        "#1339 valid extra"
    );
}

#[test]
fn body_expansion_pins_bytes_and_fixture_boundaries() {
    assert_eq!(
        recorded_body(&json!({"body":{"repeat":"é","count":2}})),
        "éé".as_bytes(),
        "#1339 repeat counts UTF-8 bytes"
    );
    assert_eq!(
        recorded_body(&json!({"body":{"repeat":"x","count":0}})),
        b"",
        "#1339 zero repeat count is legal"
    );
    assert!(
        recorded_body_checked(&json!({"body":{"repeat":"é","count":REPLAY_BODY_BUDGET}})).is_err(),
        "#1339 multibyte repeat budget"
    );
    assert!(
        recorded_body_checked(&json!({"body":{"repeat":"xx","count":u64::MAX}})).is_err(),
        "#1339 multiplication overflow before allocation"
    );
    assert!(
        recorded_body_checked(&json!({"body":{"repeat":"","count":1}})).is_err(),
        "#1339 empty repeat rejected"
    );
    assert!(
        recorded_body_checked(&json!({"body":{"repeat":"x"}})).is_err(),
        "#1339 missing repeat count rejected"
    );
    for name in ["payload-exactly-1mib", "payload-too-large"] {
        assert_boundary_body_len(name, &case_by_name(name));
    }
    assert_eq!(build_raw(&json!({"body":"abc","truncate":100})), b"POST /sms/incoming HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 3\r\n\r\n", "#1339 truncation preserves headers");
}

#[test]
#[should_panic(expected = "#1339 body byte length overflow")]
fn body_overflow_panics_before_allocation() {
    recorded_body(&json!({"body":{"repeat":"xx","count":u64::MAX}}));
}

#[test]
#[should_panic(expected = "#1339 steps must be an array, got <missing>")]
fn missing_steps_cannot_be_zero_coverage() {
    recorded_steps(&json!({}));
}

#[test]
#[should_panic(expected = "#1339 steps must not be empty")]
fn empty_steps_cannot_be_zero_coverage() {
    recorded_steps(&json!({"steps":[]}));
}

// ---------------------------------------------------------------------------
// replay driver
// ---------------------------------------------------------------------------

/// #1339 fix-r1 (P2 #13): a replay needs its case port free — a second
/// channel bound to it would answer the probe requests and corrupt the
/// recorded oracle bytes. Fail fast with a dedicated message instead of
/// letting the run mismatch. (The switch keeps the guard testable.)
async fn ensure_replay_port_free(port: u16) {
    if crate::mutants::on("sms-port-preflight") {
        return;
    }
    // #1339 fix-r6 (P2): ask "is something listening?" by *connecting*, not by
    // binding. `TcpListener::bind` sets `SO_REUSEADDR`, which on macOS/BSD
    // reports a port still held by a wildcard listener as free — the very
    // contamination this guard exists to stop — while the option-free bind used
    // by the product collides with the `TIME_WAIT` the previous replay of the
    // same case leaves behind and fails every later run. A connect probe has
    // neither problem: a live listener answers, a lingering `TIME_WAIT` socket
    // refuses.
    //
    // #1339 fix-r9 (P0, Codex): the IPv4 probe alone could not see an IPv6
    // occupier on the same port, and that is the shape the guard must catch —
    // measured on this host (`_tmp/20261106-issue-1339-fix-r6/
    // v6only_probe.log`): with `[::]:P` held by a v6-only listener, a v4
    // connect to `127.0.0.1:P` is refused, our `::` bind gets `EADDRINUSE`,
    // `v4_port_state` reports `Unshared`, and `bind_listener` falls back to
    // `0.0.0.0:P` and serves every IPv4 probe the oracle records. The replay
    // then passes in an environment where the TS baseline cannot start at all,
    // i.e. a silently weaker check.
    //
    // #1339 fix-r10 (P0, Codex + minimax boundary, two roles independently):
    // a `::1` *connect* probe was not enough. It misses a socket that is bound
    // to `[::]:P` but never listens, and on that shape the connect even hangs
    // instead of refusing (`v6_bind_probe.log` Q2: v4 refused, `::1` timed
    // out). It also folded "this host has no IPv6" together with "the probe
    // itself could not run", so a descriptor or permission failure silently
    // skipped the whole v6 half. The v6 half is therefore a *bind* probe with
    // `SO_REUSEADDR`, which asks the only question that matters: would the
    // product's own `bind(("::", port))` fail? Measured on the same host:
    //   Q1 v6-only listener on `[::]:P`        -> EADDRINUSE
    //   Q2 socket bound to `[::]:P`, no listen -> EADDRINUSE
    //   Q3 `TIME_WAIT` from the previous replay -> free
    //   Q4 third party on `[::1]:P` only        -> free (the product's
    //      wildcard bind succeeds there too, so it never falls back)
    // `SO_REUSEADDR` is what keeps Q3 free; an option-free bind — the shape
    // that broke every fix-r6 run — would collide with the previous replay's
    // `TIME_WAIT`.
    //
    // #1729 fix-r1: check the wildcard bind first. On macOS a socket bound
    // to `[::]:port` without listening can stall the IPv4 connect below;
    // the bind probe still reports that IPv6 is occupied immediately.
    match v6_wildcard_state(port) {
        V6Port::Free | V6Port::Absent => {}
        V6Port::Occupied => panic!(
            "#1339 port {port} already in use on IPv6; the `::` bind would fail and fall back to IPv4-only"
        ),
        V6Port::Unknown => {
            panic!("#1339 cannot prove the IPv6 wildcard port {port} is free; refusing to replay")
        }
    }

    // #1339 fix-r9 (P2, four roles): the IPv4 probe carries the same shape as
    // the generator's `portIsFree` — `bounded` caps the wait and
    // `ConnectionRefused` is the only proof of a free port.
    match bounded(
        "port guard 127.0.0.1",
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    {
        Ok(_) => panic!(
            "#1339 port {port} already in use; a concurrent channel-sms replay would corrupt the oracle"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Err(error) => {
            panic!("#1339 cannot prove port {port} is free on IPv4 ({error}); refusing to replay")
        }
    }
}

/// What a `::` bind learned about the IPv6 wildcard.
///
/// #1339 fix-r10 (P0): this is a *bind* probe on purpose, and it keeps
/// `SO_REUSEADDR` on purpose. The question is not "is the address unused" but
/// "would the product's own `bind(("::", port))` fail", so the probe must have
/// the same socket shape as `bind_dual_stack`; `std::net::TcpListener::bind`
/// sets `SO_REUSEADDR`, which also keeps it clear of the previous replay's
/// `TIME_WAIT` (`v6_bind_probe.log` Q1–Q4). Anything the probe cannot classify
/// is [`V6Port::Unknown`] and aborts the replay instead of being read as "no
/// IPv6 here" — the fail-open shape fix-r6 already removed from the v4 half.
enum V6Port {
    /// Nothing holds the wildcard; the product will bind it.
    Free,
    /// Something already holds `[::]:<port>` — listening or merely bound.
    Occupied,
    /// This host has no IPv6 stack at all. Not fail-open: the probe and the
    /// product issue the same `bind(("::", port))`, so an error the probe
    /// cannot attribute to a port owner is an error the product hits too and
    /// falls back to `0.0.0.0` on — exactly like the Node baseline.
    Absent,
    /// The probe could not run; the host's posture is unknown.
    Unknown,
}

/// Does this error mean "this host has no IPv6 address family"?
///
/// #1339 fix-r11 (P1, Codex): `EAFNOSUPPORT` — the error a kernel without
/// IPv6 returns from `socket(AF_INET6)` — is **not** `ErrorKind::Unsupported`.
/// Measured, not assumed (`_tmp/20261106-issue-1339-fix-r6/errno-kind.log`):
///
/// ```text
/// macOS  errno  47 -> kind Uncategorized  (Address family not supported by protocol family)
/// Linux  errno  97 -> kind Uncategorized  (Address family not supported by protocol)
/// macOS  errno  49 -> kind AddrNotAvailable
/// Linux  errno  99 -> kind AddrNotAvailable
/// ```
///
/// Relying on `ErrorKind` alone would push an IPv4-only host into
/// [`V6Port::Unknown`] and abort a replay the product would run fine (it falls
/// back to `0.0.0.0`). `libc` is already a dependency of this crate
/// (`src/node_io/generated.rs` uses `libc::EAFNOSUPPORT`).
///
/// #1339 fix-r12 (P1, minimax boundary): `ErrorKind::Unsupported` is **not**
/// evidence of "no IPv6" and was removed. On Unix it is reachable only from
/// `ENOSYS` and `EOPNOTSUPP` — neither is an address-family error. Measured
/// (`_tmp/20261106-issue-1339-fix-r6/errno-kind2.log`):
///
/// ```text
/// macOS  errno  78 (ENOSYS)      -> kind Unsupported
/// macOS  errno 102 (EOPNOTSUPP)  -> kind Unsupported
/// Linux  errno  38 (ENOSYS)      -> kind Unsupported
/// Linux  errno  95 (EOPNOTSUPP)  -> kind Unsupported
/// ```
///
/// Keeping that arm was fail-open: a sandbox that answers `socket(AF_INET6)`
/// with `ENOSYS` would be read as "this host has no IPv6", the v6 half of the
/// occupancy check would be skipped, and the replay could pass on a host where
/// someone else serves `[::]:<port>` — the exact shape fix-r10 closed. Every
/// error that is not provably "no IPv6" must stay [`V6Port::Unknown`].
fn means_no_ipv6(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::AddrNotAvailable
        || error.raw_os_error() == Some(libc::EAFNOSUPPORT)
}

fn v6_wildcard_state(port: u16) -> V6Port {
    match std::net::TcpListener::bind(("::", port)) {
        Ok(listener) => {
            drop(listener);
            V6Port::Free
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => V6Port::Occupied,
        Err(error) if means_no_ipv6(&error) => V6Port::Absent,
        Err(_) => V6Port::Unknown,
    }
}

/// #1339 fix-r12 (P2, Codex r11 #1 / minimax simplify r11 #2): pin the
/// classifier itself, not just the one arm a dual-stack host can reach.
///
/// Without this, deleting the `EAFNOSUPPORT` arm or re-adding a non-family arm
/// (`Unsupported` was one until fix-r12) leaves every existing test green on a
/// host that has IPv6, and only an IPv4-only host or a sandboxed runner would
/// notice — one direction a false pass, the other a false failure.
#[test]
fn means_no_ipv6_accepts_only_address_family_errors() {
    let err = std::io::Error::from_raw_os_error;
    // Address-family errors: this host really has no usable IPv6.
    assert!(means_no_ipv6(&err(libc::EAFNOSUPPORT)));
    assert!(means_no_ipv6(&err(libc::EADDRNOTAVAIL)));
    // Anything else — port taken, permission, descriptors, unimplemented
    // syscall — proves nothing about IPv6 and must abort the replay instead of
    // being read as "no IPv6 here" (fail-open).
    for raw in [
        libc::EADDRINUSE,
        libc::EACCES,
        libc::EMFILE,
        libc::ENOSYS,
        libc::EOPNOTSUPP,
    ] {
        assert!(
            !means_no_ipv6(&err(raw)),
            "errno {raw} must not be read as `no IPv6`"
        );
    }
}

/// #1339 fix-r2 (P1): compare the responses recorded so far against the
/// oracle's prefix after every `request` / `connect` step. A mutant that
/// rejects a request (a broken signature order or signing URL) never reaches
/// the final snapshot — the following `wait-sends` step times out first and
/// panics without the case's bound assertion, which the negative-control
/// suite judges INVALID instead of KILLED. Asserting as the responses land
/// keeps every divergence at the case's own anchor.
///
/// #1339 fix-r3 (P2): takes a snapshot instead of the live `Record`. Asserting
/// while holding the record mutex poisons it when the assertion fails; the
/// channel's logger then panics on `PoisonError` from a detached task, adding
/// an unnamed panic header that the negative-control contract rejects
/// (`tests/common/baseline.rs`), which turns a KILLED back into an INVALID.
fn assert_responses_so_far(responses: &[Value], case: &Value, name: &str) {
    let expected = recorded_responses(recorded_expected(case).unwrap_or(&Value::Null))
        .map(|values| values[..values.len().min(responses.len())].to_vec())
        .unwrap_or_default();
    crate::negative_assert_eq!(responses, &expected[..], "#1339 sms oracle: {name}");
}

async fn replay_named(name: &str) {
    let case = case_by_name(name);
    validate_mock_recording(&case);
    let steps = recorded_steps(&case).expect("#1339 steps validated array");
    assert_boundary_body_len(name, &case);
    let port = case["port"].as_u64().expect("#1339 case port") as u16;
    ensure_replay_port_free(port).await;
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
    let env_map = recorded_env(&case);
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
        extra: Some(to_js_object(recorded_extra(&case).unwrap_or(&Value::Null))),
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
            turn_gateway: None,
        })
        .await
        .expect("#1339 start");

    for step in steps {
        match step["op"].as_str().expect("#1339 step op") {
            "request" => {
                let raw = build_raw(step);
                let half_close = recorded_half_close(step).unwrap_or(false);
                let outcome = bounded("probe response", raw_request(port, &raw, half_close)).await;
                let seen = {
                    let mut record = record.lock().expect("#1339 record mutex");
                    record.responses.push(match outcome {
                        Ok(value) => value,
                        // The generator's connect refused mapping; the probe client
                        // reports ECONNREFUSED the same way for raw requests.
                        Err(_) => json!({"refused": true}),
                    });
                    record.responses.clone()
                };
                assert_responses_so_far(&seen, &case, name);
            }
            "connect" => {
                let refused = bounded("connect probe", TcpStream::connect(("127.0.0.1", port)))
                    .await
                    .is_err();
                let seen = {
                    let mut record = record.lock().expect("#1339 record mutex");
                    record.responses.push(json!({"refused": refused}));
                    record.responses.clone()
                };
                assert_responses_so_far(&seen, &case, name);
            }
            "wait-log" => {
                let level = recorded_log_level(step).unwrap_or_default().to_owned();
                let text = recorded_log_text(step).unwrap_or_default().to_owned();
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
                let count = recorded_sends_count(step).unwrap_or_default();
                until(&format!("sends {count}"), || {
                    record.lock().expect("#1339 record mutex").sends.len() as u64 >= count
                })
                .await;
            }
            "wait-rpcs" => {
                let count = recorded_rpcs_count(step).unwrap_or_default();
                until(&format!("rpcs {count}"), || {
                    record.lock().expect("#1339 record mutex").rpcs.len() as u64 >= count
                })
                .await;
            }
            "release" => {
                release_if_exists(&gates, step["gate"].as_str().expect("#1339 gate name"));
            }
            "settle" => settle().await,
            "stop" => {
                // TS `handle.stop(reason)` — an absent reason logs "no reason".
                let reason = recorded_stop_reason(step).unwrap_or("no reason");
                bounded("stop", handle.stop(reason))
                    .await
                    .expect("#1339 stop");
            }
            "cron" => {
                let delivery = cron_delivery(recorded_delivery(step).unwrap_or(&Value::Null));
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
    crate::negative_assert_eq!(
        snapshot,
        *recorded_expected(&case).expect("#1339 expected present"),
        "#1339 sms oracle: {name}"
    );
}

macro_rules! replay_cases {
    ($($case:literal => $test:ident),* $(,)?) => {
        /// #1339 fix-r3 (P2): the case names bound to replay cells, emitted by
        /// the same macro that defines the cells so the two cannot drift.
        const REPLAY_CELL_NAMES: &[&str] = &[$($case),*];
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
    // #1339 fix-r1: the seven cells the review round added.
    "valid-signature-query" => valid_signature_query,
    "json-body-number-empty-from" => json_body_number_empty_from,
    "permission-missing-request-id" => permission_missing_request_id,
    "elicitation-missing-request-id" => elicitation_missing_request_id,
    "empty-webhook-host" => empty_webhook_host,
    "start-host-unavailable" => start_host_unavailable,
    "body-truncated" => body_truncated,
}

#[test]
fn oracle_case_count_is_recorded() {
    assert_eq!(
        oracle_cases().len(),
        EXPECTED_CASE_COUNT,
        "#1339 oracle case array length"
    );
}

/// #1339 fix-r3 (P2): the ledger claims 54 of the 55 oracle cases are compared
/// against a replay cell on every run. The total is pinned by
/// `EXPECTED_CASE_COUNT` on both sides, but nothing tied the *names* together:
/// a case added without a cell would keep both counts in step and its
/// `expected` would silently stop being compared. Pin the name sets instead.
#[test]
fn every_oracle_case_has_a_replay_cell() {
    // The one case with no replay cell of its own: it only carries the port
    // that `port_busy_preflight_fails_fast` holds busy.
    const WITHOUT_CELL: &[&str] = &["port-busy-preflight"];
    let cases = oracle_cases();
    // #1339 fix-r4 (P2): never drop a case for lacking a name — a nameless
    // case would fall out of both sets and the comparison would pass while its
    // `expected` stopped being compared (`case_by_name` cannot find it either).
    let mut oracle: Vec<&str> = cases
        .iter()
        .map(|case| {
            case["name"]
                .as_str()
                .expect("#1339 every oracle case carries a string name")
        })
        .filter(|name| !WITHOUT_CELL.contains(name))
        .collect();
    let mut cells: Vec<&str> = REPLAY_CELL_NAMES.to_vec();
    oracle.sort_unstable();
    cells.sort_unstable();
    // Duplicates on either side would let a second `port-busy-preflight` (or a
    // duplicated cell name) mask a missing real case.
    for (side, names) in [("oracle", &oracle), ("replay cells", &cells)] {
        let mut unique = names.clone();
        unique.dedup();
        assert_eq!(
            unique.len(),
            names.len(),
            "#1339 duplicate {side} case name"
        );
    }
    assert_eq!(
        cells, oracle,
        "#1339 every oracle case needs exactly one replay cell"
    );
    // The exemption itself must stay exactly one case, or a second exempt case
    // could hide behind it.
    let exempt = cases
        .iter()
        .filter(|case| {
            case["name"]
                .as_str()
                .is_some_and(|name| WITHOUT_CELL.contains(&name))
        })
        .count();
    assert_eq!(exempt, WITHOUT_CELL.len(), "#1339 replay-cell exemptions");
}

/// #1339 fix-r5 (P1): `bind_dual_stack` must probe the port the kernel picked,
/// not the configured `0`. Probing `127.0.0.1:0` always succeeds, so every
/// `webhookPort: 0` start would be demoted to the IPv4-only fallback and no
/// other test in the crate would notice (`empty-webhook-host` uses a fixed
/// port, where both spellings behave alike).
#[tokio::test]
async fn dual_stack_probe_uses_the_assigned_port() {
    let listener = match bind_dual_stack(0).await {
        Ok(listener) => listener,
        Err(error) => {
            // Refusing is only right on a host that keeps `IPV6_V6ONLY` set;
            // prove the host really is IPv6-only before accepting that.
            // #1339 fix-r6 (P1): a host with IPv6 switched off reaches this
            // branch through the product's own IPv4 fallback, so a control
            // that panics there would fail the suite on a supported host.
            // Prove IPv4 still binds, then report the skip explicitly.
            let control = match TcpListener::bind(("::", 0)).await {
                Ok(control) => control,
                Err(control_error) => {
                    let v4 = TcpListener::bind(("0.0.0.0", 0)).await.unwrap_or_else(|error| {
                        panic!(
                            "#1339 neither `::` ({control_error}) nor `0.0.0.0` ({error}) binds; \
                             the IPv4 fallback this test leans on is unavailable"
                        )
                    });
                    drop(v4);
                    // #1339 fix-r7 (P1, Codex #2): prove the *product* fallback
                    // on this host, not just that `0.0.0.0` can bind. Going
                    // through `bind_listener` is what makes deleting the
                    // fallback fail here instead of passing silently.
                    let fallback = bind_listener("", 0)
                        .await
                        .expect("#1339 IPv4 fallback on a host without IPv6");
                    let fallback_address = fallback.local_addr().expect("#1339 fallback address");
                    assert!(
                        fallback_address.is_ipv4() && fallback_address.ip().is_unspecified(),
                        "#1339 expected the IPv4 wildcard fallback, got {fallback_address}"
                    );
                    eprintln!(
                        "#1339 skipped: host has no IPv6 (`::` bind failed: {control_error}); \
                         bind_listener fell back to {fallback_address}"
                    );
                    return;
                }
            };
            let control_port = control.local_addr().expect("#1339 control address").port();
            // #1339 fix-r5: decide the host type by *connecting*, not by
            // binding. A bind-based control would share the very option that
            // can be wrong (`SO_REUSEADDR` makes the v4 bind succeed on a
            // dual-stack macOS host) and would make this assertion vacuous.
            let host_is_dual_stack = TcpStream::connect(("127.0.0.1", control_port))
                .await
                .is_ok();
            assert!(
                !host_is_dual_stack,
                "#1339 this host is dual-stack, yet bind_dual_stack(0) refused: {error}"
            );
            assert!(
                error.to_string().contains("IPv6-only"),
                "#1339 unexpected bind error: {error}"
            );
            return;
        }
    };
    let address = listener.local_addr().expect("#1339 bound address");
    let port = address.port();
    assert_ne!(port, 0, "#1339 the wildcard must own a real port");
    // #1339 fix-r5 (P2) / fix-r7 (P2, Codex #2): the ledger says the address is
    // the *v6 wildcard*, not merely some v6 address — `is_unspecified()` alone
    // would also accept the IPv4 wildcard, so assert the family too.
    assert!(
        address.is_ipv6() && address.ip().is_unspecified(),
        "#1339 `::` should bind the v6 wildcard, got {address}"
    );
    TcpStream::connect(("::1", port))
        .await
        .expect("#1339 the v6 side of a dual-stack listener must answer");
    // #1339 fix-r7 (P1, minimax sibling #2): with IPv6 up but IPv4 switched
    // off, `new_v4()` fails, the probe answers `Unknown`, the listener is kept
    // — and this branch is reached. A `.expect` here would turn a supported
    // host red, the same defect fix-r6 removed from the other branch. Prove
    // the host really has no IPv4 before skipping.
    match TcpStream::connect(("127.0.0.1", port)).await {
        Ok(_) => {}
        Err(v4_error) => {
            let v4_bindable = TcpListener::bind(("127.0.0.1", 0)).await.is_ok();
            assert!(
                !v4_bindable,
                "#1339 this host binds IPv4 yet the v4 side of `::` refused: {v4_error}"
            );
            eprintln!("#1339 skipped: host has no IPv4 (`127.0.0.1` connect failed: {v4_error})");
        }
    }
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

// ---------------------------------------------------------------------------
// #1339 fix-r1 dedicated tests (paths the oracle cannot pin)
// ---------------------------------------------------------------------------

/// A `SmsSender` that accepts everything; the dedicated tests never look at
/// outbound traffic.
struct NullSender;
impl SmsSender for NullSender {
    fn send(&self, _message: SmsOutgoingMessage) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}

fn null_factory() -> TwilioFactory {
    Arc::new(|_account_sid: &JsString, _auth_token: &JsString| {
        Ok(Arc::new(NullSender) as Arc<dyn SmsSender>)
    })
}

/// A gateway whose first `submitTurn` panics where the channel awaits it: the
/// #1738 shape — the fire-and-forget task dies mid-`processMessage`, which a
/// paired insert/remove cannot survive. Later submits return an empty event
/// stream so the redelivered turn completes.
struct PanickingGateway {
    calls: Arc<SyncMutex<Vec<String>>>,
}

impl SmsGateway for PanickingGateway {
    fn submit_turn(
        &self,
        request: SubmitTurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let calls = self.calls.clone();
        Box::pin(async move {
            let first = {
                let mut calls = calls.lock().expect("#1738 panicking calls mutex");
                calls.push(String::from_utf16_lossy(request.session_key.units()));
                calls.len() == 1
            };
            if first {
                panic!("#1738 injected process_message panic (sms)");
            }
            Ok(Box::new(MockTurnStream {
                events: VecDeque::new(),
                hold: None,
                await_error_settled: false,
                gates: Arc::new(SyncMutex::new(HashMap::new())),
            }) as Box<dyn TurnEventStream>)
        })
    }

    fn respond_elicitation(
        &self,
        _response: ElicitationResponse,
    ) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }

    fn permission_decide(
        &self,
        _decision: PermissionDecision,
    ) -> BoxFuture<Result<PermissionDecisionOutcome, ChannelError>> {
        Box::pin(async { Ok(PermissionDecisionOutcome { delivered: true }) })
    }
}

/// A spawned `handle.stop()` outcome as a message: `None` when it completed,
/// otherwise the stop error or the panic payload it aborted with.
fn stop_outcome(
    joined: Result<Result<(), ChannelError>, tokio::task::JoinError>,
) -> Option<String> {
    match joined {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("#1339 stop returned an error: {error:?}")),
        Err(join_error) => {
            let panic = join_error.into_panic();
            Some(
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                    .unwrap_or_else(|| "#1339 non-string panic payload".to_owned()),
            )
        }
    }
}

/// #1339 fix-r1 (P2 #11): the injected logger runs outside the boundaries
/// lock, so a logger that panics cannot poison the mutex for later stops.
#[tokio::test]
async fn logger_panic_does_not_poison_boundaries() {
    let logs: Arc<SyncMutex<Vec<(String, String)>>> = Arc::new(SyncMutex::new(Vec::new()));
    let logger: ChannelLog = {
        let logs = logs.clone();
        Arc::new(move |level, message| {
            if message.starts_with("sms: stopping (boom") {
                panic!("#1339 injected logger panic");
            }
            logs.lock()
                .expect("#1339 logger record mutex")
                .push((level.to_owned(), message.to_owned()));
        })
    };
    // `webhookPort: 0` — an ephemeral port; this test never speaks HTTP.
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(&json!({
            "accountSid": "AC1", "authToken": "tok", "phoneNumber": "+15550000",
            "webhookPort": 0,
        }))),
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: Some(null_factory()),
        record_unhandled: None,
    });
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: Some(logger),
            turn_gateway: None,
        })
        .await
        .expect("#1339 logger-panic test start");
    // First stop: the logger panics inside `SmsShared::log`. The panic still
    // aborts the stop task itself; with the fix the boundaries guard is
    // already released when it does.
    let first = tokio::spawn({
        let handle = handle.clone();
        async move { handle.stop("boom").await }
    });
    let joined = first.await;
    let payload = stop_outcome(joined);
    crate::negative_assert!(
        payload
            .as_deref()
            .is_some_and(|message| message.contains("#1339 injected logger panic")),
        "#1339 first stop must surface the injected logger panic: {payload:?}"
    );
    // Second stop: the boundaries mutex must not be poisoned — the stop
    // completes and its log line lands.
    let second = tokio::spawn({
        let handle = handle.clone();
        async move { handle.stop("cleanup").await }
    });
    let joined = second.await;
    let second_payload = stop_outcome(joined);
    crate::negative_assert!(
        second_payload.is_none(),
        "#1339 stop after a logger panic must not see a poisoned boundaries mutex: {second_payload:?}"
    );
    until("logger-panic cleanup log", || {
        logs.lock()
            .expect("#1339 logger record mutex")
            .iter()
            .any(|(_, message)| message == "sms: stopping (cleanup)")
    })
    .await;
}

/// #1738: a panic inside `processMessage` must not leak the active-chat slot.
/// The sms delivery is fire-and-forget (`tokio::spawn`, handle dropped), so
/// the dying task would otherwise leave the chat marked active and every
/// later message for it silently skipped until restart — the TS baseline
/// releases the slot in `finally`, which the Rust port's paired
/// insert/remove could not survive.
#[tokio::test]
async fn a_panicking_process_message_does_not_leak_the_active_chat() {
    let logs: Arc<SyncMutex<Vec<(String, String)>>> = Arc::new(SyncMutex::new(Vec::new()));
    let logger: ChannelLog = {
        let logs = logs.clone();
        Arc::new(move |level, message| {
            logs.lock()
                .expect("#1738 logger record mutex")
                .push((level.to_owned(), message.to_owned()));
        })
    };
    let calls = Arc::new(SyncMutex::new(Vec::<String>::new()));
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(&json!({
            "accountSid": "AC1", "authToken": "tok", "phoneNumber": "+15550000",
            "webhookPort": 0,
        }))),
        mapper: None,
        gateway: Some(Arc::new(PanickingGateway {
            calls: calls.clone(),
        })),
        env: None,
        twilio_factory: Some(null_factory()),
        record_unhandled: None,
    });
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: Some(logger),
            turn_gateway: None,
        })
        .await
        .expect("#1738 panic-leak test start");
    let shared = channel.shared.clone();
    let chat_id = JsString::from("+15550001");

    // The production shape: only the spawned task dies, the process does not.
    let joined = tokio::spawn({
        let shared = shared.clone();
        let chat_id = chat_id.clone();
        async move { handle_incoming(&shared, chat_id, JsString::from("one")).await }
    })
    .await;
    let error =
        joined.expect_err("#1738 the injected panic must fail the spawned task, not vanish");
    crate::negative_assert!(
        error.is_panic(),
        "#1738 the spawn must end in the panic, not a clean return"
    );
    let panic = error.into_panic();
    let payload = panic
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    crate::negative_assert!(
        payload.contains("#1738 injected process_message panic (sms)"),
        "#1738 unexpected panic payload: {payload:?}"
    );

    // C3: the unwind releases the slot — no residue until restart.
    let active = {
        let runtime = shared.runtime.lock().await;
        runtime
            .active_chats
            .lock()
            .expect("#1738 sms active chats mutex")
            .contains(&chat_id)
    };
    crate::negative_assert!(
        !active,
        "#1738 a panicking turn must not leave its chat marked active"
    );

    // C4: the next message for the same chat reaches the gateway again
    // instead of being dropped as already active.
    handle_incoming(&shared, chat_id.clone(), JsString::from("two"))
        .await
        .expect("#1738 the redelivery after the panic returns Ok");
    crate::negative_assert_eq!(
        calls.lock().expect("#1738 panicking calls mutex").len(),
        2,
        "#1738 the redelivery submits a second turn"
    );
    crate::negative_assert!(
        !logs
            .lock()
            .expect("#1738 logger record mutex")
            .iter()
            .any(|(_, message)| message.contains("already active, skipping")),
        "#1738 the redelivery must not be skipped as already active"
    );
    handle
        .stop("#1738 panic-leak test done")
        .await
        .expect("#1738 panic-leak test stop");
}

/// #1339 fix-r1 (P2 #6): a JSON `null` body throws on the TS side with the
/// socket left hanging (probes/null-body.txt — no response byte ever
/// arrives), so the oracle cannot pin it. The port reports the exact
/// TypeError through the recorder and destroys the connection; the
/// hang-vs-destroy deviation is ledgered in `A8.3-CH-SMS-007`.
#[tokio::test]
async fn json_null_body_is_unhandled_destroy() {
    const PORT: u16 = 18901;
    let record: Arc<SyncMutex<Record>> = Arc::new(SyncMutex::new(Record::default()));
    let unhandled_record = record.clone();
    let record_unhandled: UnhandledRecorder = Arc::new(move |message| {
        unhandled_record
            .lock()
            .expect("#1339 record mutex")
            .unhandled
            .push(message.to_owned());
    });
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(&json!({
            "accountSid": "AC1", "authToken": "tok", "phoneNumber": "+15550000",
            "webhookPort": PORT,
        }))),
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: Some(null_factory()),
        record_unhandled: Some(record_unhandled),
    });
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#1339 json-null test start");
    // 1) No signature header: the gate is skipped, `params.Body` on null
    //    throws first.
    let outcome = bounded(
        "json null without signature",
        raw_request(
            PORT,
            b"POST /sms/incoming HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
              Content-Type: application/json\r\nContent-Length: 4\r\n\r\nnull",
            false,
        ),
    )
    .await
    .expect("#1339 json null without signature response");
    crate::negative_assert_eq!(
        outcome,
        json!({"closed": true}),
        "#1339 json null without signature must destroy the connection"
    );
    // 2) With the signature header present the gate runs `Object.keys` on
    //    null, which throws with its own message before any comparison.
    let outcome = bounded(
        "json null with signature",
        raw_request(
            PORT,
            b"POST /sms/incoming HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
              Content-Type: application/json\r\nX-Twilio-Signature: bogus\r\n\
              Content-Length: 4\r\n\r\nnull",
            false,
        ),
    )
    .await
    .expect("#1339 json null with signature response");
    crate::negative_assert_eq!(
        outcome,
        json!({"closed": true}),
        "#1339 json null with signature must destroy the connection"
    );
    settle().await;
    let unhandled = { record.lock().expect("#1339 record mutex").unhandled.clone() };
    crate::negative_assert!(
        unhandled
            .contains(&"TypeError: Cannot read properties of null (reading 'Body')".to_owned()),
        "#1339 json null without signature must record the params.Body TypeError: {unhandled:?}"
    );
    crate::negative_assert!(
        unhandled.contains(&"TypeError: Cannot convert undefined or null to object".to_owned()),
        "#1339 json null with signature must record the Object.keys TypeError: {unhandled:?}"
    );
    bounded("json null cleanup stop", handle.stop("cleanup"))
        .await
        .expect("#1339 json null cleanup stop");
}

/// #1339 fix-r1 (P2 #13): an occupied replay port fails fast with the
/// preflight panic instead of corrupting the run. Drives the
/// `port-busy-preflight` oracle case (its own port, held busy here; the
/// case has no replay cell of its own so the parallel suite cannot race
/// this hold).
#[test]
fn port_busy_preflight_fails_fast() {
    let port: u16 = case_by_name("port-busy-preflight")["port"]
        .as_u64()
        .expect("#1339 preflight case port") as u16;
    let _busy =
        std::net::TcpListener::bind(("127.0.0.1", port)).expect("#1339 preflight busy bind");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("#1339 preflight runtime build")
            .block_on(replay_named("port-busy-preflight"))
    }));
    let payload = match result {
        Err(payload) => payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
            })
            .unwrap_or_else(|| "#1339 non-string panic payload".to_owned()),
        Ok(()) => "#1339 replay unexpectedly completed".to_owned(),
    };
    crate::negative_assert!(
        payload.contains(&format!("#1339 port {port} already in use")),
        "#1339 port-busy preflight marker: {payload}"
    );
}

/// #1339 fix-r10 (P0): the IPv4 half of the guard cannot see an occupier that
/// only holds IPv6, and that shape makes the replay pass in an environment
/// where the TS baseline cannot start (`v6only_probe.log`). Two shapes have to
/// be caught, and only a wildcard bind catches both (`v6_bind_probe.log`):
/// a v6-only *listener* on `[::]:port` (Q1) and a socket bound to `[::]:port`
/// that never called `listen` (Q2 — connects to `127.0.0.1` and `::1` both
/// fail, yet the port is taken). Deleting `v6_wildcard_state` from
/// `ensure_replay_port_free` turns this test red.
#[test]
fn port_busy_on_ipv6_preflight_fails_fast() {
    // Bind the IPv6 wildcard without listening: this is the shape the connect
    // probes cannot see, and it is exactly what the guard now probes for.
    //
    // #1339 fix-r11 (P2, Codex): skipping is only allowed for an error that
    // *proves* this host has no IPv6 (same `means_no_ipv6` the guard uses).
    // Any other failure — descriptor exhaustion, permissions, a sandbox — used
    // to return `skipped` and quietly delete the IPv6 coverage, which is the
    // same fail-open shape the guard itself was fixed for.
    let socket = match tokio::net::TcpSocket::new_v6() {
        Ok(socket) => socket,
        Err(error) if means_no_ipv6(&error) => {
            eprintln!("#1339 skipped: host has no IPv6 ({error})");
            return;
        }
        Err(error) => panic!("#1339 cannot create an IPv6 socket to set up the busy port: {error}"),
    };
    if let Err(error) = socket.bind(std::net::SocketAddr::from(([0u16; 8], 0))) {
        if means_no_ipv6(&error) {
            eprintln!("#1339 skipped: host has no IPv6 wildcard ({error})");
            return;
        }
        panic!("#1339 cannot bind the IPv6 wildcard to set up the busy port: {error}");
    }
    let port = socket
        .local_addr()
        .expect("#1339 v6 busy local addr")
        .port();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("#1339 preflight runtime build")
            .block_on(ensure_replay_port_free(port))
    }));
    let payload = match result {
        Err(payload) => payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
            })
            .unwrap_or_else(|| "#1339 non-string panic payload".to_owned()),
        Ok(()) => "#1339 replay unexpectedly completed".to_owned(),
    };
    crate::negative_assert!(
        payload.contains(&format!("#1339 port {port} already in use on IPv6")),
        "#1339 v6 port-busy preflight marker: {payload}"
    );
    drop(socket);
}

/// #1339 fix-r1 (P2 #15): the mapped Node `listen` error texts (probed on
/// the pinned Node v22.22.0; probes/listen-errors.txt).
#[test]
fn listen_error_matches_node_text() {
    let addr_in_use = listen_error(
        &std::io::Error::from(std::io::ErrorKind::AddrInUse),
        "127.0.0.1",
        18802.0,
    );
    crate::negative_assert_eq!(
        addr_in_use,
        "Error: listen EADDRINUSE: address already in use 127.0.0.1:18802",
        "#1339 EADDRINUSE Node text"
    );
    let not_available = listen_error(
        &std::io::Error::from(std::io::ErrorKind::AddrNotAvailable),
        "203.0.113.7",
        18852.0,
    );
    crate::negative_assert_eq!(
        not_available,
        "Error: listen EADDRNOTAVAIL: address not available 203.0.113.7:18852",
        "#1339 EADDRNOTAVAIL Node text"
    );
    let denied = listen_error(
        &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        "127.0.0.1",
        80.0,
    );
    crate::negative_assert_eq!(
        denied,
        "Error: listen EACCES: permission denied 127.0.0.1:80",
        "#1339 EACCES Node text"
    );
}

// ---------------------------------------------------------------------------
// #1589 dedicated tests (lock-fusion negative controls and the v4 probe)
// ---------------------------------------------------------------------------

/// #1589 (issue #1): a channel the test drives through `handle_incoming`
/// directly — no HTTP listener, only the boundary mocks that path touches.
fn lock_split_channel(case: Value, record: Arc<SyncMutex<Record>>, gates: Gates) -> SmsChannel {
    let gateway: GatewayRef = Arc::new(MockGateway {
        case,
        record,
        gates,
    });
    SmsChannel::new(SmsChannelOptions {
        extra: None,
        mapper: None,
        gateway: Some(gateway),
        env: None,
        twilio_factory: None,
        record_unhandled: None,
    })
}

/// The recorded RPC names, in order.
fn recorded_rpcs(record: &Arc<SyncMutex<Record>>) -> Vec<String> {
    record
        .lock()
        .expect("#1589 record mutex")
        .rpcs
        .iter()
        .map(|rpc| {
            rpc["rpc"]
                .as_str()
                .expect("#1589 recorded rpc carries a name")
                .to_owned()
        })
        .collect()
}

/// #1589 (issue #1): the elicitation `hasPending` + `take` fusion. Fused, a
/// concurrent event-stream `clear` cannot land between the check and the
/// take, so the pending elicitation is answered exactly once and the reply
/// never becomes a turn. Under `sms-elicit-lock-split` the two lock sections
/// let the clear land in between: the stale check passes, `take` finds
/// nothing, and the message falls through to `submitTurn`.
#[tokio::test]
async fn sms_elicit_lock_split() {
    let record: Arc<SyncMutex<Record>> = Arc::new(SyncMutex::new(Record::default()));
    let gates: Gates = Arc::new(SyncMutex::new(HashMap::new()));
    let channel = lock_split_channel(json!({}), record.clone(), gates);
    let chat = JsString::from("+15550001");
    let session = JsString::from("sms:chat=+15550001:general");
    {
        let mut runtime = channel.shared.runtime.lock().await;
        let question = runtime
            .elicitation
            .capture(&chat, &session, &JsValue::Undefined);
        crate::negative_assert!(
            !question.units().is_empty(),
            "#1589 elicit lock split preset must record a question"
        );
    }
    let incoming = {
        let shared = channel.shared.clone();
        let incoming_chat = chat.clone();
        tokio::spawn(
            async move { handle_incoming(&shared, incoming_chat, JsString::from("1")).await },
        )
    };
    // Exactly one yield: the spawned task runs to its first park point — the
    // fused section (which parks inside `respondElicitation`), or the
    // switch's injected yield between the two sections — and never further
    // before the clear below runs.
    tokio::task::yield_now().await;
    {
        let mut runtime = channel.shared.runtime.lock().await;
        runtime.elicitation.clear(&chat);
    }
    bounded("elicit lock split incoming", incoming)
        .await
        .expect("#1589 elicit lock split join")
        .expect("#1589 elicit lock split handleIncoming");
    let rpcs = recorded_rpcs(&record);
    crate::negative_assert_eq!(
        rpcs.iter()
            .filter(|rpc| *rpc == "respondElicitation")
            .count(),
        1,
        "#1589 elicit lock split must answer the pending elicitation exactly once"
    );
    crate::negative_assert!(
        !rpcs.iter().any(|rpc| rpc == "submitTurn"),
        "#1589 elicit lock split must not turn the answer into a submitTurn"
    );
}

/// #1589 (issue #1): the permission `hasPending` + `beginAnswer` fusion, same
/// shape as [`sms_elicit_lock_split`]. Under `sms-permission-lock-split` the
/// clear lands between the check and `beginAnswer`, the emptied state reads
/// as "no permission pending", and the reply becomes a `submitTurn`.
#[tokio::test]
async fn sms_permission_lock_split() {
    let record: Arc<SyncMutex<Record>> = Arc::new(SyncMutex::new(Record::default()));
    let gates: Gates = Arc::new(SyncMutex::new(HashMap::new()));
    let channel = lock_split_channel(json!({}), record.clone(), gates);
    let chat = JsString::from("+15550002");
    let session = JsString::from("sms:chat=+15550002:general");
    let event = to_js(&json!({
        "requestId": "req-1589",
        "toolName": "Bash",
        "payload": {"command": "ls"},
    }));
    {
        let mut runtime = channel.shared.runtime.lock().await;
        let prompt = runtime.permissions.capture(&chat, &session, &event);
        crate::negative_assert!(
            prompt.is_some(),
            "#1589 permission lock split preset must deliver a prompt"
        );
        runtime
            .permissions
            .confirm_initial_prompt(&chat, true, Some(&JsString::from("req-1589")));
    }
    let incoming = {
        let shared = channel.shared.clone();
        let incoming_chat = chat.clone();
        tokio::spawn(
            async move { handle_incoming(&shared, incoming_chat, JsString::from("1")).await },
        )
    };
    tokio::task::yield_now().await;
    {
        let mut runtime = channel.shared.runtime.lock().await;
        runtime.permissions.clear(&chat);
    }
    bounded("permission lock split incoming", incoming)
        .await
        .expect("#1589 permission lock split join")
        .expect("#1589 permission lock split handleIncoming");
    let rpcs = recorded_rpcs(&record);
    crate::negative_assert_eq!(
        rpcs.iter().filter(|rpc| *rpc == "permissionDecide").count(),
        1,
        "#1589 permission lock split must decide the pending permission exactly once"
    );
    crate::negative_assert!(
        !rpcs.iter().any(|rpc| rpc == "submitTurn"),
        "#1589 permission lock split must not turn the answer into a submitTurn"
    );
}

/// #1589 (issue #1): the `activeChats` `contains` → `resolve` → `insert`
/// fusion. The turn gate holds the first message's turn in flight (the TS
/// shape the fusion exists for), so fused the second webhook for the same
/// chat sees the chat active and is skipped — exactly one turn. Under
/// `sms-active-insert-split` both webhooks pass their `contains` check
/// before either inserts, so both dispatch.
#[tokio::test]
async fn sms_active_insert_split() {
    let record: Arc<SyncMutex<Record>> = Arc::new(SyncMutex::new(Record::default()));
    let gates: Gates = Arc::new(SyncMutex::new(HashMap::new()));
    let channel = lock_split_channel(
        json!({"turn": {"gate": "active-insert-turn"}}),
        record.clone(),
        gates.clone(),
    );
    let chat = JsString::from("+15550003");
    let first = {
        let shared = channel.shared.clone();
        let chat = chat.clone();
        tokio::spawn(async move { handle_incoming(&shared, chat, JsString::from("first")).await })
    };
    let second = {
        let shared = channel.shared.clone();
        let chat = chat.clone();
        tokio::spawn(async move { handle_incoming(&shared, chat, JsString::from("second")).await })
    };
    // Enough yields for both spawned tasks to reach their park point: fused,
    // the first turn parks inside `submitTurn` (holding the chat active) and
    // the second task skips; under the switch both park between their two
    // lock sections, before either inserts.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    release_if_exists(&gates, "active-insert-turn");
    bounded("active insert split first", first)
        .await
        .expect("#1589 active insert split first join")
        .expect("#1589 active insert split first handleIncoming");
    bounded("active insert split second", second)
        .await
        .expect("#1589 active insert split second join")
        .expect("#1589 active insert split second handleIncoming");
    let rpcs = recorded_rpcs(&record);
    crate::negative_assert_eq!(
        rpcs.iter().filter(|rpc| *rpc == "submitTurn").count(),
        1,
        "#1589 active insert split must dispatch exactly one turn for one chat"
    );
}

/// #1589 (issue #6): pin every arm of the pure probe classifier, including
/// the one a healthy host never reaches. `sms-v4-probe-unknown-as-unshared`
/// restores the fix-r5 misreading (any non-EADDRINUSE failure read as a free
/// port) and is killed by the `Unknown` test below.
#[test]
fn classify_v4_probe_ok_is_unshared() {
    crate::negative_assert_eq!(
        classify_v4_probe(&Ok(())),
        V4Port::Unshared,
        "#1589 v4 probe must read a free port as Unshared"
    );
}

#[test]
fn classify_v4_probe_addr_in_use_is_shared() {
    crate::negative_assert_eq!(
        classify_v4_probe(&Err(std::io::Error::from(std::io::ErrorKind::AddrInUse))),
        V4Port::Shared,
        "#1589 v4 probe must read a collision as Shared"
    );
}

/// A probe that could not run (permission, descriptor exhaustion, a missing
/// address family) says nothing about the host; reading it as `Unshared`
/// would demote a working dual-stack listener to the IPv4-only fallback.
#[test]
fn classify_v4_probe_other_errors_stay_unknown() {
    for error in [
        std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        std::io::Error::from_raw_os_error(libc::EMFILE),
        std::io::Error::from_raw_os_error(libc::EAFNOSUPPORT),
    ] {
        crate::negative_assert_eq!(
            classify_v4_probe(&Err(error)),
            V4Port::Unknown,
            "#1589 v4 probe must keep unattributable errors Unknown"
        );
    }
}
