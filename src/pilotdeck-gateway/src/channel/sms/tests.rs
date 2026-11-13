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
const EXPECTED_CASE_COUNT: usize = 59;
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

/// #1813: the recorder's explicit `null` is the absence sentinel for the
/// optional `turn` / `permissionError` / `permissionGate` / `sendBlockAt` /
/// `sendError` fields — `generate.mts` writes `?? null` for each of them, and
/// an omitted field never appears as `null` in the oracle. Every one of those
/// readers goes through here, and so does `permissionDelivered`, which the
/// recorder writes as `?? true` and therefore never records as `null`: routing
/// it through the same helper keeps a hand-edited oracle from making one field
/// treat `null` differently from its siblings.
///
/// #1879: the same helper now carries the `?? {}` / `?? []` / `?? 0` /
/// `?? "no reason"` / `=== true` / falsy fields listed below in
/// `OLD_NULL_REJECTING_FIELDS`, so every recorded read that has a TS default
/// spells absence the same way — one `recorded_present` call site per name in
/// `OLD_NULL_REJECTING_FIELDS`.
fn recorded_present<'a>(value: &'a Value, field: &str) -> Option<&'a Value> {
    let present = value.get(field);
    // #1813 negative control: the switch restores the old reads, in which
    // `permissionDelivered` had no null filter and the #1879 fields (`env`,
    // `sendFailAt`, the `turn.*` members, the per-step `truncate` /
    // `halfClose`, and the per-step `headers` / `reason`) still rejected an
    // explicit null as a type drift.
    if crate::mutants::on("sms-oracle-null-sentinel") {
        return if field == "permissionDelivered" || OLD_NULL_REJECTING_FIELDS.contains(&field) {
            present
        } else {
            present.filter(|value| !value.is_null())
        };
    }
    present.filter(|value| !value.is_null())
}

/// #1879: fields whose pre-#1879 read did not filter an explicit JSON `null`.
/// The recorder writes `?? {}` / `?? []` / `?? 0` / `?? "no reason"` /
/// `=== true` / a falsy check for them, so absence spelled as `null` must not
/// become a type error on one field only. Names match the JSON key the selected
/// object carries at the read site — each name has exactly one `recorded_present`
/// call site today, so the switch still restores exactly one behaviour per
/// field. (`gate` is also read directly by the `release` replay arm, which does
/// not go through `recorded_present`; keep it that way, or the shared switch
/// would flip two behaviours at once.)
const OLD_NULL_REJECTING_FIELDS: [&str; 11] = [
    "env",
    "sendFailAt",
    "gate",
    "error",
    "events",
    "holdAfter",
    "truncate",
    "halfClose",
    "headers",
    "reason",
    "extra",
];

/// #1813: every sentinel-bearing field must read explicit `null` the same way.
/// The generated oracle writes `null` for the unconfigured optional fields and
/// `true` for an unconfigured `permissionDelivered`; a hand-edited oracle that
/// spells absence as `null` must not turn into a type error on one field only.
///
/// Scope: the top-level sentinel fields. The scalar sentinels are `?? null` /
/// `?? true` in the recorder; the two collections are `?? {}` / `?? []`
/// (`generate.mts:1097` / `:1104`), so a hand-edited explicit `null` is
/// absence there too (#1879: the shared `sms-oracle-null-sentinel` switch
/// restores the old fail-closed read for both, via `OLD_NULL_REJECTING_FIELDS`).
/// The `turn.*` members and the per-step `truncate` / `halfClose` / `headers` /
/// `reason` have their own tests (`null_turn_*_is_absence`,
/// `recorded_{truncate,half_close,header_map,stop_reason}_null_is_absence`),
/// because a single aggregate would stop at its first failing assertion.
#[test]
fn null_sentinels_are_absence_for_every_sentinel_field() {
    let nulls = json!({
        "turn": null,
        "permissionError": null,
        "permissionGate": null,
        "permissionDelivered": null,
        "sendBlockAt": null,
        "sendError": null,
        "env": null,
        "sendFailAt": null,
    });
    crate::negative_assert!(
        matches!(recorded_turn_checked(&nulls), Ok(None)),
        "#1339 #1813 null turn is absence"
    );
    crate::negative_assert!(
        matches!(recorded_permission_error_checked(&nulls), Ok(None)),
        "#1339 #1813 null permissionError is absence"
    );
    crate::negative_assert!(
        matches!(recorded_permission_gate_checked(&nulls), Ok(None)),
        "#1339 #1813 null permissionGate is absence"
    );
    crate::negative_assert!(
        matches!(recorded_permission_delivered_checked(&nulls), Ok(None)),
        "#1339 #1813 null permissionDelivered is absence"
    );
    crate::negative_assert!(
        matches!(recorded_send_block_checked(&nulls), Ok(None)),
        "#1339 #1813 null sendBlockAt is absence"
    );
    crate::negative_assert!(
        matches!(recorded_send_error_checked(&nulls), Ok(None)),
        "#1339 #1813 null sendError is absence"
    );
    crate::negative_assert!(
        matches!(recorded_env_map_checked(&nulls), Ok(None)),
        "#1339 #1879 null env is absence"
    );
    crate::negative_assert!(
        matches!(recorded_send_fail_checked(&nulls), Ok(None)),
        "#1339 #1879 null sendFailAt is absence"
    );
}

/// #1879: the TS replay never reads a `turn.*` member through `?? null`; it
/// falsy-checks `gate` / `error` and falls back with `?? []` / string equality
/// for `events` / `holdAfter`, so an explicit `null` is absence on all four.
/// One test per member: `sms-oracle-null-sentinel` binds all four as an
/// all-cells row, and a shared test would stop at its first failing assertion
/// and leave the other three fields unproven.
#[test]
fn null_turn_gate_is_absence() {
    crate::negative_assert!(
        recorded_turn_gate_checked(&json!({"gate": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 null turn.gate is absence"
    );
}

#[test]
fn null_turn_error_is_absence() {
    crate::negative_assert!(
        recorded_turn_error_checked(&json!({"error": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 null turn.error is absence"
    );
}

#[test]
fn null_turn_events_is_absence() {
    crate::negative_assert!(
        recorded_turn_events_checked(&json!({"events": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 null turn.events is absence"
    );
}

#[test]
fn null_turn_hold_is_absence() {
    crate::negative_assert!(
        recorded_turn_hold_checked(&json!({"holdAfter": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 null turn.holdAfter is absence"
    );
}

fn recorded_header_map_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        // #1879: `effectiveHeaders` spreads `step.headers ?? {}` (generate.mts:376),
        // the same `?? {}` absence signature as `env`, so a hand-edited explicit
        // `null` reads as "no overrides" on both sides.
        recorded_present(value, "headers"),
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
fn recorded_header_map_null_is_absence() {
    crate::negative_assert!(
        recorded_header_map_checked(&json!({"headers": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 null headers is absence"
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

/// #1913: every pre-existing step was an HTTP/1.1 client, so the request
/// version was implicit in `build_raw`; the `http10` cell records its own
/// `HTTP/1.0` request line, and this reader keeps that field honest.
fn recorded_version_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("version"),
        "version",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("sms-oracle-recorded-version"),
    )
}

fn recorded_version(value: &Value) -> Option<&str> {
    recorded_version_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_version_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_version_checked(&json!({"version": []})).is_err(),
        "#1913 rejects version type drift"
    );
}

#[test]
#[should_panic(expected = "#1339 version must be a string")]
fn recorded_version_panics_on_wrong_type() {
    recorded_version(&json!({"version": []}));
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
    // #1879: `REQUEST(..., step.truncate ?? 0)` (generate.mts:941) — an
    // explicit null is the same as an absent `truncate`, i.e. "no truncation".
    recorded_checked(
        recorded_present(value, "truncate"),
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
fn recorded_truncate_null_is_absence() {
    crate::negative_assert!(
        recorded_truncate_checked(&json!({"truncate": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 null truncate is absence"
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
        recorded_present(value, "turn"),
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
        // #1879: TS reads `script.turn?.gate` with a falsy check
        // (`generate.mts:108`), so an explicit JSON null is absence here too.
        // Only the null sentinel is aligned: `""` still parses as a gate name
        // and `false` / `0` stay a type drift, so a hand-edited `""` diverges
        // from the TS falsy skip by design.
        recorded_present(value, "gate"),
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
        // #1879: `if (script.turn?.error)` is falsy-checked on the TS side, so
        // an explicit null is absence. Only the null sentinel is aligned: `""`
        // still parses as an error message and `false` / `0` stay a type
        // drift, so a hand-edited `""` diverges from the TS falsy skip by
        // design.
        recorded_present(value, "error"),
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
        // #1879: `script.turn?.events ?? []` (`generate.mts:110`).
        recorded_present(value, "events"),
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
        // #1879: `script.turn?.holdAfter === 'elicit'` compares against a
        // string, so an explicit null is absence rather than a type drift.
        // Any other non-`elicit` value that is not a string (`false`, `0`)
        // keeps the type error: this unit aligns the null sentinel only.
        recorded_present(value, "holdAfter"),
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
        recorded_present(value, "permissionError"),
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
        recorded_present(value, "permissionGate"),
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
        recorded_present(value, "permissionDelivered"),
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
        // #1879: `generate.mts:1097` writes `env: definition.env ?? {}`, so an
        // explicit JSON null is absence exactly like an omitted key. Going
        // through `recorded_present` keeps that read identical to the scalar
        // sentinels; the shared switch restores the old fail-closed read.
        recorded_present(value, "env"),
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

#[test]
fn recorded_env_map_null_is_absence() {
    crate::negative_assert!(
        recorded_env_map_checked(&json!({"env": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 env null is absence, not type drift"
    );
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
    // #1879: `step.halfClose === true` (generate.mts:945) — an explicit null
    // is falsy there, so it is absence here rather than a type drift.
    recorded_checked(
        recorded_present(value, "halfClose"),
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
fn recorded_half_close_null_is_absence() {
    crate::negative_assert!(
        recorded_half_close_checked(&json!({"halfClose": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 null halfClose is absence"
    );
}

#[test]
#[should_panic(expected = "#1339 halfClose must be a boolean")]
fn recorded_half_close_panics_on_wrong_type() {
    recorded_half_close(&json!({"halfClose": []}));
}

/// #2152: `step.omitHost === true` (generate.mts builds the request line
/// without a Host when the flag is set) — an explicit null is falsy there, so
/// it is absence here rather than a type drift, exactly like `halfClose`.
fn recorded_omit_host_checked(value: &Value) -> Result<Option<bool>, String> {
    recorded_checked(
        recorded_present(value, "omitHost"),
        "omitHost",
        "a boolean",
        Value::as_bool,
        false,
        crate::mutants::on("sms-oracle-recorded-omit-host"),
    )
}

fn recorded_omit_host(value: &Value) -> Option<bool> {
    recorded_omit_host_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_omit_host_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_omit_host_checked(&json!({"omitHost": []})).is_err(),
        "#2152 rejects omitHost type drift"
    );
}

#[test]
fn recorded_omit_host_null_is_absence() {
    crate::negative_assert!(
        recorded_omit_host_checked(&json!({"omitHost": null})).is_ok_and(|value| value.is_none()),
        "#2152 null omitHost is absence"
    );
}

#[test]
#[should_panic(expected = "#1339 omitHost must be a boolean")]
fn recorded_omit_host_panics_on_wrong_type() {
    recorded_omit_host(&json!({"omitHost": []}));
}

fn recorded_log_level_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("level"),
        "wait-log.level",
        "a string",
        Value::as_str,
        // #1879: the generator's `waitLog(level, message)` helper
        // (`generate.mts:299`) always writes both keys and the TS replay
        // compares `level === step.level` with no `??` fallback, so a missing
        // level is recording drift. Reading it as `""` used to turn the wait
        // into a bare empty-string comparison instead of a field-level error.
        true,
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
fn recorded_log_level_rejects_absence() {
    crate::negative_assert!(
        recorded_log_level_checked(&json!({})).is_err(),
        "#1339 #1879 rejects missing wait-log.level"
    );
}

#[test]
#[should_panic(expected = "#1339 wait-log.level must be a string")]
fn recorded_log_level_panics_on_wrong_type() {
    recorded_log_level(&json!({"level": []}));
}

#[test]
#[should_panic(expected = "#1339 wait-log.level must be a string, got <missing>")]
fn recorded_log_level_panics_when_absent() {
    recorded_log_level(&json!({}));
}

fn recorded_log_text_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("text"),
        "wait-log.text",
        "a string",
        Value::as_str,
        // #1879: same as `recorded_log_level_checked` — `waitLog` always writes
        // `text`, and the TS predicate has no fallback for a missing one.
        true,
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
fn recorded_log_text_rejects_absence() {
    crate::negative_assert!(
        recorded_log_text_checked(&json!({})).is_err(),
        "#1339 #1879 rejects missing wait-log.text"
    );
}

#[test]
#[should_panic(expected = "#1339 wait-log.text must be a string")]
fn recorded_log_text_panics_on_wrong_type() {
    recorded_log_text(&json!({"text": []}));
}

#[test]
#[should_panic(expected = "#1339 wait-log.text must be a string, got <missing>")]
fn recorded_log_text_panics_when_absent() {
    recorded_log_text(&json!({}));
}

fn recorded_sends_count_checked(value: &Value) -> Result<Option<u64>, String> {
    // #1813: `count` is required. The TS recorder's `waitUntil` compares
    // `length >= step.count`, and `n >= undefined` is never true, so an absent
    // count can never be satisfied there (3 s timeout); reading it as 0 here
    // made the same input a no-op wait (`len() >= 0`).
    recorded_checked(
        value.get("count"),
        "wait-sends.count",
        "a u64",
        Value::as_u64,
        true,
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

#[test]
fn recorded_sends_count_rejects_absence() {
    crate::negative_assert!(
        recorded_sends_count_checked(&json!({})).is_err(),
        "#1339 #1813 wait-sends.count is a required recording field"
    );
}

#[test]
#[should_panic(expected = "#1339 wait-sends.count must be a u64, got <missing>")]
fn recorded_sends_count_panics_when_absent() {
    recorded_sends_count(&json!({}));
}

fn recorded_rpcs_count_checked(value: &Value) -> Result<Option<u64>, String> {
    // #1813: `count` is required. The TS recorder's `waitUntil` compares
    // `length >= step.count`, and `n >= undefined` is never true, so an absent
    // count can never be satisfied there (3 s timeout); reading it as 0 here
    // made the same input a no-op wait (`len() >= 0`).
    recorded_checked(
        value.get("count"),
        "wait-rpcs.count",
        "a u64",
        Value::as_u64,
        true,
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

#[test]
fn recorded_rpcs_count_rejects_absence() {
    crate::negative_assert!(
        recorded_rpcs_count_checked(&json!({})).is_err(),
        "#1339 #1813 wait-rpcs.count is a required recording field"
    );
}

#[test]
#[should_panic(expected = "#1339 wait-rpcs.count must be a u64, got <missing>")]
fn recorded_rpcs_count_panics_when_absent() {
    recorded_rpcs_count(&json!({}));
}

fn recorded_stop_reason_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        // #1879: the handler does `reason ?? "no reason"` (channel/sms/mod.rs), and
        // the recorder's `stop(null)` writes `{reason: null}` (generate.mts:305), so
        // an explicit `null` is absence here too.
        recorded_present(value, "reason"),
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

/// #1879: the stop replay arm's reason resolution, shared with the null test so
/// a change to the default cannot slip past the assertion.
fn replay_stop_reason(step: &Value) -> &str {
    // TS `handle.stop(reason)` — an absent reason logs "no reason".
    recorded_stop_reason(step).unwrap_or("no reason")
}

#[test]
fn recorded_stop_reason_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_stop_reason_checked(&json!({"reason": []})).is_err(),
        "#1339 #1729 rejects stop.reason type drift"
    );
}

#[test]
fn recorded_stop_reason_null_is_absence() {
    crate::negative_assert!(
        recorded_stop_reason_checked(&json!({"reason": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 null stop.reason is absence"
    );
}

#[test]
#[should_panic(expected = "#1339 stop.reason must be a string")]
fn recorded_stop_reason_panics_on_wrong_type() {
    recorded_stop_reason(&json!({"reason": []}));
}

fn recorded_send_block_checked(value: &Value) -> Result<Option<u64>, String> {
    recorded_checked(
        recorded_present(value, "sendBlockAt"),
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
        // #1879: `generate.mts:1104` writes `sendFailAt: definition.sendFailAt
        // ?? []`, so an explicit JSON null is absence, same as an omitted key
        // (same `recorded_present` read as `env`).
        recorded_present(value, "sendFailAt"),
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

#[test]
fn recorded_send_fail_null_is_absence() {
    crate::negative_assert!(
        recorded_send_fail_checked(&json!({"sendFailAt": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 sendFailAt null is absence, not type drift"
    );
}

fn recorded_send_error_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        recorded_present(value, "sendError"),
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
        // #1879: `definition.extra ?? {}` (generate.mts:364 / :378) has the same
        // signature as `env: definition.env ?? {}`, so an explicit `null` is
        // absence here too.
        recorded_present(value, "extra"),
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

#[test]
#[should_panic(expected = "#1339 #1879 signed case valid-signature must record request headers")]
fn signed_recording_rejects_null_headers() {
    validate_mock_recording(&json!({
        "name": "valid-signature",
        "sign": true,
        "steps": [{"op": "request", "headers": null}],
    }));
}

#[test]
fn recorded_extra_null_is_absence() {
    crate::negative_assert!(
        recorded_extra_checked(&json!({"extra": null})).is_ok_and(|value| value.is_none()),
        "#1339 #1879 null extra is absence"
    );
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

/// One recorded body as `(text, count, bytes)`. `bytes` is `text.len() * count`,
/// validated without materializing the body, so a reader that only needs the
/// length does not allocate a megabyte to measure it (#1984).
fn recorded_body_spec_checked(step: &Value) -> Result<(&str, usize, usize), String> {
    let (text, count) = match step.get("body") {
        None => return Ok(("", 0, 0)),
        Some(Value::String(text)) => (text.as_str(), 1),
        Some(spec @ Value::Object(_)) => (
            recorded_body_repeat_checked(spec)?.unwrap_or_default(),
            recorded_body_count_checked(spec)?.unwrap_or_default(),
        ),
        Some(value) => {
            if crate::mutants::on("sms-oracle-body-type") {
                return Ok(("", 0, 0));
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
    Ok((text, count, length))
}

fn recorded_body_checked(step: &Value) -> Result<Vec<u8>, String> {
    let (text, count, _) = recorded_body_spec_checked(step)?;
    Ok(text.repeat(count).into_bytes())
}

/// Recorded body byte length of one step, without materializing the bytes.
fn recorded_body_len_checked(step: &Value) -> Result<usize, String> {
    recorded_body_spec_checked(step).map(|(_, _, length)| length)
}

/// #2041: wire body bytes of one recorded `request` step — the bytes that
/// actually reach the product after `truncate`, clamped at zero like the
/// generator's `Math.max(0, bytes.length - truncate)` (generate.mts:250),
/// with the length still derived through `recorded_body_spec_checked` so a
/// 1 MiB boundary body is never materialized just to be measured. This is
/// the single derivation of the wire body length: the #1984 boundary gate,
/// `build_raw` and the #1901 stall-shape check all call it, so the three
/// cannot fork back into parallel copies that agree only by coincidence.
fn wire_body_len_checked(step: &Value) -> Result<usize, String> {
    let truncate =
        usize::try_from(recorded_truncate_checked(step)?.unwrap_or_default()).unwrap_or(usize::MAX);
    Ok(recorded_body_len_checked(step)?.saturating_sub(truncate))
}

fn wire_body_len(step: &Value) -> usize {
    wire_body_len_checked(step).unwrap_or_else(|error| panic!("{error}"))
}

fn recorded_body(step: &Value) -> Vec<u8> {
    recorded_body_checked(step).unwrap_or_else(|error| panic!("{error}"))
}

/// #1984: request bodies that reach `MAX_BODY_BYTES` are *boundary* cells and
/// must be declared here with their expected length. The check classifies by
/// the wire byte length, not by the case name, so a renamed cell or a newly
/// added boundary cell that is missing from this table fails the replay instead
/// of passing silently -- the old `_ => Ok(())` name whitelist was exactly that
/// silent pass.
const BOUNDARY_BODY_CASES: &[(&str, usize)] = &[
    ("payload-exactly-1mib", MAX_BODY_BYTES),
    ("payload-too-large", MAX_BODY_BYTES + 1),
];

/// Bytes of one recorded `request` body that actually reach the product, in
/// fixture order. `build_raw` drops `truncate` body bytes while keeping the
/// recorded `Content-Length`, so a boundary cell that truncates no longer
/// probes the cap even though its recorded body is still a megabyte (#1984).
fn recorded_request_body_lengths(case: &Value) -> Result<Vec<usize>, String> {
    recorded_steps_checked(case)?
        .into_iter()
        .flatten()
        .filter(|step| step.get("op").and_then(Value::as_str) == Some("request"))
        // #2041: the one shared wire-length derivation, the same one
        // `build_raw` and the #1901 stall shape use.
        .map(wire_body_len_checked)
        .collect()
}

fn assert_boundary_body_len_checked(name: &str, case: &Value) -> Result<(), String> {
    let lengths = recorded_request_body_lengths(case)?;
    match BOUNDARY_BODY_CASES.iter().find(|entry| entry.0 == name) {
        Some((_, expected)) => {
            if lengths != [*expected] && !crate::mutants::on("sms-oracle-boundary-body-length") {
                return Err(format!(
                    "#1339 {name} body boundary must be [{expected}], got {lengths:?}"
                ));
            }
        }
        None => {
            // #1984 fail-closed: a body that reaches the cap is a boundary cell
            // even when its name was never declared. Register it with its
            // expected length, or the replay fails here instead of skipping.
            if let Some(length) = lengths.iter().copied().find(|len| *len >= MAX_BODY_BYTES)
                && !crate::mutants::on("sms-oracle-boundary-unregistered")
            {
                return Err(format!(
                    "#1339 {name} records a {length}-byte request body that reaches the \
                     {MAX_BODY_BYTES}-byte body cap but is not declared in BOUNDARY_BODY_CASES; \
                     declare it with its expected length"
                ));
            }
        }
    }
    Ok(())
}

fn assert_boundary_body_len(name: &str, case: &Value) {
    assert_boundary_body_len_checked(name, case).unwrap_or_else(|error| panic!("{error}"));
}

/// Recorded request head and wire body of one step (#2041). The head carries
/// the recorded `Content-Length` — the full body length, exactly what the
/// generator declares (`Buffer.byteLength(bytes)`, generate.mts:259) — while
/// the body is cut to `wire_body_len`, the single wire-length derivation the
/// boundary gate also uses. `build_raw` concatenates the two; the split lets
/// the gate-equivalence test observe the wire body without re-deriving the
/// head format.
fn build_raw_parts(step: &Value) -> (Vec<u8>, Vec<u8>) {
    let body = recorded_body(step);
    let headers = recorded_headers(step);
    // #1913: `Connection: close` was hardcoded with the 1.1 request line;
    // the `http10` cell records its own `Connection: keep-alive`, so the
    // header follows the recorded map and the response is what must decide
    // the close — a replay that silently closed would keep passing with the
    // finalize gone, which is exactly what the cell exists to catch.
    let connection = if headers
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case("connection"))
    {
        ""
    } else {
        "Connection: close\r\n"
    };
    // #2152: the missing-Host cell records `omitHost: true`, so the replay
    // writes no `Host:` line — sending one would turn the parser-rejection
    // cell into an admitted request, the same fail-closed rule `setHost`
    // carries on the api-server lane.
    let host = if recorded_omit_host(step).unwrap_or(false) {
        ""
    } else {
        "Host: 127.0.0.1\r\n"
    };
    let mut head = format!(
        "{} {} {}\r\n{host}{connection}",
        recorded_method(step).unwrap_or("POST"),
        recorded_path(step).unwrap_or("/sms/incoming"),
        recorded_version(step).unwrap_or("HTTP/1.1"),
    )
    .into_bytes();
    for (key, value) in headers {
        head.extend_from_slice(format!("{key}: {value}\r\n").as_bytes());
    }
    head.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    // #2041: the wire body length comes from the one shared derivation, so
    // the bytes on the wire and the boundary gate's classification cannot
    // fork; `truncate` keeps the tail off the wire while the recorded
    // `Content-Length` above stays the full length.
    let wire_len = if crate::mutants::on("sms-oracle-wire-body-len-fork") {
        // Negative control: `build_raw` re-derives its own wire length (the
        // pre-#2041 shape of a second, forking copy) and stops honoring
        // `truncate` — `gate_and_build_raw_share_the_wire_body_length`
        // must go red here.
        body.len()
    } else {
        wire_body_len(step)
    };
    let mut wire_body = body;
    wire_body.truncate(wire_len);
    (head, wire_body)
}

fn build_raw(step: &Value) -> Vec<u8> {
    let (mut raw, body) = build_raw_parts(step);
    raw.extend_from_slice(&body);
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
    signed_request_headers_checked(case).unwrap_or_else(|error| panic!("{error}"));
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

/// #1879 / #1957: a `sign: true` recording bakes the TS-computed
/// `X-Twilio-Signature` into each request step's headers (`generate.mts:377`)
/// and the replay reuses those bytes instead of re-signing. `channel/sms/mod.rs`
/// only runs the signature gate when that header is non-empty
/// (`!signature.is_empty()`), so a hand-edited `headers` that drops it —
/// `null`, `{}`, a map without the key, or an empty value — would replay
/// unsigned and still match the recorded 200: the signature coverage would be
/// silently gone (parity false pass, #1957 P1).
///
/// The requirement covers every request step of a signed case regardless of
/// its body shape. `effectiveHeaders` only bakes a signature when the body is a
/// string, but a rewrite like `"abc"` -> `{repeat: "abc", count: 1}` keeps the
/// replay bytes byte-identical, so exempting non-string bodies would reopen the
/// same hole. A future generated case in that shape therefore fails this guard
/// loudly instead of replaying unsigned; the generator would have to bake a
/// signature (or refuse the shape) before such a case can be recorded.
///
/// The flag itself is part of the guard: `sign` must be present on any case
/// that replays a request, must be a boolean, and a signed case must replay at
/// least one request step — otherwise a hand edit could hide the whole check.
fn signed_request_headers_checked(case: &Value) -> Result<(), String> {
    let name = case
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("<unnamed>");
    // #996 switch `sms-oracle-signed-signature-header`: restores the pre-#1957
    // guard, which only required an object `headers` on a `sign: true` case.
    let legacy = crate::mutants::on("sms-oracle-signed-signature-header");
    let requests: Vec<&Value> = case
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|step| step.get("op").and_then(Value::as_str) == Some("request"))
        .collect();
    let signed = match case.get("sign") {
        Some(Value::Bool(flag)) => *flag,
        // `generate.mts:1099` writes `sign: definition.sign ?? false` on every
        // case, so a case that replays a request but carries no `sign` is a
        // hand edit. Reading it as unsigned would drop the whole guard.
        None if requests.is_empty() => false,
        None => {
            if legacy {
                return Ok(());
            }
            return Err(format!("#1339 #1957 case {name} must record a sign flag"));
        }
        // `generate.mts:1099` writes `sign: definition.sign ?? false`, so the
        // oracle only ever holds a boolean. Any other value is a hand edit
        // whose signing intent cannot be judged, so it must not skip the guard.
        Some(other) => {
            if legacy {
                return Ok(());
            }
            return Err(format!(
                "#1339 #1957 case {name} sign must be a boolean, got {other}"
            ));
        }
    };
    if !signed {
        return Ok(());
    }
    // A signed case that replays no request proves nothing about the signature
    // (same zero-coverage boundary as `recorded_steps_checked`'s empty array).
    if requests.is_empty() {
        if legacy {
            return Ok(());
        }
        return Err(format!(
            "#1339 #1957 signed case {name} must replay at least one request step"
        ));
    }
    for step in requests {
        // #1879: a null `headers` is absence for an unsigned step, but for a
        // signed one it drops the baked signature, so keep it fail-closed.
        let Some(headers) = step.get("headers").and_then(Value::as_object) else {
            return Err(format!(
                "#1339 #1879 signed case {name} must record request headers"
            ));
        };
        if legacy {
            continue;
        }
        // Header names are case-insensitive (hyper lowercases them on the way
        // in), and an empty value is as good as absence at the gate. Whitespace
        // only counts as empty too: the HTTP parser strips optional whitespace,
        // so `" "` reaches the gate as `""` and would skip validation.
        let signature = headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("x-twilio-signature"))
            .and_then(|(_, value)| value.as_str())
            .unwrap_or_default();
        if signature.trim().is_empty() {
            return Err(format!(
                "#1339 #1957 signed case {name} must record a non-empty X-Twilio-Signature header"
            ));
        }
    }
    Ok(())
}

#[test]
fn signed_recording_rejects_empty_headers() {
    crate::negative_assert!(
        signed_request_headers_checked(&json!({
            "name": "valid-signature",
            "sign": true,
            "steps": [{"op": "request", "headers": {}}],
        }))
        .is_err(),
        "#1339 #1957 signed case must record a non-empty X-Twilio-Signature header"
    );
}

#[test]
fn signed_recording_rejects_missing_signature_key() {
    crate::negative_assert!(
        signed_request_headers_checked(&json!({
            "name": "valid-signature",
            "sign": true,
            "steps": [{"op": "request", "headers": {
                "Content-Type": "application/x-www-form-urlencoded",
            }}],
        }))
        .is_err(),
        "#1339 #1957 signed case must record a non-empty X-Twilio-Signature header"
    );
}

#[test]
fn signed_recording_rejects_blank_signature_header() {
    crate::negative_assert!(
        signed_request_headers_checked(&json!({
            "name": "valid-signature",
            "sign": true,
            "steps": [{"op": "request", "headers": {"X-Twilio-Signature": ""}}],
        }))
        .is_err(),
        "#1339 #1957 signed case must record a non-empty X-Twilio-Signature header"
    );
}

#[test]
fn signed_recording_rejects_non_boolean_sign() {
    crate::negative_assert!(
        signed_request_headers_checked(&json!({
            "name": "valid-signature",
            "sign": "true",
            "steps": [{"op": "request", "headers": {}}],
        }))
        .is_err(),
        "#1339 #1957 case sign must be a boolean"
    );
}

#[test]
fn signed_recording_requires_sign_flag() {
    // `generate.mts:1099` writes `sign` on every case, so a case that replays a
    // request without it is a hand edit: reading it as unsigned would skip the
    // guard entirely (same edit family as a non-boolean `sign`).
    crate::negative_assert!(
        signed_request_headers_checked(&json!({
            "name": "valid-signature",
            "steps": [{"op": "request", "headers": {"X-Twilio-Signature": "abc"}}],
        }))
        .is_err(),
        "#1339 #1957 case must record a sign flag"
    );
}

#[test]
fn signed_recording_rejects_whitespace_signature_header() {
    // The HTTP parser strips optional whitespace, so `" \t "` reaches the gate
    // as `""` and skips signature validation.
    crate::negative_assert!(
        signed_request_headers_checked(&json!({
            "name": "valid-signature",
            "sign": true,
            "steps": [{"op": "request", "headers": {"X-Twilio-Signature": " \t "}}],
        }))
        .is_err(),
        "#1339 #1957 signed case must record a non-empty X-Twilio-Signature header"
    );
}

#[test]
fn signed_recording_requires_a_request_step() {
    // A signed case that replays no request proves nothing about the signature.
    crate::negative_assert!(
        signed_request_headers_checked(&json!({
            "name": "valid-signature",
            "sign": true,
            "steps": [{"op": "stop", "reason": "done"}],
        }))
        .is_err(),
        "#1339 #1957 signed case must replay at least one request step"
    );
}

#[test]
fn signed_recording_rejects_non_string_signature_header() {
    // A non-string header value cannot be signed or compared, so it is as good
    // as absent: `as_str()` yields `None` and the empty-value branch fires.
    crate::negative_assert!(
        signed_request_headers_checked(&json!({
            "name": "valid-signature",
            "sign": true,
            "steps": [{"op": "request", "headers": {"X-Twilio-Signature": 7}}],
        }))
        .is_err(),
        "#1339 #1957 signed case must record a non-empty X-Twilio-Signature header"
    );
}

#[test]
fn signed_recording_checks_every_request_step() {
    // The guard claims to cover *every* request step of a signed case, so a
    // second request step that lost the signature header must not slip past a
    // guard that only looked at the first one.
    crate::negative_assert!(
        signed_request_headers_checked(&json!({
            "name": "valid-signature",
            "sign": true,
            "steps": [
                {"op": "request", "headers": {"X-Twilio-Signature": "abc"}},
                {"op": "request", "headers": {
                    "Content-Type": "application/x-www-form-urlencoded",
                }},
            ],
        }))
        .is_err(),
        "#1339 #1957 signed case must record a non-empty X-Twilio-Signature header"
    );
}

#[test]
fn signed_recording_accepts_lowercase_signature_header() {
    assert!(
        signed_request_headers_checked(&json!({
            "name": "valid-signature",
            "sign": true,
            "steps": [{"op": "request", "headers": {"x-twilio-signature": "abc"}}],
        }))
        .is_ok(),
        "#1339 #1957 the signature header name is case-insensitive"
    );
}

#[test]
fn unsigned_recording_allows_null_headers() {
    // #1957 pairing control: the `sign: true` requirement must not leak onto
    // unsigned cases, where a null `headers` is ordinary absence. A case that
    // replays nothing may also omit `sign` — the flag is only required once a
    // request step exists (`absent_recording_fields_keep_defaults` calls this
    // helper with minimal JSON).
    for case in [
        json!({"name": "unsigned", "sign": false, "steps": [{"op": "request", "headers": null}]}),
        json!({"name": "unsigned", "sign": false, "steps": [{"op": "request"}]}),
        json!({"name": "unsigned", "steps": [{"op": "stop", "reason": "done"}]}),
        json!({"name": "unsigned"}),
    ] {
        assert!(
            signed_request_headers_checked(&case).is_ok(),
            "#1339 #1957 unsigned case {case} must keep a null headers as absence"
        );
    }
    validate_mock_recording(&json!({
        "name": "unsigned",
        "sign": false,
        "steps": [{"op": "request", "headers": null}],
    }));
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

/// #1984: the boundary check used to key off the case *name*, so a renamed or
/// newly added boundary cell returned `Ok(())` without looking at the body at
/// all. The real 1 MiB+1 cell under an undeclared name must now be rejected,
/// which is what closes that fail-open; the negative control
/// `sms-oracle-boundary-unregistered` disarms exactly this branch.
#[test]
fn recorded_boundary_body_length_rejects_unregistered_case() {
    crate::negative_assert!(
        assert_boundary_body_len_checked(
            "payload-too-large-renamed",
            &case_by_name("payload-too-large"),
        )
        .is_err(),
        "#1339 #1984 rejects an undeclared boundary case"
    );
}

/// #1984: a truncated boundary body no longer puts its recorded length on the
/// wire, so it must not be accepted as proof that the cap itself was probed.
#[test]
fn recorded_boundary_body_length_counts_only_wire_bytes() {
    crate::negative_assert!(
        assert_boundary_body_len_checked(
            "payload-exactly-1mib",
            &json!({"steps":[{
                "op":"request",
                "body":{"repeat":"x","count":1_048_576_u64},
                "truncate":1_u64,
            }]}),
        )
        .is_err(),
        "#1339 #1984 truncation shortens the replayed body below the cap"
    );
}

/// #2041: the #1984 review found the "wire body length" derived three times
/// in parallel (the boundary gate, `build_raw`, and the #1901 stall shape);
/// the copies agreed only by coincidence, so a later change to one of them —
/// say, a new truncation semantics inside `build_raw` — would not move the
/// others and the boundary cells would be silently misjudged again. This test
/// pins the equivalence directly: for every shape below and every request
/// step of every oracle cell, the gate's number is the count of body bytes
/// `build_raw` actually puts after the head, so the two call sites cannot
/// fork without a red test (`sms-oracle-wire-body-len-fork`).
#[test]
fn gate_and_build_raw_share_the_wire_body_length() {
    let table: &[(&str, Value)] = &[
        (
            "plain body",
            json!({"steps":[{"op":"request","body":"abcdefgh"}]}),
        ),
        (
            "absent truncate",
            json!({"steps":[{"op":"request","body":"abcdef"}]}),
        ),
        (
            "zero truncate",
            json!({"steps":[{"op":"request","body":"abcdef","truncate":0_u64}]}),
        ),
        (
            "null truncate is absence",
            json!({"steps":[{"op":"request","body":"abcdef","truncate":null}]}),
        ),
        (
            "partial truncate",
            json!({"steps":[{"op":"request","body":"abcdef","truncate":2_u64}]}),
        ),
        (
            "truncate equal to the body",
            json!({"steps":[{"op":"request","body":"abc","truncate":3_u64}]}),
        ),
        (
            "truncate past the body clamps at zero",
            json!({"steps":[{"op":"request","body":"abc","truncate":100_u64}]}),
        ),
        (
            "repeat body",
            json!({"steps":[{"op":"request","body":{"repeat":"xy","count":3},"truncate":4_u64}]}),
        ),
        (
            "several request steps, non-request steps not counted",
            json!({"steps":[
                {"op":"request","body":"abc","truncate":1_u64},
                {"op":"settle","body":"must-not-count"},
                {"op":"request","body":{"repeat":"z","count":10},"truncate":7_u64},
            ]}),
        ),
        (
            "boundary shapes",
            json!({"steps":[
                {"op":"request","body":{"repeat":"x","count":1_048_576_u64}},
                {"op":"settle"},
                {"op":"request","body":{"repeat":"y","count":1_048_577_u64},"truncate":1_u64},
            ]}),
        ),
    ];
    let mut cases: Vec<(String, Value)> = table
        .iter()
        .map(|(label, case)| ((*label).to_owned(), case.clone()))
        .collect();
    for case in oracle_cases() {
        let name = case["name"]
            .as_str()
            .expect("#2041 every oracle case carries a string name")
            .to_owned();
        cases.push((name, case));
    }
    for (label, case) in cases {
        let gate = recorded_request_body_lengths(&case)
            .unwrap_or_else(|error| panic!("#2041 {label} gate derivation: {error}"));
        let wire = recorded_steps(&case)
            .expect("#2041 steps")
            .iter()
            .filter(|step| step.get("op").and_then(Value::as_str) == Some("request"))
            .map(|step| build_raw_parts(step).1.len())
            .collect::<Vec<usize>>();
        crate::negative_assert_eq!(
            gate,
            wire,
            "#2041 the gate and build_raw must agree on the wire body length ({label})"
        );
    }
}

/// #1984: the declared boundary table and the fixture must agree in both
/// directions -- a declared cell that was renamed or resized fails, and every
/// oracle cell whose replayed `request` body reaches the cap must be declared.
/// The per-case check above catches a *replayed* undeclared cell; this test
/// also catches a rename that removed the declared cell from the fixture.
#[test]
fn boundary_body_cases_match_the_fixture() {
    // The declared entries must still bracket the cap itself; otherwise the two
    // cells stop proving that `MAX_BODY_BYTES` is the accept/reject boundary.
    let mut declared_lengths: Vec<usize> = BOUNDARY_BODY_CASES
        .iter()
        .map(|(_, length)| *length)
        .collect();
    declared_lengths.sort_unstable();
    assert_eq!(
        declared_lengths,
        [MAX_BODY_BYTES, MAX_BODY_BYTES + 1],
        "#1339 #1984 the declared boundary cells must bracket MAX_BODY_BYTES"
    );
    for (name, expected) in BOUNDARY_BODY_CASES {
        assert_eq!(
            recorded_request_body_lengths(&case_by_name(name))
                .expect("#1339 declared boundary case steps"),
            [*expected],
            "#1339 #1984 declared boundary case {name}"
        );
    }
    for case in oracle_cases() {
        let name = case["name"]
            .as_str()
            .expect("#1339 every oracle case carries a string name");
        if BOUNDARY_BODY_CASES
            .iter()
            .any(|(declared, _)| *declared == name)
        {
            continue;
        }
        // Call the gate itself instead of re-deriving its threshold here: the
        // "reaches the cap" rule lives in one place, so the two cannot drift.
        assert_boundary_body_len_checked(name, &case).unwrap_or_else(|error| {
            panic!("#1339 #1984 undeclared oracle case {name} fails the boundary gate: {error}")
        });
    }
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
    // #1879: an explicit `null` reaches the real environment path as absence,
    // not just the `recorded_env_map_checked` helper.
    assert!(
        recorded_env(&json!({"env": null})).is_empty(),
        "#1339 #1879 env null is an empty environment"
    );
    assert!(recorded_body(&empty).is_empty(), "#1339 absent body");
    assert_eq!(build_raw(&empty), b"POST /sms/incoming HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n", "#1339 absent request fields");
    validate_mock_recording(&empty);
    // Five of these are explicit absence sentinels the recorder emits
    // (`turn` / `permissionError` / `permissionGate` / `sendBlockAt` /
    // `sendError` are `?? null`). `env` / `sendFailAt` are emitted as `{}` /
    // `[]`; a hand-edited explicit `null` is absence for them too (#1879).
    validate_mock_recording(
        &json!({"turn":null,"permissionError":null,"permissionGate":null,"sendBlockAt":null,"sendError":null,"env":null,"sendFailAt":null}),
    );
    // #1879: the `turn.*` members and the per-step `truncate` / `halfClose`
    // reach the real replay path as absence too, not just the `_checked`
    // helpers — `validate_mock_recording` reads all four members through the
    // panicking wrappers, and `build_raw` keeps every body byte when
    // `truncate` is null (TS `?? 0`, `generate.mts:941`).
    validate_mock_recording(
        &json!({"turn":{"gate":null,"error":null,"events":null,"holdAfter":null}}),
    );
    let null_step = json!({"body": "abc", "truncate": null, "halfClose": null});
    assert!(
        recorded_truncate(&null_step).is_none() && recorded_half_close(&null_step).is_none(),
        "#1339 #1879 null truncate / halfClose are absence"
    );
    assert_eq!(
        build_raw(&null_step),
        build_raw(&json!({"body": "abc"})),
        "#1339 #1879 null truncate sends the same bytes as an absent key"
    );
    // Replay site `let half_close = recorded_half_close(step).unwrap_or(false)`
    // (tests.rs replay arm) must derive `false` from an explicit `null`, the
    // same as TS `step.halfClose === true` (`generate.mts:945`).
    assert!(
        !recorded_half_close(&null_step).unwrap_or(false),
        "#1339 #1879 null halfClose derives false, same as an absent key"
    );
    // #1879: null `headers` / `stop.reason` reach the real replay paths too,
    // not just the `_checked` helpers. `build_raw` builds the request bytes via
    // `recorded_headers`, and the recorded step's `headers` is the
    // `effectiveHeaders` output (`generate.mts:376` `{ ...(step.headers ?? {}) }`
    // at `:391`), which `REQUEST` consumes as-is (`generate.mts:941`). The stop
    // arm replays `recorded_stop_reason(step).unwrap_or("no reason")`, the same
    // default TS `handle.stop` applies to `reason ?? "no reason"`.
    assert_eq!(
        build_raw(&json!({"body": "abc", "headers": null})),
        build_raw(&json!({"body": "abc"})),
        "#1339 #1879 null headers sends the same bytes as an absent key"
    );
    assert_eq!(
        replay_stop_reason(&json!({"reason": null})),
        "no reason",
        "#1339 #1879 null stop.reason replays as the default reason"
    );
    // The replay builds the channel options with
    // `extra: Some(to_js_object(recorded_extra(&case).unwrap_or(&Value::Null)))`,
    // so assert that same expression yields the empty object TS `?? {}` gives.
    assert_eq!(
        to_js_object(recorded_extra(&json!({"extra": null})).unwrap_or(&Value::Null)),
        to_js_object(&json!({})),
        "#1339 #1879 null extra replays as an empty extra, same as TS ?? {{}}"
    );
    assert!(
        to_js_object(recorded_extra(&json!({"extra": null})).unwrap_or(&Value::Null)).is_empty(),
        "#1339 #1879 null extra is an empty extra"
    );
    // #1729 fix-r6: `expected` is no longer an optional recording field, so the
    // old `assert_responses_so_far(&[], &empty, …)` probe (absent `expected`
    // silently compares nothing) is gone; absence now panics — covered by
    // `recorded_expected_panics_when_absent`.
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
        recorded_header_map(&json!({"headers": null})).is_none(),
        "#1339 #1879 null headers is absence"
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
    // #1879: `wait-log.level` / `wait-log.text` are now required recording
    // fields, so absence is a field-level error instead of an empty-string
    // comparison — covered by `recorded_log_level_panics_when_absent` and
    // `recorded_log_text_panics_when_absent`.
    assert!(
        recorded_log_level(&json!({"level":"value"})).is_some(),
        "#1339 valid wait-log.level"
    );
    assert!(
        recorded_log_text(&json!({"text":"value"})).is_some(),
        "#1339 valid wait-log.text"
    );
    // #1813: `wait-sends.count` / `wait-rpcs.count` are required. The old
    // `unwrap_or_default()` turned an absent count into 0, and `len() >= 0` is
    // always true — but the TS recorder's `n >= undefined` is never true, so
    // the same recording times out there. Absence must fail here too.
    assert!(
        recorded_sends_count_checked(&empty).is_err(),
        "#1339 #1813 absent wait-sends.count must fail, not default to 0"
    );
    assert!(
        recorded_sends_count(&json!({"count":1})).is_some(),
        "#1339 valid wait-sends.count"
    );
    assert!(
        recorded_rpcs_count_checked(&empty).is_err(),
        "#1339 #1813 absent wait-rpcs.count must fail, not default to 0"
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
        recorded_stop_reason(&json!({"reason": null})).is_none(),
        "#1339 #1879 null stop.reason is absence"
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
    // #1984: iterate the declaration table instead of a second literal list, so
    // a third boundary cell is replayed here without editing this loop.
    for (name, _) in BOUNDARY_BODY_CASES.iter().copied() {
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
///
/// #2691 gap 1: the guard is parameterized by the host the product will
/// actually bind — the same `String(webhookHost ?? DEFAULT_HOST)` coalesce
/// `start` runs ([`effective_webhook_host`]). Only the empty host binds the
/// dual-stack `::` (`bind_listener`, mod.rs:815-845), so only there can the
/// v6 half below ask its question; a fixed host (every recorded case is
/// `127.0.0.1`, the unreachable `203.0.113.7`, or the empty string —
/// `empty-webhook-host`, the one recorded shape that takes the `::`
/// wildcard) never touches IPv6, and a v6-only occupier used to fail the
/// probe although the product's own `bind(("127.0.0.1", port))` succeeds —
/// measured on both sides (`_tmp/20261026-issue-2691/probes/`: a v6-only
/// holder of `[::]:P` makes the `::` bind `EADDRINUSE` while the 127.0.0.1
/// bind and Node's `listen(127.0.0.1, P)` both succeed). The v4 half stays
/// on 127.0.0.1 for every host: it watches the address the replay's own
/// probe traffic dials (`raw_request` / `raw_until_close` / the `connect`
/// steps), not the product's bind address.
///
/// #2691 fix-r2 (review r2 concurrency#2, P1): the halves divide the work by
/// the host the product binds — the v6 half (empty host only) asks who holds
/// the dual-stack `::` wildcard, and the bind probe ahead of the connect half
/// (fixed host only) asks whether the product's own `bind((host, port))`
/// would fail. The connect probe cannot answer that second question: an
/// occupier that never listens refuses instead of answering, and a
/// **dual-stack** `[::]:port` holder covers the IPv4 port too, so exactly
/// the bind the product issues fails and `start` swallows the loss into the
/// no-op handle (mod.rs:606-628) — the shape the probe below catches.
async fn ensure_replay_port_free(port: u16, host: &str) {
    if crate::mutants::on("sms-port-preflight") {
        return;
    }
    // #1339 fix-r6 (P2): ask "is something listening?" by *connecting*, not by
    // binding. A bind probe carries `SO_REUSEADDR` — the option shape both
    // std and the product's own tokio listener set on Unix (mio sets it,
    // exactly like std; see the fixed-host bind probe below) — and with that
    // option a macOS/BSD kernel reports a port still held by a *wildcard*
    // holder as free, the very contamination this guard exists to stop. A
    // live listener on the probed address always answers a connect, and a
    // lingering `TIME_WAIT` socket from the previous replay refuses instead
    // of accepting, so the connect is clear of both shapes. (`TIME_WAIT` is
    // not what separates the two probes here: the product's own bind sets
    // `SO_REUSEADDR` too, so it is no more exposed to a `TIME_WAIT`
    // collision than this connect is.)
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
    //
    // #2691: only for the empty host — the one shape whose product bind is
    // the `::` wildcard. For a fixed host the question is moot (the product
    // never issues this bind), and answering it anyway is the pre-fix false
    // red the `sms-port-preflight-v6-any-host` switch keeps for the negative
    // control.
    if host.is_empty() || crate::mutants::on("sms-port-preflight-v6-any-host") {
        match v6_wildcard_state(port) {
            V6Port::Free | V6Port::Absent => {}
            V6Port::Occupied => panic!(
                "#1339 port {port} already in use on IPv6; the `::` bind would fail and fall back to IPv4-only"
            ),
            V6Port::Unknown => {
                panic!(
                    "#1339 cannot prove the IPv6 wildcard port {port} is free; refusing to replay"
                )
            }
        }
    }

    // #2691 fix-r2 (review r2 concurrency#2, P1): the fixed host's own bind.
    // A std `TcpListener::bind((host, port))` has the same option shape as
    // the product's `bind_listener` on Unix (mio sets `SO_REUSEADDR` on the
    // tokio listener, exactly like std), so this probe's verdict is the
    // product's verdict; `SO_REUSEADDR` is also what keeps it clear of the
    // previous replay's `TIME_WAIT` (#1339 fix-r6). Measured on this host
    // (`_tmp/20261026-issue-2691/fix-r2/probes/bind-probe-errno.log`):
    //   a v6-only holder of `[::]:P`     -> Ok (the product's v4 bind
    //      succeeds there too, so the v6_only_occupier case stays green)
    //   a dual-stack holder of `[::]:P`  -> EADDRINUSE (the connect probe
    //      below is refused — the holder never listens — so this bind is the
    //      only half that sees it)
    //   a host this machine does not own -> EADDRNOTAVAIL (`203.0.113.7`,
    //      whose recording itself expects the product's bind to fail —
    //      `start-host-unavailable` — so there is nothing local to probe
    //      and the replay must proceed)
    // Anything the probe cannot classify aborts the replay, the same
    // fail-closed posture the other halves take.
    //
    // #2691 fix-r3 (review r3 MM boundary#2, P1): the probe runs *before*
    // the v4 connect probe below. Measured on the coordinator's macOS host
    // (`_tmp/20261110-pr2760-harvest/macos-dualstack-probe.log`): a v4
    // connect against a dual-stack `[::]:P` holder that never listens
    // stalls for its whole timeout — the SYN is swallowed — while the std
    // bind reports `EADDRINUSE` (errno 48) immediately. With the bind probe
    // after the connect half, the connect half died at its 5 s `bounded`
    // cap first, so on macOS this probe was unreachable and the exact
    // shape it exists for went unreported there. On Linux the same connect
    // is refused at once, so the order only decides which half reports a
    // holder, never whether one does.
    if !host.is_empty() && !crate::mutants::on("sms-port-preflight-bind-probe-skipped") {
        match std::net::TcpListener::bind((host, port)) {
            Ok(listener) => drop(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => panic!(
                "#1339 port {port} is already bound on {host}: the product's own \
                 bind would fail and start would degrade to the no-op handle — \
                 an occupier the connect probe cannot see holds it"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::AddrNotAvailable => {}
            Err(error) => {
                panic!(
                    "#1339 cannot prove port {port} is bindable on {host} ({error}); refusing to replay"
                )
            }
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

/// #2691: the host the product's `bind_listener` will bind for a case — the
/// same `String(webhookHost ?? DEFAULT_HOST)` coalesce `start` runs
/// (mod.rs:554-557), reusing the product's own `nullish` / `js_string_of`
/// and the product's own [`DEFAULT_HOST`] constant instead of a hand-built
/// twin (a non-string host coerces exactly like the channel coerces it).
/// The preflight must probe this address, not a lookalike: an empty host is
/// the only recorded shape that binds the dual-stack `::`, and the other
/// hosts this fixture records are fixed IPv4 addresses that cannot collide
/// with a v6-only occupier. A hostname or IPv6 literal host would still
/// resolve through the product's `bind((host, port))` — the bind probe in
/// [`ensure_replay_port_free`] asks that question directly, so this helper
/// does not need to predict the answer.
fn effective_webhook_host(extra: Option<&Value>) -> String {
    let field = extra
        .and_then(|extra| extra.get("webhookHost"))
        .map_or(JsValue::Undefined, to_js);
    let host = if nullish(&field) {
        JsValue::from(DEFAULT_HOST)
    } else {
        field
    };
    String::from_utf16_lossy(js_string_of(&host).units())
}

/// #2691 fix-r3 (review r3 concurrency#1 / boundary#1 / MM boundary#1, three
/// roles independently): the `extra_owners` a guard call site keeps alive
/// across `start()`, beyond the local `shared` clone every call site holds.
/// Only [`replay_named`] needs it — the `cron` steps below its guard call
/// `channel.deliver_cron_result`, so the channel (an `SmsShared` owner) is
/// still alive when the guard runs. Every other call site drops the channel
/// first (the drop form, `0` here), which is what the absolute `>= 2` of
/// fix-r2 was written for: after `drop(channel)` a no-op start leaves only
/// the local clone (count 1) while a started one keeps the handle and the
/// accept task too (count 3). In the non-drop form those counts are 2
/// (channel + clone) and 4 (channel + clone + handle + accept task), so a
/// `>= 2` written for the drop form is tautological there — the threshold
/// must be raised by exactly this constant. Pinned from the failing side by
/// [`a_replay_start_that_keeps_the_channel_alive_is_still_caught_after_start`].
const OWNERS_KEPT_ALIVE_ACROSS_START: usize = 1;

/// #2691 gap 2 (TOCTOU): the preflight above is a pre-`start` probe, but
/// `start` swallows a lost bind into the no-op handle (mod.rs:606-628: the
/// `sms: HTTP server failed` log, then the no-op return), so an occupier
/// that appears between the probe and the bind leaves the pin silently
/// measuring the foreign listener. The
/// `Arc::strong_count` guard after `start()` closes that window — the same
/// shape `head_timeout_tests.rs` runs after every one of its starts (#2020,
/// pinned from the failing side by its bind-race cell): a no-op handle owns
/// no `SmsShared`, while a started channel keeps one in the handle and one
/// in its accept task. The threshold is `>= 2 + extra_owners`: `2` is the
/// drop-form floor (local clone + one started owner; see
/// [`OWNERS_KEPT_ALIVE_ACROSS_START`] for the two call-site shapes), and
/// `extra_owners` adds the owners a call site kept alive across `start()`.
/// What slips past the probes (an occupier that appears only after the
/// preflight reported the port free — with fix-r3's bind probe first, a
/// pre-existing dual-stack `[::]:port` holder is already caught before
/// `start`) lands here.
fn assert_bound_listener(shared: &Arc<SmsShared>, port: u16, extra_owners: usize) {
    if crate::mutants::on("sms-pin-bind-race-unchecked") {
        return;
    }
    crate::negative_assert!(
        Arc::strong_count(shared) >= 2 + extra_owners,
        "#2691 start() on port {port} returned the no-op handle: the bind lost the \
         race with an occupier the preflight could not see, so this pin would \
         measure a foreign listener"
    );
}

/// #2691 fix-r2 (review r2 concurrency#3 / boundary#2 / sibling#2): does this
/// recording itself show a live listener? The product logs
/// `sms: Twilio webhook http://…` only after a successful bind and listen,
/// so the cells that expect `start` to degrade to the no-op handle — the
/// `sms: missing config`, `sms: twilio init failed` and
/// `sms: HTTP server failed` shapes (`start-missing-config`,
/// `start-twilio-init-failed`, `start-port-busy`, `start-port-out-of-range`,
/// `start-port-nan`, `start-host-unavailable`; `port-busy-preflight` too,
/// though its replay never reaches `start` — the preflight fires first) —
/// never record that line. [`assert_bound_listener`] runs only for the cells
/// that do: the discriminator anchors the positive evidence (a bound
/// listener in the recording), not an enumeration of failure modes, so a
/// future cell that fails to start in a new way cannot turn the guard
/// into a false red.
fn expects_live_listener(case: &Value) -> bool {
    recorded_expected(case)
        .and_then(|expected| expected.get("logs"))
        .and_then(Value::as_array)
        .is_some_and(|logs| {
            logs.iter().any(|log| {
                log.as_array()
                    .and_then(|pair| pair.get(1))
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.starts_with("sms: Twilio webhook http://"))
            })
        })
}

/// #2691 fix-r3 (review r3 sibling#2 / simplify#1 / MM boundary#4): pin the
/// [`expects_live_listener`] discriminator itself. It is the only thing that
/// can silently switch the whole post-start guard off — the prefix
/// `"sms: Twilio webhook http://"` is hardcoded against the product's own
/// `format!` in `mod.rs`, so a log-text drift would turn every cell's verdict
/// false and no other test would notice. The count over all 59 oracle cells
/// is the tripwire: 52 of them record a live listener (the seven no-op
/// shapes are the complete complement), and any drift in the prefix, the
/// `[level, text]` log shape or the cell set turns this red instead of
/// disarming the guard quietly.
#[test]
fn expects_live_listener_anchors_the_product_webhook_log_line() {
    let mut live = 0;
    for case in oracle_cases() {
        if expects_live_listener(&case) {
            live += 1;
        }
    }
    crate::negative_assert_eq!(
        live,
        52,
        "#2691 the live-listener discriminator no longer sees 52 of the oracle \
         cells — the product log prefix or the recorded log shape drifted, and \
         every drifted cell silently skips its post-start bind guard"
    );
    for name in ["start-ok", "start-ok-public-url", "http10"] {
        crate::negative_assert!(
            expects_live_listener(&case_by_name(name)),
            "#2691 the recorded {name} cell shows a live listener — the \
             discriminator must agree"
        );
    }
    for name in [
        "start-missing-config",
        "start-twilio-init-failed",
        "start-port-busy",
        "start-port-out-of-range",
        "start-port-nan",
        "start-host-unavailable",
        "port-busy-preflight",
    ] {
        crate::negative_assert!(
            !expects_live_listener(&case_by_name(name)),
            "#2691 the recorded {name} cell expects the no-op handle — the \
             discriminator must not run the post-start guard for it"
        );
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

/// The comparison basis for the `body-read-timeout` answer: Node v22.22.0's
/// raw 51-byte `408 Request Timeout` ([`crate::user_dialog::NODE_408_RESPONSE`],
/// measured on the pinned Node in
/// `_tmp/20261026-issue-1334/fix-r1/node-probe/probe4.out`) with hyper's
/// chunked framing for an unknown-length empty body. This is not the literal
/// wire bytes — the sms lane's bare builder writes lowercase header names and
/// an implicit `date`; see [`normalize_wire`]. The framing residual is
/// registered for this lane in
/// `specs/gateway-parity/diffs/A8.3-CH-SMS-010.md`; the sms lane returns the
/// same shared builder as the webhook lane
/// (`webhook::node_request_timeout_response`), so the two cannot drift apart.
const REQUEST_TIMEOUT_408: &[u8] =
    b"HTTP/1.1 408 Request Timeout\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";

/// The comparison form of an HTTP/1.1 response's wire bytes: header names are
/// case-folded and the `date` line is dropped — the two presentation
/// dimensions registered for this lane in
/// `specs/gateway-parity/diffs/A8.3-CH-SMS-002.md` (unapproved) and pinned for
/// this cell in `A8.3-CH-SMS-010.md`. The sms lane serves through a bare
/// `http1::Builder`, so unlike the webhook lane's `title_case_headers(true)` +
/// `auto_date_header(false)` it writes lowercase header names and hyper's
/// implicit `date`; Node's canned `requestTimeoutResponse` carries no `Date`
/// at all. Everything else — status line, the `Connection: close` value, the
/// chunk framing (`0\r\n\r\n` verbatim, not case-folded) — is compared
/// exactly, so a missing close or a changed status still fails.
///
/// Panics on anything that is not a complete HTTP/1.1 head plus body: a
/// truncated or non-response wire must not normalize into a passing string.
fn normalize_wire(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .expect("#1901 a complete HTTP/1.1 head");
    let mut out = String::with_capacity(text.len());
    for (index, line) in head.split("\r\n").enumerate() {
        if index == 0 {
            out.push_str(line);
            out.push_str("\r\n");
            continue;
        }
        let (name, value) = line.split_once(':').expect("#1901 header line");
        let name = name.trim().to_ascii_lowercase();
        if name == "date" {
            continue;
        }
        out.push_str(&name);
        out.push(':');
        out.push_str(value);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    // Chunk framing stays verbatim: the zero-size terminator is `0` in both
    // stacks and the 408 body is empty, so nothing here needs folding.
    out.push_str(body);
    out
}

/// #1901: the cell declares `Content-Length: 64`, sends 8 bytes and keeps the
/// write side open, so only the server's `requestTimeout` can end it. The
/// replay pins the recorded TS shape first, then drives the Rust port with a
/// shortened injected deadline (200 ms) and compares the wire bytes (see
/// [`normalize_wire`] for the two registered presentation dimensions).
///
/// Without the deadline the connection task never returns: the client read
/// times out at `CLIENT_DEADLINE` and the assertion below is the kill point
/// for the `sms-body-read-no-timeout` negative control.
async fn replay_body_read_timeout(case: &Value, port: u16) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const INJECTED_DEADLINE: Duration = Duration::from_millis(200);
    const CLIENT_DEADLINE: Duration = Duration::from_secs(5);

    // The recorded TS answer, pinned so this cell cannot go blind.
    let expected = &case["expected"]["responses"][0];
    crate::negative_assert_eq!(
        expected["status"].as_u64(),
        Some(408),
        "#1901 body-read-timeout: the baseline answers a stalled body with 408"
    );
    crate::negative_assert_eq!(
        expected["body"].as_str(),
        Some(""),
        "#1901 body-read-timeout: the requestTimeout body is empty"
    );
    crate::negative_assert!(
        expected.get("closed").is_none() && expected.get("refused").is_none(),
        "#1901 body-read-timeout: the baseline must deliver the 408, not reset \
         or refuse the connection, got {expected}"
    );
    // Node's `requestTimeoutResponse` writes no `Content-Type`.
    crate::negative_assert!(
        expected.get("contentType").is_none_or(Value::is_null),
        "#1901 body-read-timeout: the requestTimeout answer has no content-type, \
         got {expected}"
    );
    // The recorded request really stalls: the generator's `REQUEST()` writes
    // `Content-Length: <byteLength(body)>` from the whole body while
    // `truncate` keeps the tail off the wire, so the server waits for the
    // rest of a body it was promised. The generator only takes this shape
    // when the definition sets `stall: true` (its own read deadline instead
    // of the 3 s default), so the flag is pinned here rather than left as an
    // unread recorded field.
    let steps = recorded_steps(case).expect("#1901 steps");
    let step = &steps[0];
    crate::negative_assert_eq!(
        step.get("stall").and_then(Value::as_bool),
        Some(true),
        "#1901 body-read-timeout: the cell must be recorded with the stalled \
         request shape"
    );
    let declared = recorded_body(step).len();
    let truncate = recorded_truncate(step).unwrap_or_default();
    // The generator clamps with `Math.max(0, len - truncate)`, so a truncation
    // longer than the body can only come from hand-edited fixtures; keep that
    // corruption loud instead of letting it collapse to `sent = 0` below.
    // #2041: compared in the recorded u64 domain — a `truncate` wider than
    // `usize` is the same corruption and fails here just as loudly.
    crate::negative_assert!(
        truncate <= declared as u64,
        "#1901 body-read-timeout: truncate {truncate} exceeds the declared body \
         ({declared} bytes)"
    );
    // #2041: the sent length is the one shared wire derivation
    // (`wire_body_len`), the same one the boundary gate and `build_raw` use,
    // so the stall shape cannot fork from the replay's own truncation.
    let sent = wire_body_len(step);
    crate::negative_assert!(
        sent < declared,
        "#1901 body-read-timeout: the request must stall before its declared \
         body is complete (sent {sent} of {declared})"
    );

    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(recorded_extra(case).unwrap_or(&Value::Null))),
        mapper: None,
        gateway: None,
        env: Some(Arc::new(|_| None)),
        twilio_factory: Some(null_factory()),
        record_unhandled: None,
        request_timeout: Some(INJECTED_DEADLINE),
    });
    // #2691 fix-r2: same post-start bind proof as the replay driver — the
    // recording expects a live 408 answer, so a start that lost the bind must
    // not fall through to the refused-connect mismatch silently.
    let shared = channel.shared.clone();
    let handle = bounded(
        "body-read-timeout start",
        channel.start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        }),
    )
    .await
    .expect("#1901 start");
    drop(channel);
    assert_bound_listener(&shared, port, 0);
    let mut stream = bounded(
        "body-read-timeout connect",
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .expect("#1901 connect to the sms listener");
    bounded("body-read-timeout send", stream.write_all(&build_raw(step)))
        .await
        .expect("#1901 send the stalled request");
    let mut response = Vec::new();
    tokio::time::timeout(CLIENT_DEADLINE, stream.read_to_end(&mut response))
        .await
        .expect("#1901 a stalled body is answered at the sms body-read deadline")
        .expect("#1901 read the stalled-body response bytes");
    bounded("body-read-timeout stop", handle.stop("body-read-timeout"))
        .await
        .expect("#1901 stop after the body-read deadline");
    // Only the answer is compared here. The cell also records the stop logs,
    // but this replay deliberately stops with the `body-read-timeout` reason
    // while the oracle recorded `no reason`, so the log envelope cannot be
    // compared without normalizing the reason string; the shutdown log path is
    // covered by the other cells' `stop()` steps.
    crate::negative_assert_eq!(
        normalize_wire(&response),
        normalize_wire(REQUEST_TIMEOUT_408),
        "#1901 the sms body-read deadline answers Node's 408 in hyper's \
         chunked framing and closes the connection (normalized only for the \
         A8.3-CH-SMS-002 / -010 header case and date presentation)"
    );
}

/// The pinned 408 bytes are Node's raw 51 bytes minus their terminating CRLF
/// plus hyper's chunked framing: the presentation residual registered for this
/// lane in `A8.3-CH-SMS-010`, not "the Node 408" byte for byte.
#[test]
fn sms_408_is_node_bytes_plus_hyper_framing() {
    let node = crate::user_dialog::NODE_408_RESPONSE;
    let mut expected = node[..node.len() - 2].to_vec();
    expected.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n0\r\n\r\n");
    crate::negative_assert_eq!(
        REQUEST_TIMEOUT_408,
        expected.as_slice(),
        "#1901 hyper's 408 is Node's unframed 408 plus the chunked framing"
    );
}

/// #1901: nothing else pins the 300 s constant — the replay injects 200 ms and
/// the oracle cell's own answer is not replayed, so the default could drift to
/// 30 s or 1 h with every other check still green.
#[test]
fn body_read_deadline_defaults_to_the_node_request_timeout() {
    assert_eq!(
        DEFAULT_REQUEST_TIMEOUT,
        Duration::from_secs(300),
        "#1901 the body-read deadline must stay at Node's default requestTimeout"
    );
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: None,
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: None,
        record_unhandled: None,
        request_timeout: None,
    });
    assert_eq!(
        channel.shared.request_timeout, DEFAULT_REQUEST_TIMEOUT,
        "#1901 an unset option means the Node default, not a shorter deadline"
    );
}

async fn replay_named(name: &str) {
    let case = case_by_name(name);
    validate_mock_recording(&case);
    let steps = recorded_steps(&case).expect("#1339 steps validated array");
    assert_boundary_body_len(name, &case);
    let port = case["port"].as_u64().expect("#1339 case port") as u16;
    // #2691: the case's own effective host, so the guard probes the address
    // this very cell's product bind will use ("" -> the `::` wildcard,
    // 127.0.0.1 -> the fixed host that never touches IPv6).
    let host = effective_webhook_host(recorded_extra(&case));
    ensure_replay_port_free(port, &host).await;
    // #2691 fix-r2: the post-start guard below asserts a live listener, so
    // it runs only for the cells whose recording shows one
    // ([`expects_live_listener`]).
    let live_listener_expected = expects_live_listener(&case);
    if name == "body-read-timeout" {
        // #1901: the recorded TS answer arrives only after Node's full 300 s
        // `requestTimeout`, so the cell is replayed through a shortened
        // injected deadline instead of being waited out.
        replay_body_read_timeout(&case, port).await;
        return;
    }
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
        request_timeout: None,
    });
    // #2691 fix-r2: the strong-count guard handle. #2691 fix-r3 (review r3,
    // three roles independently): the channel stays alive (the `cron` steps
    // below call `channel.deliver_cron_result`), so it is *not* dropped the
    // way the drop-form call sites drop theirs — the guard threshold must
    // count it (no-op: channel + this clone = 2; started: channel + clone +
    // handle + accept task = 4), see [`OWNERS_KEPT_ALIVE_ACROSS_START`].
    let shared = channel.shared.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: Some(logger),
            turn_gateway: None,
        })
        .await
        .expect("#1339 start");
    if live_listener_expected {
        assert_bound_listener(&shared, port, OWNERS_KEPT_ALIVE_ACROSS_START);
    }

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
                let reason = replay_stop_reason(step);
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
    // #1901: replayed by `replay_body_read_timeout` (Node's 300 s deadline is
    // shortened through the injected option instead of being waited out).
    "body-read-timeout" => body_read_timeout,
    // #1913: the generic replay is the guard — the recorded step is a 1.0
    // keep-alive request, and `raw_request` only returns on the connection
    // close, so a lost finalize (the socket stays open waiting out the
    // keep-alive) times out at the `bounded` 5 s probe deadline.
    "http10" => http10,
    // #2152: the parser-admission cells — the oracle records llhttp's bare
    // 400 for `FROB` and `_http_server.js`'s headless 400, both before the
    // route guard could answer its 404.
    "unknown-method" => unknown_method,
    "missing-host" => missing_host,
}

/// #1913: an HTTP/1.0 request — even one asking for `Connection: keep-alive`
/// — draws a close-delimited response and a closed socket, as Node does (the
/// pinned Node v22.22.0 measurement lives in
/// `_tmp/20261108-sms-residual/probe-sms-http10.out`). The status line is
/// one difference: Node answers `HTTP/1.1`, hyper writes its own `HTTP/1.0`
/// (the accepted-diff entry A8.3-CH-SMS-015, the same class as the webhook
/// lane's A8.3-CH-WEBHOOK-004) — the connection-header residual is the
/// fix-r1 note right below. The replay cell above cannot see any of this — the
/// oracle records only status/content-type/body, so the status-line version
/// and the framing would drift silently — this test reads the raw wire.
/// #2257 fix-r1: the status line is not the only wire-visible difference —
/// Node's answer carries `Connection: close` where hyper's 1.0 branch writes
/// no `connection` header at all (close is the 1.0 default), a header-set
/// residual of the lane-level A8.3-CH-SMS-002 family — so the pin below
/// holds the hyper side to its measured shape.
#[tokio::test]
async fn http10_request_closes_the_connection() {
    // #2257 fix-r4 (review r4 P1): 18904 sat inside the wecom-callback golden
    // replay segment (18900–18921 — its post-rejects case listens on 18904)
    // and the two tests run in parallel in the same lib binary; the pin owns
    // a port in the free gap above that segment (18922 is the 404 pin).
    const PORT: u16 = 18923;
    let case = case_by_name("http10");
    let steps = recorded_steps(&case).expect("#1339 steps validated array");
    let request = build_raw(
        steps
            .first()
            .expect("#1913 the http10 case carries one request step"),
    );
    assert!(
        request.starts_with(b"POST /sms/incoming HTTP/1.0\r\n")
            && request
                .windows(b"Connection: keep-alive".len())
                .any(|window| window == b"Connection: keep-alive"),
        "#1913 pin http10: the oracle step stays an HTTP/1.0 keep-alive request: {request:?}"
    );
    // The recorded case's own options, not a hand-built twin: a regenerated
    // cell that changes the account or the path must fail here too. The port
    // is the pin's own — the replay cell owns the recorded 18856 — and with
    // no gateway the dispatched turn dies in its spawned task (logged into
    // the void, the recorder is absent) long after the 200 TWIML is already
    // on the wire.
    let mut extra = recorded_extra(&case).cloned().unwrap_or_else(|| json!({}));
    extra["webhookPort"] = json!(PORT);
    // #2257 fix-r3: `start` swallows a bind failure into a NoopHandle, so a
    // port collision would silently test someone else's listener — preflight
    // the port like the replay cells do. #2691: with the host this pin's
    // own extra resolves to (the recorded `127.0.0.1`), so the v6 half of
    // the guard does not fail the pin on a v6-only occupier the product
    // never collides with.
    let host = effective_webhook_host(Some(&extra));
    ensure_replay_port_free(PORT, &host).await;
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(&extra)),
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: Some(null_factory()),
        record_unhandled: None,
        request_timeout: None,
    });
    let shared = channel.shared.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#1913 start for the http10 pin");
    // #2691: the preflight cannot see an occupier that wins the window
    // between its probe and this bind — prove the bind really happened.
    drop(channel);
    assert_bound_listener(&shared, PORT, 0);
    let raw = bounded("http10 pin response", raw_until_close(PORT, &request)).await;
    settle().await;
    bounded("http10 pin cleanup stop", handle.stop("cleanup"))
        .await
        .expect("#1913 http10 pin cleanup stop");

    assert!(
        raw.starts_with(b"HTTP/1.0 200 OK\r\n"),
        "#1913 pin http10: hyper answers with its own 1.0 status line (Node's 1.1 line is the accepted-diff entry A8.3-CH-SMS-015): {raw:?}"
    );
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .expect("#1913 pin http10: the response carries a header end");
    let framed = head_has_header_named(head, "content-length")
        || head_has_header_named(head, "transfer-encoding");
    assert!(
        !framed,
        "#1913 pin http10: the 1.0 response is close-delimited like Node's, no content-length and no chunking: {raw:?}"
    );
    // Close is HTTP/1.0's default, so hyper's 1.0 branch (`proto/h1/conn.rs`
    // `fix_keep_alive`) only disables keep-alive without writing a header;
    // Node's wire answers the same request with `Connection: close`
    // (probe-sms-http10.out), a lane-level framing residual the other native
    // lanes replay by writing the header themselves. Pin the absence: a
    // hyper change that starts writing one must update the ledger, not pass.
    let connection_header = head_has_header_named(head, "connection");
    assert!(
        !connection_header,
        "#1913 pin http10: the 1.0 response carries no `connection` header (close is 1.0's default; Node's `Connection: close` is the lane-level A8.3-CH-SMS-002 framing family): {raw:?}"
    );
    crate::negative_assert_eq!(
        body,
        TWIML_OK,
        "#1913 pin http10: the 1.0 response replays the TWIML body unchanged"
    );
}

/// Raw-wire twin of `raw_request` (#1913): the parsed probe cannot see the
/// status-line version or the framing, so the http10 pin reads the bytes.
async fn raw_until_close(port: u16, request: &[u8]) -> Vec<u8> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1913 connect for the http10 pin");
    stream
        .write_all(request)
        .await
        .expect("#1913 write the http10 request");
    // `read_to_end` only returns when the server closes: a lost finalize
    // holds the socket open and the `bounded` deadline turns that into the
    // test failure.
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("#1913 the 1.0 response must close the connection");
    raw
}

/// #2257 fix-r2: shared response-head scan for the http10 pins — `true` when
/// any header line carries `name` (case-insensitive: hyper's bare
/// `http1::Builder` writes lowercase, Node writes Title-Case). Extracted from
/// the two inline scans in `http10_request_closes_the_connection`; the scan
/// stops at the blank line so a header-looking body cannot match.
fn head_has_header_named(head: &str, name: &str) -> bool {
    head.split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .any(|line| {
            line.split_once(':')
                .is_some_and(|(header, _)| header.trim().eq_ignore_ascii_case(name))
        })
}

/// #2257 fix-r2: positive control for the absence assertions above — the
/// scan must light up on a real `Connection: close` header (Node's measured
/// wire shape, `probe-sms-http10.out`), or the pins could pass vacuously
/// through a broken scan.
#[test]
fn head_has_header_named_detects_a_present_header() {
    let node_shape =
        "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nDate: now\r\nConnection: close\r\n\r\n";
    assert!(
        head_has_header_named(node_shape, "connection")
            && head_has_header_named(node_shape, "CONNECTION")
            && head_has_header_named(node_shape, "content-type")
            && !head_has_header_named(node_shape, "x-never-set"),
        "#2257 the header scan finds present headers case-insensitively and rejects absent ones"
    );
    // A header-looking value past the blank line must not be scanned.
    let with_body = "HTTP/1.0 200 OK\r\ncontent-type: text/xml\r\n\r\nconnection: close";
    assert!(
        !head_has_header_named(with_body, "connection"),
        "#2257 the header scan stops at the header end"
    );
}

/// #2257 fix-r2: the 404 branch goes through the same `finalize` (the route
/// guard answers before the handler), so an HTTP/1.0 request to a wrong path
/// must draw the same 1.0 framing as the 200 pin above — its own status
/// line, close-delimited body, no `connection` header. Without this pin the
/// 404 path could start writing headers on its own and no test would turn
/// red (review r1 boundary).
#[tokio::test]
async fn http10_not_found_keeps_the_close_delimited_framing() {
    const PORT: u16 = 18922;
    let case = case_by_name("http10");
    let steps = recorded_steps(&case).expect("#1339 steps validated array");
    let recorded = build_raw(
        steps
            .first()
            .expect("#1913 the http10 case carries one request step"),
    );
    // Same recorded request bytes except the path: the route guard
    // (`route_request`) answers 404 before the handler sees anything, so the
    // pin's path must differ from the recorded `/sms/incoming`.
    let request: Vec<u8> = {
        let text = String::from_utf8_lossy(&recorded).into_owned();
        let (request_line, rest) = text
            .split_once("\r\n")
            .expect("#1913 the recorded http10 request carries a request line");
        let mut words = request_line.split(' ');
        let method = words.next().expect("#1913 request method");
        let _recorded_path = words.next().expect("#1913 request path");
        let version = words.next().expect("#1913 request version");
        format!("{method} /no-such-path {version}\r\n{rest}").into_bytes()
    };
    let mut extra = recorded_extra(&case).cloned().unwrap_or_else(|| json!({}));
    extra["webhookPort"] = json!(PORT);
    // #2257 fix-r3: same preflight as the 200 pin — a NoopHandle fallback on
    // a busy port would fake-pass against a foreign listener. #2691: with
    // this pin's own extra (the recorded `127.0.0.1` host).
    let host = effective_webhook_host(Some(&extra));
    ensure_replay_port_free(PORT, &host).await;
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(&extra)),
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: Some(null_factory()),
        record_unhandled: None,
        request_timeout: None,
    });
    let shared = channel.shared.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#2257 start for the http10 404 pin");
    // #2691: same post-start bind proof as the 200 pin.
    drop(channel);
    assert_bound_listener(&shared, PORT, 0);
    let raw = bounded("http10 404 pin response", raw_until_close(PORT, &request)).await;
    settle().await;
    bounded("http10 404 pin cleanup stop", handle.stop("cleanup"))
        .await
        .expect("#2257 http10 404 pin cleanup stop");

    assert!(
        raw.starts_with(b"HTTP/1.0 404"),
        "#2257 pin http10 404: hyper answers the wrong path with its own 1.0 404 status line (same A8.3-CH-SMS-015 class as the 200 pin): {raw:?}"
    );
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .expect("#2257 pin http10 404: the response carries a header end");
    assert!(
        !head_has_header_named(head, "content-length")
            && !head_has_header_named(head, "transfer-encoding"),
        "#2257 pin http10 404: the 404 stays close-delimited through finalize, no content-length and no chunking: {raw:?}"
    );
    assert!(
        !head_has_header_named(head, "connection"),
        "#2257 pin http10 404: the 1.0 404 carries no `connection` header (1.0 default close; Node's `Connection: close` is the A8.3-CH-SMS-002 framing family): {raw:?}"
    );
    assert_eq!(
        body, "Not Found",
        "#2257 pin http10 404: the static Not Found body closes the response"
    );
}

/// Start the channel of one recorded case on the pin's own port and return
/// the stoppable handle, so the two #2152 wire pins below read the raw bytes
/// the oracle cell cannot express (it records only status/content-type/body).
async fn pin_channel(
    case: &Value,
    port: u16,
) -> std::sync::Arc<dyn crate::channel::protocol::ChannelHandle> {
    let mut extra = recorded_extra(case).cloned().unwrap_or_else(|| json!({}));
    extra["webhookPort"] = json!(port);
    // #2691: the preflight follows the effective host of the case's extra
    // (same merge as the product's `String(webhookHost ?? DEFAULT_HOST)`).
    let host = effective_webhook_host(Some(&extra));
    ensure_replay_port_free(port, &host).await;
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(&extra)),
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: Some(null_factory()),
        record_unhandled: None,
        request_timeout: None,
    });
    // #2691 fix-r2 (review r2 concurrency#3 / boundary#2 / sibling#2): the
    // same post-start bind proof the fixed-port pins run — a #2152 pin whose
    // start lost the bind race would silently measure a foreign listener
    // byte-for-byte (the occupier could be another worktree replaying the
    // same oracle). The return value is unchanged.
    let shared = channel.shared.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#2152 start for the parser-admission pin");
    drop(channel);
    assert_bound_listener(&shared, port, 0);
    handle
}

/// One recorded case's first request step, rebuilt as wire bytes.
fn pin_request(case: &Value) -> Vec<u8> {
    let steps = recorded_steps(case).expect("#1339 steps validated array");
    build_raw(
        steps
            .first()
            .expect("#2152 the parser-admission case carries one request step"),
    )
}

/// The comparison basis for the bare parser 400's wire bytes (#2152): Node's
/// llhttp rejection carries `Connection: close` and zero body bytes and no
/// `Date`; hyper registers `content-length: 0` for the empty sized body — the
/// same A8.4-NATIVE-HTTP-001 transport diff the webhook and api-server lanes
/// pin for their own admission cells. Compared through [`normalize_wire`],
/// like the 408 constant above: the sms lane's bare builder writes lowercase
/// header names and hyper's implicit `date`, the two presentation residuals
/// this lane registers for the 408 (A8.3-CH-SMS-002 / -010), so on this lane
/// only the framing below — `content-length: 0` over an empty sized body —
/// separates the bare 400 from the headless 400's chunked `Date`-carrying
/// shape.
const PARSER_BAD_REQUEST_400: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";

/// #2152: `FROB` never reaches the route guard — llhttp rejects the method
/// token before any handler runs, so the wire answer is the bare parser 400,
/// not the guard's 404 (`plain_response(404, "Not Found")`). The oracle cell
/// compares only status/content-type/body, so this pin holds the exact head
/// and body it cannot see, normalized only for the two registered
/// presentation dimensions (see [`PARSER_BAD_REQUEST_400`]).
#[tokio::test]
async fn unknown_method_is_the_bare_parser_400() {
    // #2257 fix-r4: the free gap above the wecom-callback golden replay
    // segment (18900–18921); 18922–18923 are the http10 pins, 18924–18925
    // belong to `head_timeout_tests` (r1 review caught these two pins
    // sitting on them), 18926 to the json-null cell — so these pins take
    // 18927/18928.
    const PORT: u16 = 18927;
    let case = case_by_name("unknown-method");
    let request = pin_request(&case);
    assert!(
        request.starts_with(b"FROB /sms/incoming HTTP/1.1\r\n"),
        "#2152 pin unknown-method: the oracle step stays an FROB request — a cell \
         degraded to a served method would compare 200 == 200 and the gate could \
         be removed unnoticed: {request:?}"
    );
    let handle = pin_channel(&case, PORT).await;
    let raw = bounded(
        "unknown-method pin response",
        raw_until_close(PORT, &request),
    )
    .await;
    settle().await;
    bounded("unknown-method pin cleanup stop", handle.stop("cleanup"))
        .await
        .expect("#2152 unknown-method pin cleanup stop");

    assert!(
        raw.starts_with(b"HTTP/1.1 400 Bad Request\r\n"),
        "#2152 pin unknown-method: llhttp's bare 400, not a routed answer: {raw:?}"
    );
    crate::negative_assert_eq!(
        normalize_wire(&raw),
        normalize_wire(PARSER_BAD_REQUEST_400),
        "#2152 pin unknown-method: the bare parser 400's exact head and body — \
         `Connection: close`, hyper's registered `content-length: 0`, zero body \
         bytes, nothing else on the wire. A change here must update the ledger, \
         not pass: {raw:?}"
    );
}

/// #2152: an HTTP/1.1 request without a `Host` header is rejected by
/// `_http_server.js` before any handler runs. Unlike the bare parser 400
/// above, this one carries its own `Date` and a chunked empty body (Node's
/// `missingHeaderStatusLine` shape, mirrored by `headless_bad_request`
/// writing the header itself); the pin holds both, plus the absent
/// `content-length`, so the two rejections cannot drift into one another.
/// The bare 400's wire also shows a `date` on this lane — hyper's implicit
/// one over Node's dateless bytes, the registered presentation residual
/// [`normalize_wire`] drops — so the `Date` here pins the headless 400's own
/// header, not a lane-wide presence.
#[tokio::test]
async fn missing_host_is_the_headless_parser_400() {
    // #2152 fix-r1: see the sibling pin above — 18924/18925 are the
    // head-timeout ports, so this pin takes 18928.
    const PORT: u16 = 18928;
    let case = case_by_name("missing-host");
    let request = pin_request(&case);
    let request_head = String::from_utf8_lossy(&request);
    let request_head = request_head
        .split("\r\n\r\n")
        .next()
        .expect("#2152 pin missing-host: the request carries a header end");
    assert!(
        request_head
            .split("\r\n")
            .skip(1)
            .all(|line| !line.to_ascii_lowercase().starts_with("host:")),
        "#2152 pin missing-host: the oracle step stays a Host-less request — sending \
         one would turn the cell into an admitted 200: {request:?}"
    );
    let handle = pin_channel(&case, PORT).await;
    let raw = bounded("missing-host pin response", raw_until_close(PORT, &request)).await;
    settle().await;
    bounded("missing-host pin cleanup stop", handle.stop("cleanup"))
        .await
        .expect("#2152 missing-host pin cleanup stop");

    assert!(
        raw.starts_with(b"HTTP/1.1 400 Bad Request\r\n"),
        "#2152 pin missing-host: the headless 400, not a routed answer: {raw:?}"
    );
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .expect("#2152 pin missing-host: the response carries a header end");
    assert!(
        head_has_header_named(head, "date"),
        "#2152 pin missing-host: the headless 400 carries its own Date (Node's \
         missingHostResponse writes one; the bare 400's wire date on this lane \
         is hyper's implicit residual, dropped by normalize_wire): {raw:?}"
    );
    assert!(
        head_has_header_named(head, "connection"),
        "#2152 pin missing-host: the rejection closes the connection: {raw:?}"
    );
    assert!(
        head_has_header_named(head, "transfer-encoding"),
        "#2152 pin missing-host: the empty body is chunked, Node's own framing: {raw:?}"
    );
    assert!(
        !head_has_header_named(head, "content-length"),
        "#2152 pin missing-host: a chunked rejection carries no content-length: {raw:?}"
    );
    assert_eq!(
        body, "0\r\n\r\n",
        "#2152 pin missing-host: the chunked empty body terminator"
    );
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
                    crate::test_skip_line::stderr_skip_line(&format!(
                        "#1339 skipped: host has no IPv6 (`::` bind failed: {control_error}); \
                         bind_listener fell back to {fallback_address}\n"
                    ));
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
            crate::test_skip_line::stderr_skip_line(&format!(
                "#1339 skipped: host has no IPv4 (`127.0.0.1` connect failed: {v4_error})\n"
            ));
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
        request_timeout: None,
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
        request_timeout: None,
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

/// #1790: a gateway whose `permissionDecide` panics on its first call and
/// succeeds afterwards; `submitTurn` returns an empty stream. The decide call
/// log is the observable a stuck chat never grows.
struct PermissionPanickingGateway {
    decides: Arc<SyncMutex<Vec<String>>>,
}

impl SmsGateway for PermissionPanickingGateway {
    fn submit_turn(
        &self,
        _request: SubmitTurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        Box::pin(async {
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
        decision: PermissionDecision,
    ) -> BoxFuture<Result<PermissionDecisionOutcome, ChannelError>> {
        let decides = self.decides.clone();
        Box::pin(async move {
            let first = {
                let mut decides = decides.lock().expect("#1790 decides mutex");
                decides.push(String::from_utf16_lossy(decision.session_key.units()));
                decides.len() == 1
            };
            if first {
                panic!("#1790 injected permission_decide panic");
            }
            Ok(PermissionDecisionOutcome { delivered: true })
        })
    }
}

/// Seed one delivered permission prompt for `chat`, leaving the state the
/// production flow has after the user saw the prompt and the turn ended:
/// `pending` holds one entry, `answering`/`initial_prompt_pending` are clear.
fn seed_pending_permission(shared: &Arc<SmsShared>, chat: &JsString, request_id: &str) {
    let session = JsString::from(format!(
        "sms:chat={}:general",
        String::from_utf16_lossy(chat.units())
    ));
    let event = to_js(&json!({
        "requestId": request_id,
        "toolName": "Bash",
        "payload": {"command": "ls"},
    }));
    let mut permissions = shared.lock_permissions();
    let prompt = permissions.capture(chat, &session, &event);
    crate::negative_assert!(
        prompt.is_some(),
        "#1790 preset must deliver the initial permission prompt"
    );
    permissions.confirm_initial_prompt(chat, true, Some(&JsString::from(request_id)));
}

/// #1790: a panic inside `permissionDecide` must not leave the chat stuck in
/// "权限决定处理中". `begin_answer` sets `answering`/`in_flight` before the
/// await; pre-fix only the Ok/Err arms clear them, a panic skips both and the
/// chat can never advance until restart.
#[tokio::test]
async fn a_panicking_permission_decide_does_not_lock_the_chat() {
    let decides = Arc::new(SyncMutex::new(Vec::<String>::new()));
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: None,
        mapper: None,
        gateway: Some(Arc::new(PermissionPanickingGateway {
            decides: decides.clone(),
        })),
        env: None,
        twilio_factory: None,
        record_unhandled: None,
        request_timeout: None,
    });
    let shared = channel.shared.clone();
    let chat = JsString::from("+15550010");
    seed_pending_permission(&shared, &chat, "req-1790-decide");

    // The production shape: only the fire-and-forget task dies.
    let joined = tokio::spawn({
        let shared = shared.clone();
        let chat = chat.clone();
        async move { handle_incoming(&shared, chat, JsString::from("1")).await }
    })
    .await;
    let error = joined.expect_err("#1790 the injected panic must fail the spawned task");
    crate::negative_assert!(
        error.is_panic(),
        "#1790 the spawn must end in the panic, not a clean return"
    );

    // C1: the answer flags must not survive the unwind.
    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_answering(&chat),
            "#1790 answering must not survive the permission_decide panic"
        );
        crate::negative_assert!(
            !permissions.is_in_flight(&chat),
            "#1790 in_flight must not survive the permission_decide panic"
        );
    }
    // C2: the chat must still be able to decide — the second "1" reaches the
    // gateway instead of the "权限决定处理中" status short-circuit.
    handle_incoming(&shared, chat.clone(), JsString::from("1"))
        .await
        .expect("#1790 the decision after the panic returns Ok");
    crate::negative_assert_eq!(
        decides.lock().expect("#1790 decides mutex").len(),
        2,
        "#1790 the chat must be able to decide again after the panic"
    );
}

/// #1790: a loopback sender that records every body and can panic on the first
/// body carrying a marker. The one-shot latch mirrors a provider that fails
/// once; the retry after the panic must be able to succeed.
struct ProbeSender {
    sends: Arc<SyncMutex<Vec<String>>>,
    panic_on_marker: Option<&'static str>,
    panicked: Arc<SyncMutex<bool>>,
    /// Runs once, right before the injected panic — lets a test interleave a
    /// second event (e.g. a permission captured mid-send).
    before_panic: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl ProbeSender {
    fn new(panic_on_marker: Option<&'static str>) -> (Self, Arc<SyncMutex<Vec<String>>>) {
        Self::with_hook(panic_on_marker, None)
    }

    fn with_hook(
        panic_on_marker: Option<&'static str>,
        before_panic: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> (Self, Arc<SyncMutex<Vec<String>>>) {
        let sends = Arc::new(SyncMutex::new(Vec::new()));
        let sender = Self {
            sends: sends.clone(),
            panic_on_marker,
            panicked: Arc::new(SyncMutex::new(false)),
            before_panic,
        };
        (sender, sends)
    }
}

impl SmsSender for ProbeSender {
    fn send(&self, message: SmsOutgoingMessage) -> BoxFuture<Result<(), ChannelError>> {
        let body = String::from_utf16_lossy(message.body.units());
        let sends = self.sends.clone();
        let marker = self.panic_on_marker;
        let panicked = self.panicked.clone();
        let before_panic = self.before_panic.clone();
        Box::pin(async move {
            sends.lock().expect("#1790 sends mutex").push(body.clone());
            let should_panic = marker.is_some_and(|marker| body.contains(marker));
            if should_panic {
                let mut flag = panicked.lock().expect("#1790 panic flag mutex");
                if !*flag {
                    *flag = true;
                    drop(flag);
                    if let Some(hook) = before_panic {
                        hook();
                    }
                    panic!("#1790 injected prompt-send panic");
                }
            }
            Ok(())
        })
    }
}

/// Install `sender` as the channel's Twilio boundary; `send_reply` then
/// reaches it instead of returning the "twilio not installed" false.
async fn install_sender(shared: &Arc<SmsShared>, sender: impl SmsSender + 'static) {
    shared.runtime.lock().await.client = Some(Arc::new(sender));
}

/// Build a bare channel around `gateway` (no Twilio boundary, no extra cfg).
fn channel_with_gateway(gateway: Arc<dyn SmsGateway>) -> Arc<SmsShared> {
    SmsChannel::new(SmsChannelOptions {
        extra: None,
        mapper: None,
        gateway: Some(gateway),
        env: None,
        twilio_factory: None,
        record_unhandled: None,
        request_timeout: None,
    })
    .shared
    .clone()
}

/// Count recorded bodies carrying the permission-prompt marker.
fn prompt_sends(sends: &Arc<SyncMutex<Vec<String>>>) -> usize {
    sends
        .lock()
        .expect("#1790 sends mutex")
        .iter()
        .filter(|body| body.contains("需要权限才能继续执行。"))
        .count()
}

/// #1790: `permissionDecide` always succeeds; `submitTurn` yields one
/// `permission_request`, which drives the initial-prompt send through
/// `process_message`.
struct PermissionRequestGateway {
    request_id: &'static str,
}

impl SmsGateway for PermissionRequestGateway {
    fn submit_turn(
        &self,
        _request: SubmitTurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let event = to_js(&json!({
            "type": "permission_request",
            "requestId": self.request_id,
            "toolName": "Bash",
            "payload": {"command": "ls"},
        }));
        Box::pin(async move {
            Ok(Box::new(MockTurnStream {
                events: VecDeque::from([event]),
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

/// #1790: `permissionDecide` always succeeds; `submitTurn` yields an empty
/// stream and records the submitted text. Drives the permission branch without
/// any gateway-side failure.
struct DecideOkGateway {
    submits: Arc<SyncMutex<Vec<String>>>,
}

impl DecideOkGateway {
    fn new() -> Self {
        Self {
            submits: Arc::new(SyncMutex::new(Vec::new())),
        }
    }
}

impl SmsGateway for DecideOkGateway {
    fn submit_turn(
        &self,
        request: SubmitTurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.submits
            .lock()
            .expect("#1790 submits mutex")
            .push(String::from_utf16_lossy(request.message.units()));
        Box::pin(async {
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

/// #1790: `permissionDecide` rejects on the first call and succeeds after —
/// the no-panic regression that proves the flags do not linger across an
/// error/retry either.
struct DecisionErrThenOkGateway {
    decides: Arc<SyncMutex<Vec<String>>>,
}

impl SmsGateway for DecisionErrThenOkGateway {
    fn submit_turn(
        &self,
        _request: SubmitTurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        Box::pin(async {
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
        decision: PermissionDecision,
    ) -> BoxFuture<Result<PermissionDecisionOutcome, ChannelError>> {
        let decides = self.decides.clone();
        Box::pin(async move {
            let count = {
                let mut decides = decides.lock().expect("#1790 decides mutex");
                decides.push(String::from_utf16_lossy(decision.session_key.units()));
                decides.len()
            };
            if count == 1 {
                return Err(ChannelError::Error(
                    "#1790 injected decide error".to_owned(),
                ));
            }
            Ok(PermissionDecisionOutcome { delivered: true })
        })
    }
}

/// #1790: the first `permissionDecide` parks forever behind a gate, so the
/// caller can be cancelled or timed out mid-await; every later call succeeds.
struct ParkingDecisionGateway {
    decides: Arc<SyncMutex<Vec<String>>>,
    gates: Gates,
}

impl SmsGateway for ParkingDecisionGateway {
    fn submit_turn(
        &self,
        _request: SubmitTurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        Box::pin(async {
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
        decision: PermissionDecision,
    ) -> BoxFuture<Result<PermissionDecisionOutcome, ChannelError>> {
        let decides = self.decides.clone();
        let gates = self.gates.clone();
        Box::pin(async move {
            let first = {
                let mut decides = decides.lock().expect("#1790 decides mutex");
                decides.push(String::from_utf16_lossy(decision.session_key.units()));
                decides.len() == 1
            };
            if first {
                released(gate(&gates, "1790-park")).await;
            }
            Ok(PermissionDecisionOutcome { delivered: true })
        })
    }
}

/// Seed two delivered/queued permissions so the first decision has a queued
/// follow-up prompt to deliver (`next_prompts` is only rebuilt by
/// `finish_answer` from `pending`).
fn seed_two_pending_permissions(shared: &Arc<SmsShared>, chat: &JsString) {
    seed_pending_permission(shared, chat, "req-1790-first");
    let session = JsString::from(format!(
        "sms:chat={}:general",
        String::from_utf16_lossy(chat.units())
    ));
    let event = to_js(&json!({
        "requestId": "req-1790-second",
        "toolName": "Bash",
        "payload": {"command": "pwd"},
    }));
    let mut permissions = shared.lock_permissions();
    let prompt = permissions.capture(chat, &session, &event);
    crate::negative_assert!(
        prompt.is_none(),
        "#1790 the second capture must queue without a prompt"
    );
}

/// #1790: a panic while the *next* prompt is being delivered must release
/// `prompt_delivering`. Pre-fix `take_next_prompt` refuses forever and every
/// later message is silently dropped (`BeginAnswer::None`).
#[tokio::test]
async fn a_panicking_prompt_delivery_does_not_lock_the_chat() {
    let shared = channel_with_gateway(Arc::new(DecideOkGateway::new()));
    let chat = JsString::from("+15550011");
    seed_two_pending_permissions(&shared, &chat);
    let (sender, sends) = ProbeSender::new(Some("需要权限才能继续执行。"));
    install_sender(&shared, sender).await;

    let joined = tokio::spawn({
        let shared = shared.clone();
        let chat = chat.clone();
        async move { handle_incoming(&shared, chat, JsString::from("1")).await }
    })
    .await;
    let error = joined.expect_err("#1790 the injected prompt-send panic must fail the task");
    crate::negative_assert!(
        error.is_panic(),
        "#1790 the prompt-send spawn must end in the panic"
    );

    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_prompt_delivering(&chat),
            "#1790 prompt_delivering must not survive the prompt-send panic"
        );
    }
    // The queued prompt is still pending: the next decision retries delivery
    // and only then releases the answer lock.
    handle_incoming(&shared, chat.clone(), JsString::from("1"))
        .await
        .expect("#1790 the retry after the prompt panic returns Ok");
    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_prompt_delivering(&chat),
            "#1790 the retried prompt delivery must clear prompt_delivering"
        );
        crate::negative_assert!(
            !permissions.is_answering(&chat),
            "#1790 the retried prompt delivery must release the answer lock"
        );
    }
    crate::negative_assert_eq!(
        prompt_sends(&sends),
        2,
        "#1790 the panic and the retry each send one prompt"
    );
}

/// #1790: the confirmation send also holds `answering` across an await (after
/// `finish_answer`, before any next prompt). A panic there must settle the
/// answer instead of leaving every later message dropped as
/// `BeginAnswer::None`.
#[tokio::test]
async fn a_panicking_confirmation_send_does_not_lock_the_chat() {
    let gateway = Arc::new(DecideOkGateway::new());
    let submits = gateway.submits.clone();
    let shared = channel_with_gateway(gateway);
    let chat = JsString::from("+15550016");
    seed_pending_permission(&shared, &chat, "req-1790-confirm");
    let (sender, _sends) = ProbeSender::new(Some("已允许一次"));
    install_sender(&shared, sender).await;

    let joined = tokio::spawn({
        let shared = shared.clone();
        let chat = chat.clone();
        async move { handle_incoming(&shared, chat, JsString::from("1")).await }
    })
    .await;
    let error = joined.expect_err("#1790 the injected confirmation panic must fail the task");
    crate::negative_assert!(
        error.is_panic(),
        "#1790 the confirmation spawn must end in the panic"
    );

    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_answering(&chat),
            "#1790 answering must not survive the confirmation-send panic"
        );
        crate::negative_assert!(
            !permissions.is_in_flight(&chat),
            "#1790 in_flight must not survive the confirmation-send panic"
        );
        crate::negative_assert!(
            !permissions.has_pending(&chat),
            "#1790 the confirmation panic must settle the answered permission"
        );
    }
    // The chat still advances: an ordinary message now reaches the turn.
    handle_incoming(&shared, chat.clone(), JsString::from("hello"))
        .await
        .expect("#1790 the message after the confirmation panic returns Ok");
    crate::negative_assert_eq!(
        submits.lock().expect("#1790 submits mutex").len(),
        1,
        "#1790 the chat must run a turn after the confirmation panic"
    );
}

/// #1790 (review P1): a permission captured *while* the confirmation is being
/// sent is queued in `pending` with no prompt of its own (`capture` returns
/// `None` once `answering` is set) and `confirm_answer` bails on the non-empty
/// queue. A panic on that send used to leave `answering` set with nothing
/// delivering and nothing queued — every later message dropped as
/// `BeginAnswer::None`. The guard's recovery promotes the queued head to a
/// retry prompt instead.
#[tokio::test]
async fn a_permission_captured_during_the_confirmation_send_is_not_dropped() {
    let shared = channel_with_gateway(Arc::new(DecideOkGateway::new()));
    let chat = JsString::from("+15550019");
    seed_pending_permission(&shared, &chat, "req-1790-during-first");
    let inject = {
        let shared = shared.clone();
        let chat = chat.clone();
        Arc::new(move || {
            let session = JsString::from(format!(
                "sms:chat={}:general",
                String::from_utf16_lossy(chat.units())
            ));
            let event = to_js(&json!({
                "requestId": "req-1790-during-second",
                "toolName": "Bash",
                "payload": {"command": "ls"},
            }));
            // Mid-send: `answering` is set, so this only queues the request.
            let prompt = shared.lock_permissions().capture(&chat, &session, &event);
            crate::negative_assert!(
                prompt.is_none(),
                "#1790 capture during the confirmation send must only queue"
            );
        }) as Arc<dyn Fn() + Send + Sync>
    };
    let (sender, sends) = ProbeSender::with_hook(Some("已允许一次"), Some(inject));
    install_sender(&shared, sender).await;

    let joined = tokio::spawn({
        let shared = shared.clone();
        let chat = chat.clone();
        async move { handle_incoming(&shared, chat, JsString::from("1")).await }
    })
    .await;
    let error = joined.expect_err("#1790 the injected confirmation panic must fail the task");
    crate::negative_assert!(
        error.is_panic(),
        "#1790 the confirmation spawn must end in the panic"
    );

    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            permissions.is_answering(&chat),
            "#1790 the answered permission is settled but the queued one is not"
        );
        crate::negative_assert!(
            permissions.has_pending(&chat),
            "#1790 the permission captured mid-send must still be queued"
        );
        crate::negative_assert!(
            !permissions.is_in_flight(&chat),
            "#1790 in_flight must not survive the confirmation-send panic"
        );
        crate::negative_assert!(
            !permissions.is_prompt_delivering(&chat),
            "#1790 prompt_delivering must not survive the confirmation-send panic"
        );
    }
    // The queued request must reach the user on the next message instead of
    // being dropped with the chat locked.
    handle_incoming(&shared, chat.clone(), JsString::from("hello"))
        .await
        .expect("#1790 the message after the confirmation panic returns Ok");
    crate::negative_assert_eq!(
        prompt_sends(&sends),
        1,
        "#1790 the permission captured mid-send must be re-sent after the panic"
    );
    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_answering(&chat),
            "#1790 the retried prompt must release the answer lock"
        );
        crate::negative_assert!(
            permissions.has_pending(&chat),
            "#1790 the retried request must stay queued until it is answered"
        );
    }
}

/// Fails the confirmation send (the `!delivered` branch) after running a hook,
/// so a test can queue a second permission into that window.
struct FailingConfirmationSender {
    sends: Arc<SyncMutex<Vec<String>>>,
    hook: Arc<dyn Fn() + Send + Sync>,
}

impl SmsSender for FailingConfirmationSender {
    fn send(&self, message: SmsOutgoingMessage) -> BoxFuture<Result<(), ChannelError>> {
        let body = String::from_utf16_lossy(message.body.units());
        self.sends
            .lock()
            .expect("#1790 sends mutex")
            .push(body.clone());
        let hook = self.hook.clone();
        Box::pin(async move {
            if body.contains("已允许一次") {
                hook();
                return Err(ChannelError::Plain(
                    "#1790 injected send failure".to_owned(),
                ));
            }
            Ok(())
        })
    }
}

/// #1790: the `!delivered` branch has the same residue as the guard path — an
/// undelivered confirmation with a permission captured meanwhile must still be
/// re-sent instead of wedging the chat.
#[tokio::test]
async fn an_undelivered_confirmation_does_not_lock_the_chat() {
    let shared = channel_with_gateway(Arc::new(DecideOkGateway::new()));
    let chat = JsString::from("+15550023");
    seed_pending_permission(&shared, &chat, "req-1790-undelivered-first");
    let sends = Arc::new(SyncMutex::new(Vec::new()));
    let inject = {
        let shared = shared.clone();
        let chat = chat.clone();
        Arc::new(move || {
            let session = JsString::from(format!(
                "sms:chat={}:general",
                String::from_utf16_lossy(chat.units())
            ));
            let event = to_js(&json!({
                "requestId": "req-1790-undelivered-second",
                "toolName": "Bash",
                "payload": {"command": "ls"},
            }));
            let prompt = shared.lock_permissions().capture(&chat, &session, &event);
            crate::negative_assert!(
                prompt.is_none(),
                "#1790 capture during the confirmation send must only queue"
            );
        }) as Arc<dyn Fn() + Send + Sync>
    };
    install_sender(
        &shared,
        FailingConfirmationSender {
            sends: sends.clone(),
            hook: inject,
        },
    )
    .await;

    handle_incoming(&shared, chat.clone(), JsString::from("1"))
        .await
        .expect("#1790 the failed confirmation still returns Ok");
    handle_incoming(&shared, chat.clone(), JsString::from("hello"))
        .await
        .expect("#1790 the message after the failed confirmation returns Ok");
    crate::negative_assert_eq!(
        prompt_sends(&sends),
        1,
        "#1790 the permission captured mid-send must be re-sent after the send failure"
    );
    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_answering(&chat),
            "#1790 the retried prompt must release the answer lock"
        );
        crate::negative_assert!(
            permissions.has_pending(&chat),
            "#1790 the retried request must stay queued until it is answered"
        );
    }
}

/// #1790: a panic while the *initial* permission prompt is being sent must
/// release `initial_prompt_pending`. Pre-fix every later message gets
/// "权限提示发送中，请稍候。" forever.
#[tokio::test]
async fn a_panicking_initial_prompt_delivery_does_not_lock_the_chat() {
    let shared = channel_with_gateway(Arc::new(PermissionRequestGateway {
        request_id: "req-1790-initial",
    }));
    let chat = JsString::from("+15550012");
    let (sender, sends) = ProbeSender::new(Some("需要权限才能继续执行。"));
    install_sender(&shared, sender).await;

    let joined = tokio::spawn({
        let shared = shared.clone();
        let chat = chat.clone();
        async move { handle_incoming(&shared, chat, JsString::from("hello")).await }
    })
    .await;
    let error = joined.expect_err("#1790 the injected initial-prompt panic must fail the task");
    crate::negative_assert!(
        error.is_panic(),
        "#1790 the initial-prompt spawn must end in the panic"
    );

    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_initial_prompt_pending(&chat),
            "#1790 initial_prompt_pending must not survive the prompt-send panic"
        );
        crate::negative_assert!(
            !permissions.is_in_flight(&chat),
            "#1790 in_flight must not survive the initial-prompt panic"
        );
    }
    // The captured request is still queued; the next "1" retries the prompt
    // and settles the answer.
    handle_incoming(&shared, chat.clone(), JsString::from("1"))
        .await
        .expect("#1790 the retry after the initial-prompt panic returns Ok");
    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_initial_prompt_pending(&chat),
            "#1790 the retried prompt delivery must clear initial_prompt_pending"
        );
        crate::negative_assert!(
            !permissions.is_answering(&chat),
            "#1790 the retried prompt delivery must release the answer lock"
        );
    }
    crate::negative_assert_eq!(
        prompt_sends(&sends),
        2,
        "#1790 the panic and the retry each send one prompt"
    );
}

/// #1790 (review P2 #2): the initial-prompt guard's cancel path — the adapter
/// stopping while the first prompt send is parked must still run
/// `confirm_initial_prompt(false)`, otherwise every later message keeps
/// getting "权限提示发送中，请稍候。".
#[tokio::test]
async fn cancelled_initial_prompt_delivery_does_not_lock_the_chat() {
    let shared = channel_with_gateway(Arc::new(PermissionRequestGateway {
        request_id: "req-1790-initial-cancel",
    }));
    let chat = JsString::from("+15550022");
    let sends = Arc::new(SyncMutex::new(Vec::new()));
    install_sender(
        &shared,
        ParkingSender {
            sends: sends.clone(),
        },
    )
    .await;

    let handle = tokio::spawn({
        let shared = shared.clone();
        let chat = chat.clone();
        async move { handle_incoming(&shared, chat, JsString::from("hello")).await }
    });
    until("the initial prompt send is parked", || {
        sends.lock().expect("#1790 sends mutex").len() == 1
    })
    .await;
    handle.abort();
    let error = handle
        .await
        .expect_err("#1790 the aborted task must not complete");
    crate::negative_assert!(
        error.is_cancelled(),
        "#1790 the aborted initial-prompt send must report cancellation"
    );

    let permissions = shared.lock_permissions();
    crate::negative_assert!(
        !permissions.is_initial_prompt_pending(&chat),
        "#1790 initial_prompt_pending must not survive the initial-prompt-send cancellation"
    );
    crate::negative_assert!(
        !permissions.is_in_flight(&chat),
        "#1790 in_flight must not survive the initial-prompt-send cancellation"
    );
    crate::negative_assert!(
        permissions.has_pending(&chat),
        "#1790 the captured request must stay queued after the cancelled prompt send"
    );
}

/// #1790: an error return must not leave the flags set either — the same
/// release runs without any panic, so the guard is not the only path.
#[tokio::test]
async fn permission_flags_do_not_linger_without_a_panic() {
    let decides = Arc::new(SyncMutex::new(Vec::<String>::new()));
    let shared = channel_with_gateway(Arc::new(DecisionErrThenOkGateway {
        decides: decides.clone(),
    }));
    let chat = JsString::from("+15550013");
    seed_pending_permission(&shared, &chat, "req-1790-retry");
    let (sender, sends) = ProbeSender::new(None);
    install_sender(&shared, sender).await;

    // First decision rejects: the entry is restored and the flags are clear.
    handle_incoming(&shared, chat.clone(), JsString::from("1"))
        .await
        .expect("#1790 the rejected decision still returns Ok");
    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_answering(&chat),
            "#1790 answering must not linger after a rejected decision"
        );
        crate::negative_assert!(
            !permissions.is_in_flight(&chat),
            "#1790 in_flight must not linger after a rejected decision"
        );
        crate::negative_assert!(
            permissions.has_pending(&chat),
            "#1790 the rejected decision must restore the pending permission"
        );
    }
    // Second decision succeeds: no flag survives a normal completion either.
    handle_incoming(&shared, chat.clone(), JsString::from("1"))
        .await
        .expect("#1790 the retried decision returns Ok");
    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_answering(&chat),
            "#1790 answering must not linger after the retried decision"
        );
        crate::negative_assert!(
            !permissions.is_in_flight(&chat),
            "#1790 in_flight must not linger after the retried decision"
        );
        crate::negative_assert!(
            !permissions.is_prompt_delivering(&chat),
            "#1790 prompt_delivering must not linger after the retried decision"
        );
        crate::negative_assert!(
            !permissions.has_pending(&chat),
            "#1790 the retried decision must consume the pending permission"
        );
    }
    crate::negative_assert_eq!(
        decides.lock().expect("#1790 decides mutex").len(),
        2,
        "#1790 both decisions reach the gateway"
    );
    crate::negative_assert_eq!(
        sends
            .lock()
            .expect("#1790 sends mutex")
            .iter()
            .filter(|body| body.contains("已允许一次，继续执行。"))
            .count(),
        1,
        "#1790 only the successful decision sends a confirmation"
    );
}

/// #1790: the fire-and-forget task can be cancelled while the decision is in
/// flight (channel stop / task abort). The guard must release the flags on
/// drop, or the chat is stuck until restart.
#[tokio::test]
async fn cancelled_permission_decide_does_not_lock_the_chat() {
    let decides = Arc::new(SyncMutex::new(Vec::<String>::new()));
    let shared = channel_with_gateway(Arc::new(ParkingDecisionGateway {
        decides: decides.clone(),
        gates: Arc::new(SyncMutex::new(HashMap::new())),
    }));
    let chat = JsString::from("+15550014");
    seed_pending_permission(&shared, &chat, "req-1790-cancel");

    let handle = tokio::spawn({
        let shared = shared.clone();
        let chat = chat.clone();
        async move { handle_incoming(&shared, chat, JsString::from("1")).await }
    });
    until("the decision is parked", || {
        decides.lock().expect("#1790 decides mutex").len() == 1
    })
    .await;
    handle.abort();
    let error = handle
        .await
        .expect_err("#1790 the aborted task must not complete");
    crate::negative_assert!(
        error.is_cancelled(),
        "#1790 the aborted task must report cancellation, not a panic"
    );

    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_answering(&chat),
            "#1790 answering must not survive cancellation"
        );
        crate::negative_assert!(
            !permissions.is_in_flight(&chat),
            "#1790 in_flight must not survive cancellation"
        );
        crate::negative_assert!(
            permissions.has_pending(&chat),
            "#1790 cancellation must restore the pending permission"
        );
    }
    // The chat still advances: a later decision reaches the gateway again.
    handle_incoming(&shared, chat.clone(), JsString::from("1"))
        .await
        .expect("#1790 the decision after cancellation returns Ok");
    crate::negative_assert_eq!(
        decides.lock().expect("#1790 decides mutex").len(),
        2,
        "#1790 the chat must be able to decide again after cancellation"
    );
}

/// #1790: the same release must run when the caller times out the in-flight
/// decision instead of cancelling the whole task.
#[tokio::test]
async fn timed_out_permission_decide_does_not_lock_the_chat() {
    let decides = Arc::new(SyncMutex::new(Vec::<String>::new()));
    let shared = channel_with_gateway(Arc::new(ParkingDecisionGateway {
        decides: decides.clone(),
        gates: Arc::new(SyncMutex::new(HashMap::new())),
    }));
    let chat = JsString::from("+15550015");
    seed_pending_permission(&shared, &chat, "req-1790-timeout");

    let timed_out = tokio::time::timeout(
        Duration::from_millis(50),
        handle_incoming(&shared, chat.clone(), JsString::from("1")),
    )
    .await;
    crate::negative_assert!(
        timed_out.is_err(),
        "#1790 the parked decision must time out"
    );
    crate::negative_assert_eq!(
        decides.lock().expect("#1790 decides mutex").len(),
        1,
        "#1790 the timed-out decision reached the gateway once"
    );

    {
        let permissions = shared.lock_permissions();
        crate::negative_assert!(
            !permissions.is_answering(&chat),
            "#1790 answering must not survive the timeout"
        );
        crate::negative_assert!(
            !permissions.is_in_flight(&chat),
            "#1790 in_flight must not survive the timeout"
        );
        crate::negative_assert!(
            permissions.has_pending(&chat),
            "#1790 the timeout must restore the pending permission"
        );
    }
    handle_incoming(&shared, chat.clone(), JsString::from("1"))
        .await
        .expect("#1790 the decision after the timeout returns Ok");
    crate::negative_assert_eq!(
        decides.lock().expect("#1790 decides mutex").len(),
        2,
        "#1790 the chat must be able to decide again after the timeout"
    );
}

/// #1790 (review P2 #2): the answer guard's cancel/timeout paths are covered
/// above; the prompt guard shares the same `Drop` shape and only had panic
/// tests. A send that parks forever models the adapter stopping mid-send.
struct ParkingSender {
    sends: Arc<SyncMutex<Vec<String>>>,
}

impl SmsSender for ParkingSender {
    fn send(&self, message: SmsOutgoingMessage) -> BoxFuture<Result<(), ChannelError>> {
        let body = String::from_utf16_lossy(message.body.units());
        let sends = self.sends.clone();
        Box::pin(async move {
            sends.lock().expect("#1790 sends mutex").push(body);
            std::future::pending::<Result<(), ChannelError>>().await
        })
    }
}

async fn park_confirmation(
    chat_id: &str,
    request_id: &str,
) -> (Arc<SmsShared>, JsString, Arc<SyncMutex<Vec<String>>>) {
    let shared = channel_with_gateway(Arc::new(DecideOkGateway::new()));
    let chat = JsString::from(chat_id);
    seed_pending_permission(&shared, &chat, request_id);
    let sends = Arc::new(SyncMutex::new(Vec::new()));
    install_sender(
        &shared,
        ParkingSender {
            sends: sends.clone(),
        },
    )
    .await;
    (shared, chat, sends)
}

/// #1790: aborting the task while the confirmation send is parked must settle
/// the answer — the cancel path of the prompt guard.
#[tokio::test]
async fn cancelled_confirmation_send_does_not_lock_the_chat() {
    let (shared, chat, sends) = park_confirmation("+15550020", "req-1790-cancel-confirm").await;
    let handle = tokio::spawn({
        let shared = shared.clone();
        let chat = chat.clone();
        async move { handle_incoming(&shared, chat, JsString::from("1")).await }
    });
    until("the confirmation send is parked", || {
        sends.lock().expect("#1790 sends mutex").len() == 1
    })
    .await;
    handle.abort();
    let error = handle
        .await
        .expect_err("#1790 the aborted task must not complete");
    crate::negative_assert!(
        error.is_cancelled(),
        "#1790 the aborted confirmation send must report cancellation"
    );

    let permissions = shared.lock_permissions();
    crate::negative_assert!(
        !permissions.is_answering(&chat),
        "#1790 answering must not survive the confirmation-send cancellation"
    );
    crate::negative_assert!(
        !permissions.is_in_flight(&chat),
        "#1790 in_flight must not survive the confirmation-send cancellation"
    );
    crate::negative_assert!(
        !permissions.has_pending(&chat),
        "#1790 the cancelled confirmation must settle the answered permission"
    );
}

/// #1790: the same release must run when the caller times out a parked
/// confirmation send instead of aborting the task.
#[tokio::test]
async fn timed_out_confirmation_send_does_not_lock_the_chat() {
    let (shared, chat, sends) = park_confirmation("+15550021", "req-1790-timeout-confirm").await;
    let timed_out = tokio::time::timeout(
        Duration::from_millis(50),
        handle_incoming(&shared, chat.clone(), JsString::from("1")),
    )
    .await;
    crate::negative_assert!(
        timed_out.is_err(),
        "#1790 the parked confirmation send must time out"
    );
    crate::negative_assert_eq!(
        sends.lock().expect("#1790 sends mutex").len(),
        1,
        "#1790 the timed-out confirmation reached the sender once"
    );

    let permissions = shared.lock_permissions();
    crate::negative_assert!(
        !permissions.is_answering(&chat),
        "#1790 answering must not survive the confirmation-send timeout"
    );
    crate::negative_assert!(
        !permissions.is_in_flight(&chat),
        "#1790 in_flight must not survive the confirmation-send timeout"
    );
    crate::negative_assert!(
        !permissions.has_pending(&chat),
        "#1790 the timed-out confirmation must settle the answered permission"
    );
}

/// #1339 fix-r1 (P2 #6): a JSON `null` body throws on the TS side with the
/// socket left hanging (probes/null-body.txt — no response byte ever
/// arrives), so the oracle cannot pin it. The port reports the exact
/// TypeError through the recorder and destroys the connection; the
/// hang-vs-destroy deviation is ledgered in `A8.3-CH-SMS-007`.
#[tokio::test]
async fn json_null_body_is_unhandled_destroy() {
    const PORT: u16 = 18926;
    let record: Arc<SyncMutex<Record>> = Arc::new(SyncMutex::new(Record::default()));
    let unhandled_record = record.clone();
    let record_unhandled: UnhandledRecorder = Arc::new(move |message| {
        unhandled_record
            .lock()
            .expect("#1339 record mutex")
            .unhandled
            .push(message.to_owned());
    });
    // #2257 fix-r4 (review r4 P2a): same preflight as the http10 pins — a
    // NoopHandle fallback on a busy port would test a foreign listener.
    // #2257 fix-r5 (review r5 anthropic-concurrency P1): 18901 previously
    // sat inside the wecom-callback golden replay segment (18900–18921),
    // held only by the fixture's documented courtesy of skipping 18901 —
    // moved out to the free gap above that segment, same policy as the
    // 200/404 pins (18923/18922), the head-timeout ports (18924/18925),
    // the #2152 parser-admission pins (18927/18928) and the #2691
    // port-guard cases (18929–18932).
    // #2691: this pin's extra carries no `webhookHost`, so the effective
    // host is the `127.0.0.1` default — the fixed-host shape.
    let extra = json!({
        "accountSid": "AC1", "authToken": "tok", "phoneNumber": "+15550000",
        "webhookPort": PORT,
    });
    let host = effective_webhook_host(Some(&extra));
    ensure_replay_port_free(PORT, &host).await;
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(&extra)),
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: Some(null_factory()),
        record_unhandled: Some(record_unhandled),
        request_timeout: None,
    });
    let shared = channel.shared.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#1339 json-null test start");
    // #2691: same post-start bind proof as the http10 pins — the NoopHandle
    // fallback this test's own `closed: true` outcome could fake-pass through
    // (a foreign listener that ignores the request destroys too) must not
    // stay silent.
    drop(channel);
    assert_bound_listener(&shared, PORT, 0);
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
            crate::test_skip_line::stderr_skip_line(&format!(
                "#1339 skipped: host has no IPv6 ({error})\n"
            ));
            return;
        }
        Err(error) => panic!("#1339 cannot create an IPv6 socket to set up the busy port: {error}"),
    };
    if let Err(error) = socket.bind(std::net::SocketAddr::from(([0u16; 8], 0))) {
        if means_no_ipv6(&error) {
            crate::test_skip_line::stderr_skip_line(&format!(
                "#1339 skipped: host has no IPv6 wildcard ({error})\n"
            ));
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
            // #2691: the empty host — the one shape whose product bind is the
            // `::` wildcard the v6 half exists for.
            .block_on(ensure_replay_port_free(port, ""))
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

/// #2691 gap 1: the preflight's v6 half is calibrated for the *empty*
/// `webhookHost` — the one shape whose product bind is the dual-stack `::`
/// (`bind_listener`, mod.rs:815-845). A fixed `127.0.0.1` host never touches
/// IPv6, so a genuine v6-only occupier of `[::]:port` used to fail the probe
/// although the product's own `bind(("127.0.0.1", port))` succeeds — a false
/// red. Both halves measured (`_tmp/20261026-issue-2691/probes/`: with
/// `IPV6_V6ONLY` set on `[::]:P`, the guard's `::`-shaped bind reports
/// `EADDRINUSE` while the 127.0.0.1 bind and Node's `listen(127.0.0.1, P)`
/// both succeed). Unfixed code fails here with the guard's own IPv6 panic;
/// the `sms-port-preflight-v6-any-host` switch keeps that shape for the
/// negative control.
#[test]
fn v6_only_occupier_does_not_block_the_fixed_127_0_0_1_host() {
    // #2691 fix-r2 (review r2, three roles independently): 18927 collided
    // with the #2152 parser-admission pin `unknown_method_is_the_bare_parser_400`
    // under nextest's default parallelism — moved into the free gap, keeping
    // the ledger in `head_timeout_tests.rs` in sync (18929 here, 18930 for the
    // TOCTOU case, 18931 for the dual-stack preflight case below).
    const PORT: u16 = 18929;
    // A genuine v6-only holder of `[::]:PORT`, bound without listening (the
    // Q2 shape of `port_busy_on_ipv6_preflight_fails_fast`). tokio 1.48's
    // `TcpSocket` cannot set `IPV6_V6ONLY` and the crate forbids `unsafe`,
    // so the occupier is a socket2 socket — the crate tokio's own listener
    // is built on.
    let socket = match socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None) {
        Ok(socket) => socket,
        Err(error) if means_no_ipv6(&error) => {
            eprintln!("#2691 skipped: host has no IPv6 ({error})");
            return;
        }
        Err(error) => panic!("#2691 cannot create an IPv6 socket for the occupier: {error}"),
    };
    if let Err(error) = socket.set_only_v6(true) {
        panic!("#2691 cannot make the occupier v6-only: {error}");
    }
    let address = std::net::SocketAddr::from(([0u16; 8], PORT));
    if let Err(error) = socket.bind(&address.into()) {
        if means_no_ipv6(&error) {
            eprintln!("#2691 skipped: host has no IPv6 wildcard ({error})");
            return;
        }
        panic!("#2691 cannot bind the v6-only occupier to {address}: {error}");
    }
    // 1. The preflight must not fire its v6 half for the fixed host: the
    //    product never binds IPv6 on this shape.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#2691 preflight runtime build");
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(ensure_replay_port_free(PORT, DEFAULT_HOST))
    }));
    if let Err(payload) = panicked {
        let text = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
            })
            .unwrap_or_else(|| "#2691 non-string panic payload".to_owned());
        panic!(
            "#2691 the v6 half probed the fixed 127.0.0.1 host — the product \
             never binds IPv6 there, so this is the pre-fix false red: {text}"
        );
    }
    // 2. The product binding path runs to its own success: with the v6-only
    //    occupier still held, the channel binds 127.0.0.1:PORT, answers the
    //    request, and the post-start guard sees a live listener.
    runtime.block_on(async {
        let case = case_by_name("http10");
        let steps = recorded_steps(&case).expect("#2691 http10 steps validated array");
        let request = build_raw(
            steps
                .first()
                .expect("#2691 the http10 case carries one request step"),
        );
        let mut extra = recorded_extra(&case).cloned().unwrap_or_else(|| json!({}));
        extra["webhookPort"] = json!(PORT);
        let channel = SmsChannel::new(SmsChannelOptions {
            extra: Some(to_js_object(&extra)),
            mapper: None,
            gateway: None,
            env: None,
            twilio_factory: Some(null_factory()),
            record_unhandled: None,
            request_timeout: None,
        });
        let shared = channel.shared.clone();
        let handle = channel
            .start(ChannelStartDeps {
                config: None,
                report_status: None,
                log: None,
                turn_gateway: None,
            })
            .await
            .expect("#2691 start with the v6-only occupier held");
        drop(channel);
        assert_bound_listener(&shared, PORT, 0);
        let raw = bounded("#2691 v6-occupier pin response", async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut stream = TcpStream::connect(("127.0.0.1", PORT))
                .await
                .expect("#2691 connect to the pin's own 127.0.0.1 listener");
            stream
                .write_all(&request)
                .await
                .expect("#2691 send the http10 request");
            let mut raw = Vec::new();
            stream
                .read_to_end(&mut raw)
                .await
                .expect("#2691 read the pin response to the close");
            raw
        })
        .await;
        crate::negative_assert!(
            raw.starts_with(b"HTTP/1.0 200 OK\r\n"),
            "#2691 the pin answered through its own 127.0.0.1 listener while a \
             v6-only socket held [::]:{PORT}: {raw:?}"
        );
        settle().await;
        bounded("#2691 v6-occupier pin stop", handle.stop("cleanup"))
            .await
            .expect("#2691 v6-occupier pin cleanup stop");
    });
    drop(socket);
}

/// #2691 gap 2 (TOCTOU): the preflight is a pre-`start` probe — an occupier
/// that appears in the window between the probe and the bind is invisible to
/// it, and `start` swallows the lost bind into the no-op handle
/// (mod.rs:606-628), so a pin would silently measure the foreign listener.
/// This test manufactures that window deterministically and proves the
/// post-start guard catches it (the same shape `head_timeout_tests.rs`
/// guards its own module with, #2020). The `sms-pin-bind-race-unchecked`
/// switch disarms the guard; this test is then the one that must fail.
#[tokio::test]
async fn a_pin_start_that_loses_the_bind_race_is_caught_after_start() {
    // #2691 fix-r2 (review r2, three roles independently): 18928 collided
    // with the #2152 parser-admission pin
    // `missing_host_is_the_headless_parser_400` — and this case deliberately
    // holds the port, so the collision was a guaranteed cross-cell break
    // under nextest's default parallelism, not a scheduling race. Moved to
    // 18930; the ledger in `head_timeout_tests.rs` stays in sync.
    const PORT: u16 = 18930;
    let extra = json!({
        "accountSid": "AC1", "authToken": "tok", "phoneNumber": "+15550000",
        "webhookPort": PORT,
    });
    let host = effective_webhook_host(Some(&extra));
    // 1. The preflight passes: the port is free.
    ensure_replay_port_free(PORT, &host).await;
    // 2. The occupier appears inside the window the probe cannot see.
    let occupier = std::net::TcpListener::bind(("127.0.0.1", PORT))
        .expect("#2691 occupier bind between the preflight and the start");
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(&extra)),
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: Some(null_factory()),
        record_unhandled: None,
        request_timeout: None,
    });
    let shared = channel.shared.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#2691 a lost bind is still a started channel");
    drop(channel);
    // 3. The post-start guard must fire: the no-op handle owns no SmsShared.
    //    #2691 fix-r3: the drop form (`0` extra owners) — the non-drop form
    //    `replay_named` needs is pinned by its own cell below.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_bound_listener(&shared, PORT, 0)
    }));
    match result {
        Err(payload) => {
            let text = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&str>()
                        .map(|text| (*text).to_owned())
                })
                .unwrap_or_else(|| "#2691 non-string panic payload".to_owned());
            crate::negative_assert!(
                text.contains("#2691 start() on port"),
                "#2691 the post-start bind guard fired with an unexpected message: {text}"
            );
        }
        Ok(()) => panic!(
            "#2691 the post-start bind guard did not fire although the no-op \
             handle owns no SmsShared — the pre-fix TOCTOU shape would \
             silently measure the occupier's listener"
        ),
    }
    // Production hands the no-op handle to the supervisor; it must stop.
    bounded("#2691 TOCTOU cleanup stop", handle.stop("cleanup"))
        .await
        .expect("#2691 stop the no-op handle");
    drop(occupier);
    settle().await;
}

/// #2691 fix-r3 (review r3 concurrency#1 / boundary#1 / MM boundary#1, three
/// roles independently): `replay_named`'s post-start guard is the *non-drop*
/// form — the channel stays alive because the `cron` steps below the guard
/// call `channel.deliver_cron_result` — so at the guard a lost bind already
/// counts channel + this clone = 2 owners and the drop-form `>= 2` fix-r2
/// wrote was tautological there (the TOCTOU stayed open on every oracle cell
/// that expects a live listener). This cell reproduces exactly that
/// ownership shape — no `drop(channel)` before the guard, the same
/// [`OWNERS_KEPT_ALIVE_ACROSS_START`] the replay driver passes — and proves
/// the guard still fires. Weaken the threshold back to the drop form, or
/// drop the constant to 0, and this test turns red with its own marker; the
/// `sms-pin-bind-race-unchecked` switch disarms the guard for both forms.
#[tokio::test]
async fn a_replay_start_that_keeps_the_channel_alive_is_still_caught_after_start() {
    // #2691 fix-r3: 18932 — the fourth #2691 port-guard slot; see the ledger
    // in `head_timeout_tests.rs`.
    const PORT: u16 = 18932;
    let extra = json!({
        "accountSid": "AC1", "authToken": "tok", "phoneNumber": "+15550000",
        "webhookPort": PORT,
    });
    let host = effective_webhook_host(Some(&extra));
    // 1. The preflight passes: the port is free.
    ensure_replay_port_free(PORT, &host).await;
    // 2. The occupier appears inside the window the probe cannot see.
    let occupier = std::net::TcpListener::bind(("127.0.0.1", PORT))
        .expect("#2691 replay-form occupier bind between the preflight and the start");
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: Some(to_js_object(&extra)),
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: Some(null_factory()),
        record_unhandled: None,
        request_timeout: None,
    });
    // 3. `replay_named`'s shape: the channel is NOT dropped — its `cron`
    //    steps would still call `channel.deliver_cron_result`.
    let shared = channel.shared.clone();
    let handle = channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#2691 a lost bind is still a started channel (replay form)");
    // 4. The post-start guard must fire anyway: the no-op shape counts
    //    channel + clone = 2, below the 2 + OWNERS_KEPT_ALIVE_ACROSS_START
    //    threshold.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_bound_listener(&shared, PORT, OWNERS_KEPT_ALIVE_ACROSS_START)
    }));
    match result {
        Err(payload) => {
            let text = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&str>()
                        .map(|text| (*text).to_owned())
                })
                .unwrap_or_else(|| "#2691 non-string panic payload".to_owned());
            crate::negative_assert!(
                text.contains("#2691 start() on port"),
                "#2691 the replay-form bind guard fired with an unexpected message: {text}"
            );
        }
        Ok(()) => panic!(
            "#2691 the replay-form post-start bind guard did not fire: with the \
             channel kept alive the no-op shape still counts 2 owners, so a \
             threshold written for the drop form (`>= 2`) is tautological here — \
             the pre-fix TOCTOU shape would silently measure the occupier's \
             listener through every oracle cell that expects a live listener"
        ),
    }
    // Production hands the no-op handle to the supervisor; it must stop.
    bounded(
        "#2691 replay-form TOCTOU cleanup stop",
        handle.stop("cleanup"),
    )
    .await
    .expect("#2691 stop the replay-form no-op handle");
    drop(occupier);
    drop(channel);
    settle().await;
}

/// #2691 fix-r2 (review r2 concurrency#2, P1): parameterizing the preflight
/// by the effective host dropped more than the false red — on a host where a
/// dual-stack `[::]:port` holder makes the product's own fixed-host bind
/// fail (measured on this Linux host,
/// `_tmp/20261026-issue-2691/fix-r2/probes/bind-probe-errno.log`: the v4
/// connect probe is refused while the product-shaped bind reports
/// `EADDRINUSE`), nothing reported the loss: the v6 half is correctly scoped
/// to the empty host and the connect half cannot see an occupier that never
/// listens. The bind probe in [`ensure_replay_port_free`] restores the only
/// question that matters — would the product's own `bind((host, port))`
/// fail — and this case pins it from the failing side: the
/// `sms-port-preflight-bind-probe-skipped` switch restores the pre-fix
/// shape and must turn this test red.
///
/// #2691 fix-r3 (review r3 MM boundary#2/#3, DS concurrency#2 / boundary#3):
/// the kill no longer leans on Linux-only kernel semantics. With the bind
/// probe moved ahead of the connect probe, the unmutated run reports the
/// holder on both platforms — Linux `EADDRINUSE` errno 98 (fix-r2 probe
/// log) and macOS `EADDRINUSE` errno 48 (the coordinator's measured macOS
/// baseline, `_tmp/20261110-pr2760-harvest/macos-dualstack-probe.log`;
/// re-verification on the coordinator's host is recorded in the fix-r3
/// RESULT). Under the switch the probe is skipped, and what the run meets
/// next is the connect probe: on Linux it is refused at once (the Ok
/// branch below then proves the product-shaped bind still fails); on macOS
/// it stalls on the dual-stack holder for its whole `bounded` cap — a shape
/// only the skipped probe could classify — and the stall is attributed to
/// the switch with this test's own marker instead of being read as an
/// unrelated timeout.
///
/// The not-caught branch is the equivalence proof, not an unconditional
/// catch: the std probe and the product's tokio bind share the same option
/// shape on Unix (mio sets `SO_REUSEADDR`), so a kernel that lets the
/// specific-address bind through over the wildcard holder lets the
/// product's own bind through too — the branch proves the product-shaped
/// bind really succeeds instead of assuming it.
#[test]
fn dual_stack_occupier_is_reported_by_the_fixed_host_bind_probe() {
    // #2691 fix-r2: 18931 — the third #2691 port-guard slot; see the ledger
    // in `head_timeout_tests.rs`.
    const PORT: u16 = 18931;
    // A dual-stack holder of `[::]:PORT`, bound without listening: the exact
    // occupier of the review's Q2 — it covers the IPv4 port, so the
    // product's fixed-host bind collides, while the v4 connect probe is
    // refused and sees nothing. socket2 shapes it (`set_only_v6(false)`
    // forces the dual-stack semantics instead of trusting the host's
    // `bindv6only` default).
    let socket = match socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None) {
        Ok(socket) => socket,
        Err(error) if means_no_ipv6(&error) => {
            eprintln!("#2691 skipped: host has no IPv6 ({error})");
            return;
        }
        Err(error) => panic!("#2691 cannot create an IPv6 socket for the occupier: {error}"),
    };
    if let Err(error) = socket.set_only_v6(false) {
        panic!("#2691 cannot make the occupier dual-stack: {error}");
    }
    let address = std::net::SocketAddr::from(([0u16; 8], PORT));
    if let Err(error) = socket.bind(&address.into()) {
        if means_no_ipv6(&error) {
            eprintln!("#2691 skipped: host has no IPv6 wildcard ({error})");
            return;
        }
        panic!("#2691 cannot bind the dual-stack occupier to {address}: {error}");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#2691 preflight runtime build");
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(ensure_replay_port_free(PORT, DEFAULT_HOST))
    }));
    match panicked {
        Err(payload) => {
            let text = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&str>()
                        .map(|text| (*text).to_owned())
                })
                .unwrap_or_else(|| "#2691 non-string panic payload".to_owned());
            if text.contains("#1339 wait timeout: port guard 127.0.0.1") {
                // #2691 fix-r3: the v4 connect probe stalled on the
                // dual-stack holder — the macOS shape (measured on the
                // coordinator's macOS host: the SYN is swallowed for the
                // whole `bounded` cap). Only the bind probe classifies this
                // holder, and fix-r3 moved it ahead of the connect probe
                // precisely so it is reachable; surviving to the connect
                // stall means the probe did not run — the switch.
                panic!(
                    "#2691 the fixed-host bind probe did not fire: the v4 connect \
                     probe alone cannot classify the dual-stack holder ({text})"
                );
            }
            crate::negative_assert!(
                text.contains(&format!("#1339 port {PORT} is already bound on")),
                "#2691 the fixed-host bind probe fired with an unexpected message: {text}"
            );
        }
        Ok(()) => {
            // The kernel let the product-shaped bind through over the
            // wildcard holder (a hypothetical shape: both measured hosts —
            // Linux errno 98, macOS errno 48 — report `EADDRINUSE`). Prove
            // the product's own bind really succeeds there — the same std
            // shape the probe uses — or the preflight silently lost the
            // occupier.
            match std::net::TcpListener::bind((DEFAULT_HOST, PORT)) {
                Ok(listener) => drop(listener),
                Err(error) => panic!(
                    "#2691 the fixed-host bind probe did not fire although the \
                     dual-stack [::]:{PORT} holder blocks the product's own bind: {error}"
                ),
            }
        }
    }
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
        request_timeout: None,
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
        let mut permissions = channel.shared.lock_permissions();
        let prompt = permissions.capture(&chat, &session, &event);
        crate::negative_assert!(
            prompt.is_some(),
            "#1589 permission lock split preset must deliver a prompt"
        );
        permissions.confirm_initial_prompt(&chat, true, Some(&JsString::from("req-1589")));
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
        channel.shared.lock_permissions().clear(&chat);
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
