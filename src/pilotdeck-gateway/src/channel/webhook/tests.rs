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
    BoxFuture, ChannelAdapter, ChannelError, ChannelKey, ChannelStartDeps, TurnEventStream,
    TurnGateway, TurnRequest,
};
use crate::user_dialog::NODE_408_RESPONSE;
use pilotdeck_protocol::JsValue;
/// One event, or the iterator rejection the TS channel catches per turn.
type TurnEvent = Result<JsValue, ChannelError>;

/// Adapts the mock channels' `mpsc` receivers to #1333's `TurnEventStream`
/// (`crate::channel::protocol::TurnEventStream`).
struct MockStream(mpsc::UnboundedReceiver<TurnEvent>);
impl TurnEventStream for MockStream {
    fn next(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>> {
        Box::pin(async move {
            match self.0.recv().await {
                Some(Ok(event)) => Ok(Some(event)),
                Some(Err(error)) => Err(error),
                None => Ok(None),
            }
        })
    }
}
use crate::{negative_assert, negative_assert_eq, test_baseline};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
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
            Err(format!("#1334 {field} must be {kind}, got <missing>"))
        }
        None => Ok(None),
        Some(value) => match parse(value) {
            Some(parsed) => Ok(Some(parsed)),
            None if fail_open => Ok(None),
            None => Err(format!("#1334 {field} must be {kind}, got {value}")),
        },
    }
}

fn recorded_method_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("method"),
        "method",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-method"),
    )
}

fn recorded_method(value: &Value) -> Option<&str> {
    recorded_method_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_method_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_method_checked(&json!({"method": []})).is_err(),
        "#1334 #1729 rejects method type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 method must be a string")]
fn recorded_method_panics_on_wrong_type() {
    recorded_method(&json!({"method": []}));
}

fn recorded_target_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("target"),
        "target",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-target"),
    )
}

fn recorded_target(value: &Value) -> Option<&str> {
    recorded_target_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_target_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_target_checked(&json!({"target": []})).is_err(),
        "#1334 #1729 rejects target type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 target must be a string")]
fn recorded_target_panics_on_wrong_type() {
    recorded_target(&json!({"target": []}));
}

fn recorded_headers_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        value.get("headers"),
        "headers",
        "an object",
        |value| value.is_object().then_some(value),
        false,
        crate::mutants::on("webhook-oracle-recorded-headers"),
    )
}

fn recorded_headers(value: &Value) -> Option<&Value> {
    recorded_headers_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_headers_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_headers_checked(&json!({"headers": []})).is_err(),
        "#1334 #1729 rejects headers type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 headers must be an object")]
fn recorded_headers_panics_on_wrong_type() {
    recorded_headers(&json!({"headers": []}));
}

fn recorded_body_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("body"),
        "body",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-body"),
    )
}

fn recorded_body(value: &Value) -> Option<&str> {
    recorded_body_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_body_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_body_checked(&json!({"body": []})).is_err(),
        "#1334 #1729 rejects body type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 body must be a string")]
fn recorded_body_panics_on_wrong_type() {
    recorded_body(&json!({"body": []}));
}

fn recorded_mode_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("mode"),
        "mode",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-mode"),
    )
}

fn recorded_mode(value: &Value) -> Option<&str> {
    recorded_mode_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_mode_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_mode_checked(&json!({"mode": []})).is_err(),
        "#1334 #1729 rejects mode type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 mode must be a string")]
fn recorded_mode_panics_on_wrong_type() {
    recorded_mode(&json!({"mode": []}));
}

/// #1684: the recorded request version (`steps[].version`, absent means
/// HTTP/1.1). The golden `http10` case is the only recording that sets it.
fn recorded_version_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("version"),
        "version",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-version"),
    )
}

fn recorded_version(value: &Value) -> Option<&str> {
    recorded_version_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_version_rejects_wrong_type() {
    negative_assert!(
        recorded_version_checked(&json!({"version": []})).is_err(),
        "#1334 #1729 rejects version type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 version must be a string")]
fn recorded_version_panics_on_wrong_type() {
    recorded_version(&json!({"version": []}));
}

fn recorded_count_checked(value: &Value) -> Result<Option<u64>, String> {
    recorded_checked(
        value.get("count"),
        "count",
        "a u64",
        Value::as_u64,
        false,
        crate::mutants::on("webhook-oracle-recorded-count"),
    )
}

fn recorded_count(value: &Value) -> Option<u64> {
    recorded_count_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_count_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_count_checked(&json!({"count": []})).is_err(),
        "#1334 #1729 rejects count type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 count must be a u64")]
fn recorded_count_panics_on_wrong_type() {
    recorded_count(&json!({"count": []}));
}

fn recorded_pin_checked(value: &Value) -> Result<Option<bool>, String> {
    recorded_checked(
        value.get("pin"),
        "pin",
        "a boolean",
        Value::as_bool,
        false,
        crate::mutants::on("webhook-oracle-recorded-pin"),
    )
}

fn recorded_pin(value: &Value) -> Option<bool> {
    recorded_pin_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_pin_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_pin_checked(&json!({"pin": []})).is_err(),
        "#1334 #1729 rejects pin type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 pin must be a boolean")]
fn recorded_pin_panics_on_wrong_type() {
    recorded_pin(&json!({"pin": []}));
}

fn recorded_cron_session_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("sessionKey"),
        "cron.sessionKey",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-cron-session"),
    )
}

fn recorded_cron_session(value: &Value) -> Option<&str> {
    recorded_cron_session_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_cron_session_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_cron_session_checked(&json!({"sessionKey": []})).is_err(),
        "#1334 #1729 rejects cron.sessionKey type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 cron.sessionKey must be a string")]
fn recorded_cron_session_panics_on_wrong_type() {
    recorded_cron_session(&json!({"sessionKey": []}));
}

fn recorded_cron_text_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("text"),
        "cron.text",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-cron-text"),
    )
}

fn recorded_cron_text(value: &Value) -> Option<&str> {
    recorded_cron_text_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_cron_text_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_cron_text_checked(&json!({"text": []})).is_err(),
        "#1334 #1729 rejects cron.text type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 cron.text must be a string")]
fn recorded_cron_text_panics_on_wrong_type() {
    recorded_cron_text(&json!({"text": []}));
}

fn recorded_cron_origin_session_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("originSessionKey"),
        "cron.originSessionKey",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-cron-origin-session"),
    )
}

fn recorded_cron_origin_session(value: &Value) -> Option<&str> {
    recorded_cron_origin_session_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_cron_origin_session_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_cron_origin_session_checked(&json!({"originSessionKey": []})).is_err(),
        "#1334 #1729 rejects cron.originSessionKey type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 cron.originSessionKey must be a string")]
fn recorded_cron_origin_session_panics_on_wrong_type() {
    recorded_cron_origin_session(&json!({"originSessionKey": []}));
}

fn recorded_cron_origin_channel_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("originChannelKey"),
        "cron.originChannelKey",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-cron-origin-channel"),
    )
}

fn recorded_cron_origin_channel(value: &Value) -> Option<&str> {
    recorded_cron_origin_channel_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_cron_origin_channel_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_cron_origin_channel_checked(&json!({"originChannelKey": []})).is_err(),
        "#1334 #1729 rejects cron.originChannelKey type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 cron.originChannelKey must be a string")]
fn recorded_cron_origin_channel_panics_on_wrong_type() {
    recorded_cron_origin_channel(&json!({"originChannelKey": []}));
}

fn recorded_options_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        value.get("options"),
        "options",
        "an object",
        |value| value.is_object().then_some(value),
        false,
        crate::mutants::on("webhook-oracle-recorded-options"),
    )
}

fn recorded_options(value: &Value) -> Option<&Value> {
    recorded_options_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_options_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_options_checked(&json!({"options": []})).is_err(),
        "#1334 #1729 rejects options type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 options must be an object")]
fn recorded_options_panics_on_wrong_type() {
    recorded_options(&json!({"options": []}));
}

fn recorded_port_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        value.get("port"),
        "options.port",
        "a number or <busy-port>",
        |value| (value.is_number() || value.as_str() == Some("<busy-port>")).then_some(value),
        false,
        crate::mutants::on("webhook-oracle-recorded-port"),
    )
}

fn recorded_port(value: &Value) -> Option<&Value> {
    recorded_port_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_port_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_port_checked(&json!({"port": []})).is_err(),
        "#1334 #1729 rejects options.port type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 options.port must be a number or <busy-port>")]
fn recorded_port_panics_on_wrong_type() {
    recorded_port(&json!({"port": []}));
}

fn recorded_routes_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        value.get("routes"),
        "options.routes",
        "an object",
        |value| value.is_object().then_some(value),
        true,
        crate::mutants::on("webhook-oracle-recorded-routes"),
    )
}

fn recorded_routes(value: &Value) -> Option<&Value> {
    recorded_routes_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_routes_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_routes_checked(&json!({"routes": []})).is_err(),
        "#1334 #1729 rejects options.routes type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 options.routes must be an object")]
fn recorded_routes_panics_on_wrong_type() {
    recorded_routes(&json!({"routes": []}));
}

fn recorded_secret_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("secret"),
        "options.secret",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-secret"),
    )
}

fn recorded_secret(value: &Value) -> Option<&str> {
    recorded_secret_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_secret_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_secret_checked(&json!({"secret": []})).is_err(),
        "#1334 #1729 rejects options.secret type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 options.secret must be a string")]
fn recorded_secret_panics_on_wrong_type() {
    recorded_secret(&json!({"secret": []}));
}

fn recorded_rate_limit_checked(value: &Value) -> Result<Option<f64>, String> {
    recorded_checked(
        value.get("rateLimit"),
        "options.rateLimit",
        "a number",
        Value::as_f64,
        false,
        crate::mutants::on("webhook-oracle-recorded-rate-limit"),
    )
}

fn recorded_rate_limit(value: &Value) -> Option<f64> {
    recorded_rate_limit_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_rate_limit_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_rate_limit_checked(&json!({"rateLimit": []})).is_err(),
        "#1334 #1729 rejects options.rateLimit type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 options.rateLimit must be a number")]
fn recorded_rate_limit_panics_on_wrong_type() {
    recorded_rate_limit(&json!({"rateLimit": []}));
}

fn recorded_max_body_checked(value: &Value) -> Result<Option<f64>, String> {
    recorded_checked(
        value.get("maxBodyBytes"),
        "options.maxBodyBytes",
        "a number",
        Value::as_f64,
        false,
        crate::mutants::on("webhook-oracle-recorded-max-body"),
    )
}

fn recorded_max_body(value: &Value) -> Option<f64> {
    recorded_max_body_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_max_body_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_max_body_checked(&json!({"maxBodyBytes": []})).is_err(),
        "#1334 #1729 rejects options.maxBodyBytes type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 options.maxBodyBytes must be a number")]
fn recorded_max_body_panics_on_wrong_type() {
    recorded_max_body(&json!({"maxBodyBytes": []}));
}

fn recorded_mapper_uuid_checked(value: &Value) -> Result<Option<bool>, String> {
    recorded_checked(
        value.get("mapperFixedUuid"),
        "options.mapperFixedUuid",
        "a boolean",
        Value::as_bool,
        false,
        crate::mutants::on("webhook-oracle-recorded-mapper-uuid"),
    )
}

fn recorded_mapper_uuid(value: &Value) -> Option<bool> {
    recorded_mapper_uuid_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_mapper_uuid_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_mapper_uuid_checked(&json!({"mapperFixedUuid": []})).is_err(),
        "#1334 #1729 rejects options.mapperFixedUuid type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 options.mapperFixedUuid must be a boolean")]
fn recorded_mapper_uuid_panics_on_wrong_type() {
    recorded_mapper_uuid(&json!({"mapperFixedUuid": []}));
}

fn recorded_scripts_checked(value: &Value) -> Result<Option<&Vec<Value>>, String> {
    recorded_checked(
        value.get("scripts"),
        "scripts",
        "an array",
        Value::as_array,
        false,
        crate::mutants::on("webhook-oracle-recorded-scripts"),
    )
}

fn recorded_scripts(value: &Value) -> Option<&Vec<Value>> {
    recorded_scripts_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_scripts_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_scripts_checked(&json!({"scripts": false})).is_err(),
        "#1334 #1729 rejects scripts type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 scripts must be an array")]
fn recorded_scripts_panics_on_wrong_type() {
    recorded_scripts(&json!({"scripts": false}));
}

fn recorded_steps_checked(value: &Value) -> Result<Option<&Vec<Value>>, String> {
    // #1729 fix-r5: `steps` is a required recording field. An absent key must
    // fail instead of silently becoming a zero-step replay. An *empty* array
    // stays legal: `port-in-use` / `invalid-port` are recorded with zero steps
    // on purpose (the case is driven by options alone), which is why the empty
    // check the sms harness has is not mirrored here.
    recorded_checked(
        value.get("steps"),
        "steps",
        "an array",
        Value::as_array,
        true,
        crate::mutants::on("webhook-oracle-recorded-steps"),
    )
}

fn recorded_steps(value: &Value) -> Option<&Vec<Value>> {
    recorded_steps_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_steps_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_steps_checked(&json!({"steps": false})).is_err(),
        "#1334 #1729 rejects steps type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 steps must be an array")]
fn recorded_steps_panics_on_wrong_type() {
    recorded_steps(&json!({"steps": false}));
}

#[test]
#[should_panic(expected = "#1334 steps must be an array, got <missing>")]
fn recorded_steps_panics_when_absent() {
    recorded_steps(&json!({}));
}

fn recorded_op_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("op"),
        "op",
        "a string",
        Value::as_str,
        true,
        crate::mutants::on("webhook-oracle-recorded-op"),
    )
}

fn recorded_op(value: &Value) -> Option<&str> {
    recorded_op_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_op_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_op_checked(&json!({"op": []})).is_err(),
        "#1334 #1729 rejects op type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 op must be a string")]
fn recorded_op_panics_on_wrong_type() {
    recorded_op(&json!({"op": []}));
}

fn recorded_stop_reason_checked(value: &Value) -> Result<Option<&str>, String> {
    recorded_checked(
        value.get("reason"),
        "stop.reason",
        "a string",
        Value::as_str,
        false,
        crate::mutants::on("webhook-oracle-recorded-stop-reason"),
    )
}

fn recorded_stop_reason(value: &Value) -> Option<&str> {
    recorded_stop_reason_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_stop_reason_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_stop_reason_checked(&json!({"reason": []})).is_err(),
        "#1334 #1729 rejects stop.reason type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 stop.reason must be a string")]
fn recorded_stop_reason_panics_on_wrong_type() {
    recorded_stop_reason(&json!({"reason": []}));
}

/// The generator's gateway mock: every `submitTurn` records its input and
/// replays one scripted event list; the string `"<throw>"` rejects the
/// iterator with `Error: gateway exploded`.
struct MockGateway {
    calls: Arc<Mutex<Vec<Value>>>,
    scripts: Mutex<VecDeque<Vec<Value>>>,
}

impl MockGateway {
    fn from_scripts_checked(scripts: &[Value]) -> Result<Self, String> {
        let scripts = scripts
            .iter()
            .enumerate()
            .map(|(index, value)| {
                recorded_checked(
                    Some(value),
                    &format!("scripts[{index}]"),
                    "an array",
                    Value::as_array,
                    false,
                    crate::mutants::on("webhook-oracle-script-entry"),
                )
                .map(|events| events.cloned().unwrap_or_default())
            })
            .collect::<Result<VecDeque<_>, _>>()?;
        Ok(Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            scripts: Mutex::new(scripts),
        })
    }

    fn from_scripts(scripts: &[Value]) -> Self {
        // Validate before a detached submit task can hide a malformed script.
        Self::from_scripts_checked(scripts).unwrap_or_else(|error| panic!("{error}"))
    }

    fn calls_snapshot(&self) -> Vec<Value> {
        self.calls.lock().expect("#1334 calls mutex").clone()
    }
}

impl TurnGateway for MockGateway {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.calls.lock().expect("#1334 calls mutex").push(json!({
            "sessionKey": String::from_utf16_lossy(request.session_key.units()),
            "channelKey": request.channel_key.to_string(),
            "message": String::from_utf16_lossy(request.message.units()),
        }));
        let script = self
            .scripts
            .lock()
            .expect("#1334 scripts mutex")
            .pop_front()
            .unwrap_or_default();
        let (sender, receiver) = mpsc::unbounded_channel();
        for step in script {
            if step.as_str() == Some("<throw>") {
                let _ = sender.send(Err(ChannelError::error("gateway exploded")));
            } else {
                let event =
                    parse_json(&step.to_string()).expect("#1334 scripted gateway event JSON");
                let _ = sender.send(Ok(event));
            }
        }
        Box::pin(async move { Ok(Box::new(MockStream(receiver)) as Box<dyn TurnEventStream>) })
    }
}

/// A gateway whose event streams never end: the turn under test stays in
/// flight until the test aborts it.
struct HeldGateway {
    calls: Arc<Mutex<Vec<String>>>,
    held: Mutex<Vec<mpsc::UnboundedSender<TurnEvent>>>,
}

impl TurnGateway for HeldGateway {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.calls
            .lock()
            .expect("#1334 held gateway calls mutex")
            .push(String::from_utf16_lossy(request.session_key.units()));
        let (sender, receiver) = mpsc::unbounded_channel();
        self.held
            .lock()
            .expect("#1334 held gateway mutex")
            .push(sender);
        Box::pin(async move { Ok(Box::new(MockStream(receiver)) as Box<dyn TurnEventStream>) })
    }
}

/// A gateway that records the turn and withholds its single reply event
/// behind a watch gate, so a stop can race a genuinely in-flight turn.
struct GatedGateway {
    calls: Arc<Mutex<Vec<String>>>,
    releases: Mutex<Vec<watch::Sender<bool>>>,
}

impl TurnGateway for GatedGateway {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.calls
            .lock()
            .expect("#1334 gated calls mutex")
            .push(String::from_utf16_lossy(request.session_key.units()));
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
        Box::pin(async move { Ok(Box::new(MockStream(receiver)) as Box<dyn TurnEventStream>) })
    }
}

/// A gateway whose `submit` rejects before producing any stream: the
/// submit-failure arm of `process_message` that the three replay mocks never
/// reach (`"<throw>"` fails the stream iteration, not the submit). #1614.
struct FailingGateway {
    calls: Arc<Mutex<Vec<String>>>,
}

impl TurnGateway for FailingGateway {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.calls
            .lock()
            .expect("#1614 failing gateway calls mutex")
            .push(String::from_utf16_lossy(request.session_key.units()));
        Box::pin(async move { Err(ChannelError::error("submit rejected")) })
    }
}

/// A gateway whose first `submit` panics where the channel awaits it: the
/// #1738 shape — the fire-and-forget task dies mid-`processMessage`, which a
/// paired insert/remove cannot survive. Later submits return an empty stream
/// so the redelivered turn completes.
struct PanickingGateway {
    calls: Arc<Mutex<Vec<String>>>,
}

impl TurnGateway for PanickingGateway {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        let calls = self.calls.clone();
        Box::pin(async move {
            let first = {
                let mut calls = calls.lock().expect("#1738 panicking calls mutex");
                calls.push(String::from_utf16_lossy(request.session_key.units()));
                calls.len() == 1
            };
            if first {
                panic!("#1738 injected process_message panic (webhook)");
            }
            let (sender, receiver) = mpsc::unbounded_channel();
            drop(sender);
            Ok(Box::new(MockStream(receiver)) as Box<dyn TurnEventStream>)
        })
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
fn install(channel: &WebhookChannel, gateway: Option<Arc<dyn TurnGateway>>, logger: ChannelLog) {
    let mut state = channel.ctx.state.lock().expect("#1334 webhook state mutex");
    state.gateway = gateway;
    state.logger = Some(logger);
}

/// The recorder's `requestBytes`: Host first, the step's headers in order,
/// then `Connection: close` for close mode, then the body's byte length.
/// #1684: the request line's version comes from `steps[].version` (absent
/// means HTTP/1.1), so the golden `http10` cell sends a real HTTP/1.0 request.
fn request_bytes_checked(step: &Value) -> Result<String, String> {
    let mut lines = vec![format!(
        "{} {} {}",
        recorded_method_checked(step)?.unwrap_or_default(),
        recorded_target_checked(step)?.unwrap_or_default(),
        recorded_version_checked(step)?.unwrap_or("HTTP/1.1")
    )];
    lines.push("Host: 127.0.0.1".to_owned());
    let headers = recorded_headers_checked(step)?.and_then(Value::as_object);
    let has = |name: &str| headers.is_some_and(|map| map.contains_key(name));
    if let Some(map) = headers {
        for (name, value) in map {
            let text = recorded_checked(
                Some(value),
                &format!("headers.{name}"),
                "a string",
                Value::as_str,
                false,
                crate::mutants::on("webhook-oracle-header-value"),
            )?
            .unwrap_or_default();
            lines.push(format!("{name}: {text}"));
        }
    }
    let mode = recorded_mode_checked(step)?.unwrap_or("close");
    if mode == "close" && !has("Connection") {
        lines.push("Connection: close".to_owned());
    }
    let body = recorded_body_checked(step)?;
    if let Some(body) = body.filter(|_| !has("Content-Length")) {
        lines.push(format!("Content-Length: {}", body.len()));
    }
    Ok(format!(
        "{}\r\n\r\n{}",
        lines.join("\r\n"),
        body.unwrap_or_default()
    ))
}

fn request_bytes(step: &Value) -> String {
    request_bytes_checked(step).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_header_value_rejects_drift() {
    negative_assert!(
        request_bytes_checked(&json!({"headers":{"x-test":7}})).is_err(),
        "#1334 #1729 rejects header value type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 headers.x-test must be a string")]
fn recorded_header_value_panics_on_drift() {
    request_bytes(&json!({"headers":{"x-test":7}}));
}

#[test]
fn recorded_script_entry_rejects_drift() {
    negative_assert!(
        MockGateway::from_scripts_checked(&[Value::Null]).is_err(),
        "#1334 #1729 rejects script entry type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 scripts[0] must be an array")]
fn recorded_script_entry_panics_on_drift() {
    MockGateway::from_scripts(&[Value::Null]);
}

#[test]
fn absent_recording_fields_keep_defaults() {
    let empty = json!({});
    assert_eq!(
        request_bytes(&empty),
        "  HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        "#1334 absent request fields"
    );
    let delivery = delivery_from_step(&empty);
    assert!(
        delivery.session_key.equals_str(""),
        "#1334 absent cron session"
    );
    assert!(
        delivery.text.equals_str("cron text"),
        "#1334 absent cron text"
    );
    assert!(
        delivery.origin_session_key.is_none() && delivery.origin_channel_key.is_none(),
        "#1334 absent cron origins"
    );
    assert!(
        MockGateway::from_scripts_checked(&[]).is_ok(),
        "#1334 absent scripts"
    );
    assert!(
        MockGateway::from_scripts_checked(&[json!([])]).is_ok(),
        "#1334 empty script is legal"
    );
    assert!(recorded_method(&empty).is_none(), "#1334 absent method");
    assert!(
        recorded_method(&json!({"method":"value"})).is_some(),
        "#1334 valid method"
    );
    assert!(recorded_target(&empty).is_none(), "#1334 absent target");
    assert!(
        recorded_target(&json!({"target":"value"})).is_some(),
        "#1334 valid target"
    );
    assert!(recorded_headers(&empty).is_none(), "#1334 absent headers");
    assert!(
        recorded_headers(&json!({"headers":{}})).is_some(),
        "#1334 valid headers"
    );
    assert!(recorded_body(&empty).is_none(), "#1334 absent body");
    assert!(
        recorded_body(&json!({"body":"value"})).is_some(),
        "#1334 valid body"
    );
    assert!(recorded_mode(&empty).is_none(), "#1334 absent mode");
    assert!(
        recorded_mode(&json!({"mode":"value"})).is_some(),
        "#1334 valid mode"
    );
    assert!(recorded_version(&empty).is_none(), "#1334 absent version");
    assert!(
        recorded_version(&json!({"version":"HTTP/1.0"})).is_some(),
        "#1334 valid version"
    );
    assert!(recorded_count(&empty).is_none(), "#1334 absent count");
    assert!(
        recorded_count(&json!({"count":1})).is_some(),
        "#1334 valid count"
    );
    assert!(recorded_pin(&empty).is_none(), "#1334 absent pin");
    assert!(
        recorded_pin(&json!({"pin":true})).is_some(),
        "#1334 valid pin"
    );
    assert!(
        recorded_cron_session(&empty).is_none(),
        "#1334 absent cron.sessionKey"
    );
    assert!(
        recorded_cron_session(&json!({"sessionKey":"value"})).is_some(),
        "#1334 valid cron.sessionKey"
    );
    assert!(
        recorded_cron_text(&empty).is_none(),
        "#1334 absent cron.text"
    );
    assert!(
        recorded_cron_text(&json!({"text":"value"})).is_some(),
        "#1334 valid cron.text"
    );
    assert!(
        recorded_cron_origin_session(&empty).is_none(),
        "#1334 absent cron.originSessionKey"
    );
    assert!(
        recorded_cron_origin_session(&json!({"originSessionKey":"value"})).is_some(),
        "#1334 valid cron.originSessionKey"
    );
    assert!(
        recorded_cron_origin_channel(&empty).is_none(),
        "#1334 absent cron.originChannelKey"
    );
    assert!(
        recorded_cron_origin_channel(&json!({"originChannelKey":"value"})).is_some(),
        "#1334 valid cron.originChannelKey"
    );
    assert!(recorded_options(&empty).is_none(), "#1334 absent options");
    assert!(
        recorded_options(&json!({"options":{}})).is_some(),
        "#1334 valid options"
    );
    assert!(recorded_port(&empty).is_none(), "#1334 absent options.port");
    assert!(
        recorded_port(&json!({"port":0})).is_some(),
        "#1334 valid options.port"
    );
    assert!(
        recorded_routes_checked(&empty).is_err(),
        "#1334 missing options.routes"
    );
    assert!(
        recorded_routes(&json!({"routes":{}})).is_some(),
        "#1334 valid options.routes"
    );
    assert!(
        recorded_secret(&empty).is_none(),
        "#1334 absent options.secret"
    );
    assert!(
        recorded_secret(&json!({"secret":"value"})).is_some(),
        "#1334 valid options.secret"
    );
    assert!(
        recorded_rate_limit(&empty).is_none(),
        "#1334 absent options.rateLimit"
    );
    assert!(
        recorded_rate_limit(&json!({"rateLimit":1.5})).is_some(),
        "#1334 valid options.rateLimit"
    );
    assert!(
        recorded_max_body(&empty).is_none(),
        "#1334 absent options.maxBodyBytes"
    );
    assert!(
        recorded_max_body(&json!({"maxBodyBytes":1.5})).is_some(),
        "#1334 valid options.maxBodyBytes"
    );
    assert!(
        recorded_mapper_uuid(&empty).is_none(),
        "#1334 absent options.mapperFixedUuid"
    );
    assert!(
        recorded_mapper_uuid(&json!({"mapperFixedUuid":true})).is_some(),
        "#1334 valid options.mapperFixedUuid"
    );
    assert!(recorded_scripts(&empty).is_none(), "#1334 absent scripts");
    assert!(
        recorded_scripts(&json!({"scripts":[]})).is_some(),
        "#1334 valid scripts"
    );
    // #1729 fix-r5: `steps` is required (absent = recording drift), but an
    // empty array is a legal recording for the zero-step cases.
    assert!(
        recorded_steps_checked(&empty).is_err(),
        "#1334 steps is a required recording field"
    );
    assert!(
        recorded_steps(&json!({"steps":[]})).is_some(),
        "#1334 valid steps"
    );
    assert!(recorded_op_checked(&empty).is_err(), "#1334 missing op");
    assert!(
        recorded_op(&json!({"op":"value"})).is_some(),
        "#1334 valid op"
    );
    assert!(
        recorded_stop_reason(&empty).is_none(),
        "#1334 absent stop.reason"
    );
    assert!(
        recorded_stop_reason(&json!({"reason":"value"})).is_some(),
        "#1334 valid stop.reason"
    );
}

#[test]
fn recorded_request_modes_and_bytes_stay_legal() {
    for mode in ["close", "keepalive", "none"] {
        let step = json!({"method":"POST","target":"/webhooks/main","mode":mode,"body":"é","headers":{"x-test":"yes"}});
        let wire = request_bytes(&step);
        assert_eq!(
            wire.contains("Connection: close"),
            mode == "close",
            "#1334 close header matches recorded mode"
        );
        assert!(
            wire.ends_with("Content-Length: 2\r\n\r\né"),
            "#1334 recorded UTF-8 body length"
        );
    }
    assert_eq!(
        request_bytes(
            &json!({"headers":{"Connection":"keep-alive","Content-Length":"0"},"body":""})
        )
        .matches("Connection:")
        .count(),
        1,
        "#1334 explicit headers preserved"
    );
    assert_eq!(
        recorded_port(&json!({"port":"<busy-port>"})).and_then(Value::as_str),
        Some("<busy-port>"),
        "#1334 busy-port sentinel"
    );
    assert!(
        request_bytes(&json!({"method":"GET","target":"/health","version":"HTTP/1.0"}))
            .starts_with("GET /health HTTP/1.0\r\n"),
        "#1684 a recorded version lands on the request line"
    );
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
    /// The recorder's deadline passed with nothing on the wire: the socket is
    /// still open and silent (the 400 ms null-body probe).
    NoResponse,
    /// The peer closed the socket without sending a byte (the oversize cell).
    /// Kept apart from `NoResponse` so the oracle can tell "closed" from
    /// "hung", which the two endings used to collapse into one value.
    Closed,
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

/// The recorder's `close` handler: the peer ended the socket.
fn eof_finish(raws: Vec<String>, buffer: String) -> Exchange {
    if buffer.is_empty() && raws.is_empty() {
        Exchange::Closed
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
            Ok(Ok(0)) => return eof_finish(raws, buffer),
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
/// sequence with `date` masked and chunk-size lines case-folded — plus, on
/// the `parser_admission` step only, hyper's bare-400 `content-length: 0`
/// dropped (A8.4-NATIVE-HTTP-001 item 3, scoped like api-server's #1683
/// fix). Every other length line compares verbatim, so an unregistered
/// step that grows one replays red instead of normalizing away (#2153).
fn normalize(raw: &str, parser_admission: bool) -> Value {
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
        if (parser_admission || crate::mutants::on("webhook-replay-cl0-mask-always"))
            && name == "content-length"
            && value == "0"
        {
            // hyper's implicit empty-body length on the registered bare
            // 400 only.
            continue;
        }
        let entry = format!("{name}: {}", if name == "date" { "<date>" } else { value });
        headers.push(entry);
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

/// #2153: the label of the one step whose hyper bytes carry a
/// `content-length: 0` the TS golden does not — the bare parser-level 400
/// of A8.4-NATIVE-HTTP-001 item 3, scoped like api-server's
/// `parser_admission` cells.
const PARSER_ADMISSION_LABEL: &str = "unknown-method";

/// #2153 (fix-r3, review r3): the predicate that grants the
/// `content-length: 0` mask. The exemption in `run_request` and the
/// uniqueness guard both go through this one function, so widening the
/// predicate anywhere fails the guard, which pins that exactly one oracle
/// step satisfies it; the raw-byte pin's step lookup reads the same shared
/// `PARSER_ADMISSION_LABEL` constant the predicate compares against, so its
/// two legs stay same-sourced either way.
fn parser_admission(label: &str) -> bool {
    if crate::mutants::on("webhook-replay-cl0-wide") {
        // #2789 negative control: the pre-#2153 blanket shape — every step
        // exempt — which the uniqueness guard below must reject.
        return true;
    }
    label == PARSER_ADMISSION_LABEL
}

/// The recorder's `runRequests`: one oracle response entry per exchange
/// outcome, keep-alive counts included.
async fn run_request(port: &Option<u16>, step: &Value, responses: &mut Vec<Value>) {
    let label = step["label"].as_str().expect("#1334 oracle step label");
    // #2153: see `fn parser_admission` above. Every other step compares its
    // length lines verbatim, so an extra one replays red instead of
    // normalizing away; the
    // `content_length_zero_stays_scoped_to_the_registered_rejection` pin
    // holds the raw bytes behind the mask in both directions.
    let parser_admission_step = parser_admission(label);
    let mode = recorded_mode(step).unwrap_or("close");
    // Validate even when close mode or a failed exchange will not consume
    // these fields. Otherwise malformed recordings can still replay green.
    let recorded_count = recorded_count(step).unwrap_or(1);
    let pin_response = recorded_pin(step).unwrap_or(false);
    let count = if mode == "keepalive" {
        usize::try_from(recorded_count).expect("#1334 count must fit usize")
    } else {
        1
    };
    let exchange = raw_exchange(*port, &request_bytes(step), mode, count).await;
    match exchange {
        Exchange::Error(code) => responses.push(json!({"label": label, "error": code})),
        Exchange::NoResponse => responses.push(json!({"label": label, "noResponse": true})),
        Exchange::Closed => responses.push(json!({"label": label, "closed": true})),
        Exchange::Raws(raws) => {
            if count == 1 {
                let one = raws.into_iter().next().unwrap_or_default();
                let mut entry = normalize(&one, parser_admission_step);
                entry["label"] = json!(label);
                responses.push(entry);
                if pin_response {
                    responses.push(json!({"label": format!("{label}-pin"), "raw": pin(&one)}));
                }
            } else {
                for (index, one) in raws.iter().enumerate() {
                    let mut entry = normalize(one, parser_admission_step);
                    entry["label"] = json!(format!("{label}-{}", index + 1));
                    responses.push(entry);
                }
            }
        }
    }
}

#[tokio::test]
#[should_panic(expected = "#1334 count must be a u64")]
async fn request_count_is_validated_in_close_mode() {
    run_request(
        &None,
        &json!({"label":"bad","mode":"close","count":[]}),
        &mut Vec::new(),
    )
    .await;
}

#[tokio::test]
#[should_panic(expected = "#1334 pin must be a boolean")]
async fn request_pin_is_validated_before_a_failed_exchange() {
    run_request(&None, &json!({"label":"bad","pin":[]}), &mut Vec::new()).await;
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
        session_key: JsString::from(recorded_cron_session(step).unwrap_or_default()),
        channel_key: ChannelKey::new("webhook"),
        origin_session_key: recorded_cron_origin_session(step).map(JsString::from),
        origin_channel_key: recorded_cron_origin_channel(step).map(ChannelKey::new),
        text: JsString::from(recorded_cron_text(step).unwrap_or("cron text")),
    }
}

/// One oracle scenario through the native channel: build the options the
/// generator built (the `<busy-port>` sentinel gets its own blocker), drive
/// the step DSL against the real listener, then capture after the same
/// quiet wait the recorder used.
async fn run_case(case: &Value) -> Value {
    let options = recorded_options(case).unwrap_or(&Value::Null);
    let steps = recorded_steps(case).cloned().unwrap_or_default();
    let mut blocker = None;
    let port_option = match recorded_port(options).and_then(Value::as_str) {
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
        _ => recorded_port(options).and_then(Value::as_f64),
    };
    let routes = parse_json(
        &recorded_routes(options)
            .expect("#1334 options.routes validated object")
            .to_string(),
    )
    .expect("#1334 oracle routes JSON")
    .as_object()
    .expect("#1334 oracle routes object")
    .clone();
    let gateway = Arc::new(MockGateway::from_scripts(
        recorded_scripts(case)
            .map(Vec::as_slice)
            .unwrap_or_default(),
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
        secret: recorded_secret(options).map(str::to_owned),
        routes: Some(routes),
        rate_limit: recorded_rate_limit(options),
        max_body_bytes: recorded_max_body(options),
        mapper: recorded_mapper_uuid(options)
            .unwrap_or(false)
            .then(|| WebhookSessionMapper::new(Arc::new(|| JsString::from(FIXED_UUID)))),
        now: None,
        accepted: None,
        request_timeout: None,
    });
    let deps = ChannelStartDeps {
        config: None,
        report_status: None,
        log: Some(logger),
        turn_gateway: (case["name"].as_str() != Some("gateway-missing"))
            .then(|| gateway.clone() as Arc<dyn TurnGateway>),
    };
    channel
        .start(deps.clone())
        .await
        .expect("#1334 channel start");
    let mut port = channel.bound_addr().map(|address| address.port());
    let mut responses: Vec<Value> = Vec::new();
    for step in steps {
        match recorded_op(&step).expect("#1334 op validated string") {
            "request" => run_request(&port, &step, &mut responses).await,
            "stop" => {
                channel
                    .stop_with_reason(recorded_stop_reason(&step))
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

/// `Date.now() / 1000` carries millisecond resolution and nothing finer, so
/// two no-id deliveries inside one millisecond share a fallback id there. A
/// finer Rust clock would hand them different dedup keys, which is why the
/// default clock is quantized to milliseconds.
#[test]
fn fallback_delivery_id_carries_millisecond_resolution() {
    // Fixed instants, not a wall-clock sample: the contract is about what the
    // quantizer does to a given instant, so a single `default_now()` reading
    // could pass by luck (a sample landing on a whole millisecond) and would
    // never see the sub-millisecond bits at all.
    let same_ms_early =
        seconds_from_duration(std::time::Duration::from_nanos(1_700_000_000_123_000_000));
    // Same millisecond, 999_999 ns later. `Date.now()` cannot tell these apart,
    // so neither may the fallback id, or two deliveries in one millisecond
    // would stop deduping. A sub-millisecond-preserving quantizer
    // (`as_secs_f64()`) fails here.
    let same_ms_late =
        seconds_from_duration(std::time::Duration::from_nanos(1_700_000_000_123_999_999));
    negative_assert_eq!(
        number_to_string(same_ms_early),
        number_to_string(same_ms_late),
        "#1334 deliveries inside one millisecond share the fallback id"
    );
    // The next millisecond must differ, so millisecond resolution survives. A
    // whole-second quantizer (`as_secs()`) fails here.
    let next_ms = seconds_from_duration(std::time::Duration::from_nanos(1_700_000_000_124_000_000));
    negative_assert!(
        number_to_string(same_ms_early) != number_to_string(next_ms),
        "#1334 adjacent milliseconds give different fallback ids"
    );
    // And the serialized form keeps TS's at-most-three-decimals shape.
    for value in [same_ms_early, same_ms_late, next_ms] {
        let text = number_to_string(value);
        let decimals = text.split('.').nth(1).map(str::len).unwrap_or(0);
        negative_assert!(
            decimals <= 3,
            "#1334 the fallback delivery id keeps TS millisecond resolution, got {text}"
        );
    }
}

/// The recorder must keep "the peer closed without a byte" apart from "the
/// deadline passed in silence": the oversize cell is the former and the
/// null-body cell the latter, and a single `noResponse` value for both made
/// the oversize cell blind to a regression from "close" to "hang".
#[test]
fn silence_and_zero_byte_close_stay_distinguishable() {
    negative_assert_eq!(
        format!("{:?}", timer_finish(Vec::new(), String::new())),
        "NoResponse",
        "#1334 the silence probe records noResponse"
    );
    negative_assert_eq!(
        format!("{:?}", eof_finish(Vec::new(), String::new())),
        "Closed",
        "#1334 the zero-byte close records closed"
    );
}

/// The terminal comparison reads `expected` through the same tri-state
/// accessor as every other recording field: a present-but-wrong-type
/// container fails with a `#1334` diagnostic instead of a generic object
/// mismatch, and an absent one cannot shrink the case to a blind compare.
fn recorded_expected_checked(value: &Value) -> Result<Option<&Value>, String> {
    recorded_checked(
        value.get("expected"),
        "expected",
        "an object",
        |value| value.is_object().then_some(value),
        true,
        crate::mutants::on("webhook-oracle-recorded-expected"),
    )
}

fn recorded_expected(value: &Value) -> Option<&Value> {
    recorded_expected_checked(value).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn recorded_expected_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_expected_checked(&json!({"expected": []})).is_err(),
        "#1334 #1729 rejects expected type drift"
    );
}

#[test]
#[should_panic(expected = "#1334 expected must be an object")]
fn recorded_expected_panics_on_wrong_type() {
    recorded_expected(&json!({"expected": []}));
}

#[test]
#[should_panic(expected = "#1334 expected must be an object, got <missing>")]
fn recorded_expected_panics_when_absent() {
    recorded_expected(&json!({}));
}

async fn replay(name: &str) {
    let case = case(name);
    let expected = recorded_expected(&case)
        .expect("#1334 expected validated object")
        .clone();
    let actual = run_case(&case).await;
    negative_assert_eq!(actual, expected, "#1334 webhook oracle case {}", name);
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
    // #1682: `/webhooks/<Object.prototype own property name>` resolves through
    // the prototype chain in TS, so it authenticates against the global secret
    // and is accepted instead of 404'd.
    object_prototype_route_names => "object-prototype-route-names",
}

/// #1684: oracle cases with a dedicated direct test instead of a generic
/// replay. `http10` records Node's `HTTP/1.1` status line for an HTTP/1.0
/// request, but hyper writes its own `HTTP/1.0` line (the accepted-diff
/// entry A8.3-CH-WEBHOOK-004, approved by its R1 correction
/// `A8.3-CH-WEBHOOK-004-R1`, the channel's record of the
/// A8.4-NATIVE-HTTP-001 item 4 class), and the oracle's `closed` flag only
/// records zero-byte closes, so the socket close is asserted directly too.
const PINNED_CASE_NAMES: &[&str] = &["http10"];

/// #1682: the routing fallback has no name table of its own — it asks the
/// shared `config::builtin_property` predicate. This pins that predicate to
/// the *measured* V8 set, so dropping a name (which would silently restore a
/// 404 divergence) or inventing one (which would 200 a genuinely unknown
/// route) fails a test instead of only a review.
///
/// The reference list is `Object.getOwnPropertyNames(Object.prototype)` under
/// Node v22.22.0, the baseline the webhook oracle is generated on (see
/// `tests/fixtures/webhook-channel/generate.mts`, case
/// `object-prototype-route-names`).
#[test]
fn object_prototype_names_match_the_measured_v8_set() {
    let measured = [
        "constructor",
        "__defineGetter__",
        "__defineSetter__",
        "hasOwnProperty",
        "__lookupGetter__",
        "__lookupSetter__",
        "isPrototypeOf",
        "propertyIsEnumerable",
        "toString",
        "valueOf",
        "__proto__",
        "toLocaleString",
    ];
    // Set equality, both directions: a name missing from the shared table would
    // silently restore a 404 divergence, and a name *added* to it would 200 a
    // route the baseline answers 404 for. Membership probes alone catch only
    // the first direction (#2183 评审 r2 P1).
    let mut table = crate::config::BUILTIN_PROPERTY_NAMES;
    table.sort_unstable();
    let mut expected = measured;
    expected.sort_unstable();
    negative_assert_eq!(
        table,
        expected,
        "#1682 Object.prototype own property names under Node v22.22.0"
    );
    negative_assert!(
        !crate::config::builtin_property(&JsString::from("nope")),
        "#1682 an ordinary name must not answer as an Object.prototype property"
    );
}

/// The replay list, the pinned list and the oracle describe the same case
/// set; a scenario added to the oracle without a test (or vice versa) fails
/// here.
#[test]
fn replay_cases_cover_exactly_the_oracle() {
    let recorded = oracle();
    let cases = recorded["cases"]
        .as_array()
        .expect("#1334 oracle cases array");
    negative_assert_eq!(
        recorded["expectedCaseCount"].as_u64(),
        Some(21),
        "#1334 oracle declares twenty-one cases"
    );
    negative_assert_eq!(cases.len(), 21, "#1334 oracle carries twenty-one cases");
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
    replayed.extend_from_slice(PINNED_CASE_NAMES);
    replayed.sort_unstable();
    negative_assert_eq!(
        replayed,
        names,
        "#1334 replay case names and oracle case names are the same set"
    );
}

/// #1684: an HTTP/1.0 request — even one asking for `Connection: keep-alive`
/// — draws a close-delimited response and a closed socket, as Node does.
/// Only the status line differs: Node echoes `HTTP/1.1`, hyper writes its own
/// `HTTP/1.0` (the accepted-diff entry A8.3-CH-WEBHOOK-004, oracle case
/// `http10` records Node's line), as in the other listeners — the lane list lives in
/// `finalize`'s `Sibling:` paragraph and in `A8.3-CH-WEBHOOK-004`「范围」.
#[tokio::test]
async fn http10_request_closes_the_connection() {
    let case = case("http10");
    let steps = recorded_steps(&case).expect("#1334 oracle steps");
    let request = request_bytes(
        steps
            .first()
            .expect("#1684 the http10 case carries one request step"),
    );
    assert!(
        request.starts_with("GET /health HTTP/1.0\r\n")
            && request.contains("\r\nConnection: keep-alive\r\n"),
        "#1684 pin http10: the oracle step stays an HTTP/1.0 keep-alive request: {request:?}"
    );
    let expected = recorded_expected(&case).expect("#1334 expected validated object");
    // The recorded case's own options, not a hand-built twin: a regenerated
    // cell that changes the route or port must fail here too.
    let options = recorded_options(&case).expect("#1334 oracle options object");
    let routes = parse_json(
        &recorded_routes(options)
            .expect("#1334 options.routes validated object")
            .to_string(),
    )
    .expect("#1334 oracle routes JSON")
    .as_object()
    .expect("#1334 oracle routes object")
    .clone();
    let channel = WebhookChannel::new(WebhookChannelOptions {
        port: recorded_port(options).and_then(Value::as_f64),
        routes: Some(routes),
        ..Default::default()
    });
    channel
        .start(ChannelStartDeps::default())
        .await
        .expect("#1684 start for the http10 pin");
    let port = channel
        .bound_addr()
        .expect("#1684 bound listener for the http10 pin")
        .port();
    let (raw, closed) = raw_until_close(port, &request).await;
    let _ = channel.stop_with_reason(Some("1684")).await;

    assert!(
        raw.starts_with("HTTP/1.0 200 OK\r\n"),
        "#1684 pin http10: hyper answers with its own 1.0 status line (Node's 1.1 line is the accepted-diff entry A8.3-CH-WEBHOOK-004): {raw:?}"
    );
    assert!(
        closed,
        "#1684 pin http10: the 1.0 response closes the connection like Node: {raw:?}"
    );
    // Everything else replays: the close-delimited framing (no `content-length`,
    // no chunking), the header set and the body, byte for byte after the usual
    // normalization — with no length exemption, since neither side writes one
    // here (the 1.0 close-delimited framing is its own registered diff, pinned
    // above as hyper's own 1.0 status line).
    let actual = normalize(&raw, false);
    let recorded = &expected["responses"][0];
    negative_assert_eq!(
        actual["headers"],
        recorded["headers"],
        "#1684 pin http10: the 1.0 response replays Node's header set and close-delimited framing"
    );
    negative_assert_eq!(
        actual["body"],
        recorded["body"],
        "#1684 pin http10: the 1.0 response replays Node's body"
    );
}

/// #2153: the replay normalization drops hyper's `content-length: 0` on the
/// one registered bare parser-level 400 (the `unknown-method` step,
/// A8.4-NATIVE-HTTP-001 item 3, scoped like api-server's `parser_admission`
/// cells), so the raw bytes have to be pinned in both directions or the mask
/// could hide a drift: the rejection that stopped carrying the line (a
/// missing rejection) and an unregistered answer that grew one (the extra
/// length line the replay must catch) both compare green through the mask
/// alone. Both requests are the case's own recorded steps, sent through the
/// same `request_bytes` path the replay uses.
#[tokio::test]
async fn content_length_zero_stays_scoped_to_the_registered_rejection() {
    let case = case("health-and-routing");
    let steps = recorded_steps(&case).expect("#1334 oracle steps");
    let step = |label: &str| {
        steps
            .iter()
            .find(|step| step["label"].as_str() == Some(label))
            .unwrap_or_else(|| panic!("#2153 the {label} step is missing from the oracle"))
    };
    let options = recorded_options(&case).expect("#1334 oracle options object");
    let routes = parse_json(
        &recorded_routes(options)
            .expect("#1334 options.routes validated object")
            .to_string(),
    )
    .expect("#1334 oracle routes JSON")
    .as_object()
    .expect("#1334 oracle routes object")
    .clone();
    let channel = WebhookChannel::new(WebhookChannelOptions {
        routes: Some(routes),
        // #2153 fix-r1 (review r1): a temporary port, like every other
        // listener in this file — `..Default::default()` would bind the
        // channel's DEFAULT_PORT (8643), which has no ownership and fails
        // loudly under a concurrent test run.
        port: Some(0.0),
        ..Default::default()
    });
    channel
        .start(ChannelStartDeps::default())
        .await
        .expect("#2153 start for the content-length pin");
    let port = channel
        .bound_addr()
        .expect("#2153 bound listener for the content-length pin")
        .port();

    // Registered direction: the bare 400 really carries the line the mask
    // drops — a hyper that stopped emitting the rejection (or its length
    // line) fails here instead of replaying green.
    let (bare, closed) = raw_until_close(port, &request_bytes(step(PARSER_ADMISSION_LABEL))).await;
    assert!(
        bare.starts_with("HTTP/1.1 400 Bad Request\r\n"),
        "#2153 pin: the unknown-method step stays a bare parser-level 400: {bare:?}"
    );
    assert!(
        bare.to_ascii_lowercase()
            .contains("\r\ncontent-length: 0\r\n"),
        "#2153 pin: the bare 400 carries the registered content-length: 0: {bare:?}"
    );
    assert!(
        closed,
        "#2153 pin: the bare 400 closes the connection: {bare:?}"
    );

    // Unregistered direction: the health 200 carries exactly its one
    // recorded length line — a spurious `content-length: 0` anywhere else is
    // the extra line the narrowed replay must catch.
    let (health, _) = raw_until_close(port, &request_bytes(step("health"))).await;
    let lower = health.to_ascii_lowercase();
    negative_assert_eq!(
        lower.match_indices("content-length:").count(),
        1,
        "#2153 pin: the health 200 carries exactly one content-length line: {health:?}"
    );
    assert!(
        lower.contains("\r\ncontent-length: 36\r\n"),
        "#2153 pin: the health 200 keeps its recorded length 36: {health:?}"
    );

    let _ = channel.stop_with_reason(None).await;
}

/// #2153: the exemption `run_request` grants the `unknown-method` step is
/// the only one left — an injected `content-length: 0` on any other step (or
/// a spurious second line next to a recorded value) must survive
/// normalization, while the registered bare 400's line stays masked.
#[test]
fn only_the_registered_step_masks_a_content_length_zero() {
    let head =
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nDate: Fri, 09 Jan 2026 00:00:00 GMT";
    let plain = format!("{head}\r\n\r\n{{\"ok\":true}}");
    let injected = format!("{head}\r\ncontent-length: 0\r\n\r\n{{\"ok\":true}}");
    let valued = format!("{head}\r\ncontent-length: 36\r\n\r\n{{\"ok\":true}}");
    let duplicated =
        format!("{head}\r\ncontent-length: 36\r\ncontent-length: 0\r\n\r\n{{\"ok\":true}}");
    negative_assert!(
        normalize(&injected, false) != normalize(&plain, false),
        "#2153 an extra content-length: 0 on an unregistered step must not normalize away"
    );
    negative_assert!(
        normalize(&duplicated, false) != normalize(&valued, false),
        "#2153 a spurious second content-length: 0 must not normalize away"
    );
    // The registered direction stays masked: the bare 400's hyper line is
    // the one exemption left, so the TS-shaped bytes still compare equal.
    let bare = "HTTP/1.1 400 Bad Request\r\nconnection: close\r\ncontent-length: 0\r\n\r\n";
    let ts_shaped = "HTTP/1.1 400 Bad Request\r\nconnection: close\r\n\r\n";
    negative_assert_eq!(
        normalize(bare, true),
        normalize(ts_shaped, true),
        "#2153 the registered unknown-method exemption still masks hyper's content-length: 0"
    );
}

/// #2153 fix-r1 (review r1): the exemption picks its step by label, and
/// labels are not unique across the oracle — `first` / `repeat` already
/// repeat across cases. This guard pins that `PARSER_ADMISSION_LABEL`
/// occurs in exactly one step of the whole oracle, so a future case reusing
/// the label cannot silently inherit the bare-400 exemption; it fails here
/// instead. fix-r2 (review r2): the filter reads the same shared
/// `PARSER_ADMISSION_LABEL` the exemption itself uses. fix-r3 (review r3):
/// both go through `fn parser_admission`, so widening the predicate
/// anywhere — not just the constant — fails this guard.
#[test]
fn unknown_method_is_the_label_of_exactly_one_oracle_step() {
    let value = oracle();
    let hits = value["cases"]
        .as_array()
        .expect("#1334 oracle cases array")
        .iter()
        .flat_map(|case| {
            let name = case["name"].as_str().expect("#1334 oracle case name");
            recorded_steps(case)
                .expect("#1334 oracle steps")
                .iter()
                .filter(|step| step["label"].as_str().is_some_and(parser_admission))
                .map(move |_| name)
        })
        .collect::<Vec<&str>>();
    assert_eq!(
        hits,
        vec!["health-and-routing"],
        "#2153 the unknown-method label belongs to exactly the health-and-routing \
         case's bare parser-level 400 — any other case reusing the label must \
         fail here rather than inherit the content-length exemption"
    );
}

/// The fixture must regenerate byte-identically on a host with the fixed
/// baseline; the authoring session has no such host.
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn webhook_channel_regenerates_identically() {
    crate::test_baseline::regenerates_identically("webhook-channel");
}

/// #1813: a zero-step cell replays no request at all, so a scenario whose
/// whole `steps` array went missing would still replay "successfully" against
/// a recording it never exercised (`steps` itself is required, but an empty
/// array is legal). Only the two cells whose scenario ends at the bind failure
/// may carry zero steps; every other cell must record at least one step.
#[test]
fn zero_step_cells_are_the_two_named_cells() {
    // #1813 negative control: model a recording that lost one cell's whole
    // `steps` array while the rest still parses — that is the drift this
    // check exists to catch. The switch discards `invalid-signature`'s recorded
    // steps at the read result, so the classification below still runs through
    // `steps.is_empty()` only; a KILLED here proves the comparison notices a
    // genuinely empty read. (Mutating the expectation, or adding the name to
    // the list directly, would only prove the assertion is not empty.)
    const ZERO_STEP: &[&str] = &["invalid-port", "port-in-use"];
    const LOST_STEPS_CELL: &str = "invalid-signature";
    let lost_steps = crate::mutants::on("webhook-zero-step-whitelist");
    let recorded = oracle();
    let cases = recorded["cases"]
        .as_array()
        .expect("#1334 oracle cases array");
    let mut zero: Vec<&str> = Vec::new();
    for case in cases {
        let name = case["name"].as_str().expect("#1334 oracle case name");
        let steps: &[Value] = recorded_steps(case).expect("#1334 steps validated array");
        let steps: &[Value] = if lost_steps && name == LOST_STEPS_CELL {
            &[]
        } else {
            steps
        };
        if steps.is_empty() {
            zero.push(name);
        }
    }
    zero.sort_unstable();
    let mut allowed: Vec<&str> = ZERO_STEP.to_vec();
    allowed.sort_unstable();
    negative_assert_eq!(
        zero,
        allowed,
        "#1334 only the named cells may replay zero steps"
    );
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

/// #1681: `{"text": ""}` is not a delivery. The response is the ignored
/// reply and no `deliveryInfo` entry is written, so a later reply for
/// `webhook:main:d5` cannot resolve `deliver`.
#[tokio::test]
async fn empty_text_request_is_not_a_delivery() {
    let case = case("no-text");
    // #1729 fix-r2 (sibling P2-a): read the container through the checked
    // accessors too, so a drifted `options`/`routes` type fails with the
    // field-level #1334 diagnostic instead of a bare serde error.
    let options = recorded_options(&case).expect("#1334 oracle options object");
    let routes = parse_json(
        &recorded_routes(options)
            .expect("#1334 options.routes validated object")
            .to_string(),
    )
    .expect("#1334 oracle routes JSON")
    .as_object()
    .expect("#1334 oracle routes object")
    .clone();
    let channel = WebhookChannel::new(WebhookChannelOptions {
        port: Some(0.0),
        host: None,
        secret: None,
        routes: Some(routes),
        rate_limit: None,
        max_body_bytes: None,
        mapper: None,
        now: None,
        request_timeout: None,
        accepted: None,
    });
    channel
        .start(ChannelStartDeps {
            config: None,
            report_status: None,
            log: None,
            turn_gateway: None,
        })
        .await
        .expect("#1334 channel start");
    let port = channel.bound_addr().map(|address| address.port());
    let mut responses = Vec::new();
    let step = json!({
        "op": "request",
        "label": "empty-text",
        "method": "POST",
        "target": "/webhooks/main",
        "headers": {"x-delivery-id": "d5"},
        "body": "{\"text\":\"\"}",
        "mode": "close",
    });
    run_request(&port, &step, &mut responses).await;
    negative_assert_eq!(
        responses.first().and_then(|entry| entry["body"].as_str()),
        Some("{\"status\":\"ignored\",\"reason\":\"no text content\"}"),
        "#1681 the empty string is ignored"
    );
    negative_assert!(
        channel
            .ctx
            .state
            .lock()
            .expect("#1334 webhook state mutex")
            .delivery_info
            .is_empty(),
        "#1681 the empty string writes no delivery info"
    );
    channel
        .stop_with_reason(None)
        .await
        .expect("#1334 channel stop");
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
        accepted: None,
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
            turn_gateway: Some(gateway.clone() as Arc<dyn TurnGateway>),
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

/// #1614: a rejected `submit` must reach the reply and the log, must not be
/// retried, and must not leave the chat marked active — the three things the
/// success-only mocks left to static comparison.
#[tokio::test]
async fn submit_rejection_delivers_the_failure_without_retrying_or_leaking_the_active_chat() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let gateway = Arc::new(FailingGateway {
        calls: calls.clone(),
    });
    let recorder = Recorder::default();
    let channel = WebhookChannel::new(test_options());
    install(&channel, Some(gateway), recorder.logger());
    let ctx = channel.ctx.clone();
    handle_incoming(&ctx, "webhook:main:err", "hello".to_owned())
        .await
        .expect("#1614 a rejected submit is surfaced as a delivered failure reply");
    // ① 错误传播：网关错误进日志，用户拿到失败文案而不是空回复。
    negative_assert!(
        recorder.has("webhook: submitTurn error: Error: submit rejected"),
        "#1614 the gateway rejection reaches the channel log"
    );
    negative_assert!(
        recorder.has(
            "webhook: response for webhook:main:err: ⚠️ Failed to process this message. Please retry."
        ),
        "#1614 the rejected turn's failure reply is delivered"
    );
    // ② 不重试：一次投递只提交一次。
    negative_assert_eq!(
        calls.lock().expect("#1614 failing calls mutex").len(),
        1,
        "#1614 a rejected submit is not retried"
    );
    // ③ 状态不残留：拒绝后聊天不留在 active 集合里。
    let active = ctx
        .state
        .lock()
        .expect("#1334 webhook state mutex")
        .active_chats
        .lock()
        .expect("#1738 webhook active chats mutex")
        .contains("webhook:main:err");
    negative_assert!(
        !active,
        "#1614 the rejected chat leaves no half-open session"
    );
}

/// #1738: a panic inside `processMessage` must not leak the active-chat
/// slot. The webhook delivery is fire-and-forget (`tokio::spawn`, handle
/// dropped), so the dying task would otherwise leave the chat marked active
/// and every later delivery for it silently skipped until restart — the TS
/// baseline releases the slot in `finally`, which the Rust port's paired
/// insert/remove could not survive.
#[tokio::test]
async fn a_panicking_process_message_does_not_leak_the_active_chat() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let gateway = Arc::new(PanickingGateway {
        calls: calls.clone(),
    });
    let recorder = Recorder::default();
    let channel = WebhookChannel::new(test_options());
    install(&channel, Some(gateway), recorder.logger());
    let ctx = channel.ctx.clone();

    // The production shape: only the spawned task dies, the process does not.
    let joined = tokio::spawn({
        let ctx = ctx.clone();
        async move { handle_incoming(&ctx, "webhook:main:chat", "one".to_owned()).await }
    })
    .await;
    let error =
        joined.expect_err("#1738 the injected panic must fail the spawned task, not vanish");
    negative_assert!(
        error.is_panic(),
        "#1738 the spawn must end in the panic, not a clean return"
    );
    let panic = error.into_panic();
    let payload = panic
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    negative_assert!(
        payload.contains("#1738 injected process_message panic (webhook)"),
        "#1738 unexpected panic payload: {payload:?}"
    );

    // C1: the unwind releases the slot — no residue until restart.
    let active = ctx
        .state
        .lock()
        .expect("#1334 webhook state mutex")
        .active_chats
        .lock()
        .expect("#1738 webhook active chats mutex")
        .contains("webhook:main:chat");
    negative_assert!(
        !active,
        "#1738 a panicking turn must not leave its chat marked active"
    );

    // C2: the next delivery for the same chat reaches the gateway again
    // instead of being dropped as already active.
    handle_incoming(&ctx, "webhook:main:chat", "two".to_owned())
        .await
        .expect("#1738 the redelivery after the panic returns Ok");
    negative_assert_eq!(
        calls.lock().expect("#1738 panicking calls mutex").len(),
        2,
        "#1738 the redelivery submits a second turn"
    );
    negative_assert!(
        !recorder.has("webhook: chat webhook:main:chat already active, skipping"),
        "#1738 the redelivery must not be skipped as already active"
    );
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
            turn_gateway: Some(gateway.clone() as Arc<dyn TurnGateway>),
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
    // #2047: `stop()` breaks the accept loop and drops the listener, so a
    // connection still sitting in the kernel backlog is reset instead of
    // answered — under a loaded runner the accept loop can lag, and the RST
    // then eats the 408 this test is about. A fixed sleep is not a timeout;
    // wait until the loop reports it took *this* connection out of the
    // backlog before asking for a stop.
    let accepted = Arc::new(tokio::sync::Notify::new());
    let notify = accepted.clone();
    let channel = WebhookChannel::new(WebhookChannelOptions {
        port: Some(0.0),
        routes: Some(insecure_routes()),
        request_timeout: Some(Duration::from_millis(200)),
        accepted: Some(Arc::new(move || notify.notify_one())),
        ..Default::default()
    });
    channel
        .start(ChannelStartDeps {
            log: Some(recorder.logger()),
            turn_gateway: Some(gateway.clone() as Arc<dyn TurnGateway>),
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
    timeout(DEADLINE, accepted.notified())
        .await
        .expect("#2047 the accept loop takes the stalled-body connection before stop()");
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

/// A real duplicated descriptor models a child between fork and exec. Exercise
/// the production ChannelHandle stop with both an unused and a served socket.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn stop_disables_listening_even_while_a_duplicate_descriptor_is_alive() {
    for served_request in [false, true] {
        let channel = WebhookChannel::new(test_options());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let duplicate = socket2::SockRef::from(&listener).try_clone().unwrap();
        let (stop, stopping) = watch::channel(false);
        let accept = tokio::spawn(accept_loop(listener, channel.ctx.clone(), stopping));
        {
            let mut state = channel.ctx.state.lock().unwrap();
            state.bound = Some(address);
            state.server = Some(StopSlot { stop, accept });
        }
        let handle = WebhookHandle {
            ctx: channel.ctx.clone(),
        };
        if served_request {
            let mut socket = TcpStream::connect(address).await.unwrap();
            socket
                .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut response = Vec::new();
            timeout(DEADLINE, socket.read_to_end(&mut response))
                .await
                .expect("the webhook listener answers before stop")
                .unwrap();
            assert!(response.starts_with(b"HTTP/1.1 200 "));
        }
        timeout(DEADLINE, handle.stop("duplicate-listener"))
            .await
            .expect("the listener and accepted connections stop")
            .expect("the listener shutdown succeeds");
        assert!(
            TcpStream::connect(address).await.is_err(),
            "stop must disable the shared listener; served_request={served_request}"
        );
        let replacement = TcpListener::bind(address)
            .await
            .expect("the stopped listener's exact address can be rebound with its duplicate alive");
        assert!(
            duplicate.local_addr().is_ok(),
            "the duplicate is still owned"
        );
        drop(replacement);
        drop(duplicate);
    }
}

#[tokio::test]
async fn replacement_channel_rebinds_the_released_port() {
    let first_gateway = Arc::new(MockGateway::from_scripts(&[]));
    let first_recorder = Recorder::default();
    let first = WebhookChannel::new(test_options());
    first
        .start(ChannelStartDeps {
            log: Some(first_recorder.logger()),
            turn_gateway: Some(first_gateway.clone() as Arc<dyn TurnGateway>),
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
            turn_gateway: Some(second_gateway.clone() as Arc<dyn TurnGateway>),
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

/// A `POST /webhooks/main` with a literal `null` body, optionally followed by
/// a pipelined `GET /health` in the same buffer.
fn null_request_bytes(pipeline_health: bool) -> Vec<u8> {
    let mut bytes = b"POST /webhooks/main HTTP/1.1\r\nHost: 127.0.0.1\r\n\
                      Content-Type: application/json\r\nX-Delivery-Id: p0-null\r\n\
                      Content-Length: 4\r\n\r\nnull"
        .to_vec();
    if pipeline_health {
        bytes.extend_from_slice(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    }
    bytes
}

/// #1534 P0: a literal-`null` body followed by a pipelined request, then a
/// client FIN, must reclaim the connection. hyper 1.7.0's
/// `mid_message_detect_eof` returns `Pending` whenever its `read_buf` holds
/// the pipelined bytes, so without the duplicate-socket FIN probe `stop()`
/// waits on the connection forever (measured in
/// `_tmp/20261106-1534-p0/verdict.md`: Rust hung, the same bytes/timing on TS
/// reclaimed the socket in +1 ms). The `webhook-null-body-probe-off` mutant
/// removes the probe so the negative control reproduces the hang.
#[tokio::test]
async fn pipelined_null_body_disconnect_reclaims_the_connection() {
    let channel = WebhookChannel::new(test_options());
    channel
        .start(ChannelStartDeps::default())
        .await
        .expect("#1334 start for the pipelined-null disconnect test");
    let port = channel
        .bound_addr()
        .expect("#1334 bound listener for the pipelined-null test")
        .port();
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1334 connect to the pipelined-null listener");
    // One write, so hyper reads the null POST and the follow-up request into
    // the same buffer — the condition that stops it from reading the FIN.
    stream
        .write_all(&null_request_bytes(true))
        .await
        .expect("#1334 send the pipelined null request");
    // Let hyper read the whole segment into its buffer before the FIN.
    tokio::time::sleep(Duration::from_millis(300)).await;
    stream
        .shutdown()
        .await
        .expect("#1334 half-close the pipelined-null client");
    drop(stream);
    let resolved = matches!(
        timeout(DEADLINE, channel.stop_with_reason(Some("p0"))).await,
        Ok(Ok(()))
    );
    negative_assert!(
        resolved,
        "#1334 a pipelined null body reclaims the connection after the client FIN"
    );
}

/// Plain control: with no pipelined bytes hyper itself reads the FIN
/// (`read_buf` empty), so this resolves both before and after the #1534 fix.
/// It pins that the duplicate-socket probe did not change the plain path.
#[tokio::test]
async fn plain_null_body_disconnect_reclaims_the_connection() {
    let channel = WebhookChannel::new(test_options());
    channel
        .start(ChannelStartDeps::default())
        .await
        .expect("#1334 start for the plain-null disconnect test");
    let port = channel
        .bound_addr()
        .expect("#1334 bound listener for the plain-null test")
        .port();
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1334 connect to the plain-null listener");
    stream
        .write_all(&null_request_bytes(false))
        .await
        .expect("#1334 send the plain null request");
    tokio::time::sleep(Duration::from_millis(300)).await;
    stream
        .shutdown()
        .await
        .expect("#1334 half-close the plain-null client");
    drop(stream);
    let resolved = matches!(
        timeout(DEADLINE, channel.stop_with_reason(Some("p0"))).await,
        Ok(Ok(()))
    );
    negative_assert!(
        resolved,
        "#1334 a plain null body reclaims the connection after the client FIN"
    );
}

/// #1684: one request answered with the raw bytes up to the peer's FIN, plus
/// whether the peer actually closed (`true`) or the read deadline expired
/// while the socket stayed open (`false`) — the oracle's `closed` flag only
/// records zero-byte closes, so a close-delimited response cannot tell the
/// two apart on its own.
async fn raw_until_close(port: u16, request: &str) -> (String, bool) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1684 connect for the http10 pin");
    stream
        .write_all(&latin1_bytes(request))
        .await
        .expect("#1684 write the http10 request");
    let mut raw = String::new();
    let mut chunk = [0u8; 8192];
    let start = Instant::now();
    loop {
        let Some(wait) = DEADLINE.checked_sub(start.elapsed()) else {
            return (raw, false);
        };
        match timeout(wait, stream.read(&mut chunk)).await {
            Err(_elapsed) => return (raw, false),
            Ok(Ok(0)) => return (raw, true),
            Ok(Ok(read)) => raw.extend(chunk[..read].iter().map(|&byte| byte as char)),
            Ok(Err(error)) => panic!("#1684 http10 read failed: {error}"),
        }
    }
}

// ---------------------------------------------------------------- loader

// #2301: the mount is the production construction seam — the startup assembly
// registers it through `production_loader_mounts` and calls
// `load_enabled_channels` (pilotdeckServer.ts:288-296, the loadEnabledChannels
// port). The mount forwards `extra.port` / `extra.secret`;
// `WebhookChannel::from_options` applies the TS nullish-then-`Number()`
// coercions, so a concrete port through the loader config reaches the real
// listener and the TS dist start line — `webhook: listening on
// http://127.0.0.1:<port> — routes: (none configured)` (no routes survive
// `build_options`) — is observable. The port is an OS-assigned free port (bind
// :0, read, drop, rebind through the channel), the same trick the busy-port
// blocker uses, rather than a hard-coded constant that could collide on a
// shared runner.
#[tokio::test]
async fn loader_mount_builds_webhook_channel() {
    let mut mounts = crate::channel::loader::ChannelLoaderMounts::default();
    register(&mut mounts);
    assert!(mounts.is_registered("webhook"), "#1334 loader mount key");
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("#2301 free-port probe");
    let port = probe.local_addr().expect("#2301 probe address").port();
    drop(probe);
    let adapters = parse_json(
        &json!({
            "webhook": {"enabled": true, "extra": {"port": port}},
            "telegram": {"enabled": false},
        })
        .to_string(),
    )
    .expect("#2301 adapters JSON");
    let mut errors = Vec::new();
    let channels = crate::channel::loader::load_enabled_channels(
        Some(&adapters),
        &crate::channel::loader::LoadEnabledChannelsOptions { pilot_home: None },
        &mounts,
        &mut |message| errors.push(message),
    );
    assert!(errors.is_empty(), "#2301 loader errors: {errors:?}");
    assert_eq!(channels.len(), 1, "#2301 exactly the webhook channel loads");
    assert_eq!(channels[0].channel_key().to_string(), "webhook");

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
                    .expect("#2301 log sink")
                    .push((level.to_owned(), message.to_owned()));
            })),
            turn_gateway: None,
        })
        .await
        .expect("#2301 loader-built webhook start");
    let listening =
        format!("webhook: listening on http://127.0.0.1:{port} — routes: (none configured)");
    assert!(
        logs.lock()
            .expect("#2301 log sink")
            .iter()
            .any(|(level, message)| level == "info" && message == &listening),
        "#2301 the TS dist anchor line {listening:?} in {:?}",
        logs.lock().expect("#2301 log sink")
    );
    TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#2301 the loader-built listener accepts connections");
    handle.stop("test").await.expect("#2301 webhook stop");
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "#2301 the listener is gone after stop"
    );
}
