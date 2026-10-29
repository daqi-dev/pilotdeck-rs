use super::binding::BindingOwners;
use super::*;
use pilotdeck_protocol::{JsObject, parse_json, stringify_json};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio::sync::Semaphore;

const ORACLE: &str = include_str!("../../../tests/fixtures/connection-context/oracle.json");
#[derive(Default)]
struct MemoryText {
    // Raw text: an outbound id may contain a lone surrogate escape, which
    // serde_json::Value cannot represent.
    frames: Mutex<Vec<String>>,
    closes: Mutex<Vec<Value>>,
    final_in_flight: Mutex<Vec<Vec<JsValue>>>,
    owner: Mutex<Option<std::sync::Weak<GatewayConnection>>>,
}
impl GatewayTextConnection for MemoryText {
    fn send_text(&self, message: String) {
        let frame = parse_json(&message).unwrap();
        if frame.get("final") == Some(&JsValue::Bool(true))
            && let Some(owner) = self
                .owner
                .lock()
                .unwrap()
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
        {
            let sessions = session_values(&owner.state.lock().unwrap().in_flight);
            self.final_in_flight.lock().unwrap().push(sessions);
        }
        self.frames.lock().unwrap().push(message);
    }
    fn close(&self, code: u16, reason: &str) {
        self.closes
            .lock()
            .unwrap()
            .push(json!({"code":code,"reason":reason}));
    }
}
/// Session sets may contain lone UTF-16 surrogates. Returning JsValue keeps
/// every code unit; going through serde_json::Value would panic on them.
fn session_values(sessions: &[SessionKey]) -> Vec<JsValue> {
    sessions.iter().map(|key| key.value().clone()).collect()
}
/// Copy an object without one key, preserving insertion order.
fn without_key(source: &JsObject, skip: &str) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in source.entries() {
        if !key.equals_str(skip) {
            object.insert(key.clone(), value.clone());
        }
    }
    object
}
/// Outbound frames may carry lone surrogates; serde_json rejects those, so the
/// semantic replay parses with the same JS-semantics codec as production.
fn frame_values(frames: &[String]) -> Vec<JsValue> {
    frames.iter().map(|raw| parse_json(raw).unwrap()).collect()
}
struct Gateway {
    calls: Mutex<Vec<JsValue>>,
    bindings: BindingOwners,
    preserve: bool,
    permission_event: bool,
    submit_mode: String,
    fail_disconnect: bool,
    replay_bindings: bool,
    hold: std::sync::atomic::AtomicBool,
    hold_describe: std::sync::atomic::AtomicBool,
    started: Semaphore,
    release: Semaphore,
    submit_gates: Mutex<Vec<Arc<Semaphore>>>,
}
impl Gateway {
    fn new(preserve: bool) -> Self {
        Self {
            calls: Mutex::new(vec![]),
            bindings: BindingOwners::default(),
            preserve,
            permission_event: false,
            submit_mode: String::new(),
            fail_disconnect: false,
            replay_bindings: false,
            hold: std::sync::atomic::AtomicBool::new(false),
            hold_describe: std::sync::atomic::AtomicBool::new(false),
            started: Semaphore::new(0),
            release: Semaphore::new(0),
            submit_gates: Mutex::new(vec![]),
        }
    }
    fn record(&self, method: &str, params: JsValue) {
        let mut call = JsObject::new();
        call.insert("method".into(), JsValue::from(method));
        call.insert("params".into(), params);
        self.calls.lock().unwrap().push(JsValue::Object(call));
    }
}
impl ConnectionGateway for Gateway {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async {
            self.calls
                .lock()
                .unwrap()
                .push(js(&json!({"method":"describe_server"})));
            if self.hold_describe.load(std::sync::atomic::Ordering::SeqCst) {
                self.started.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            // TS insertion order (name before capabilities), not serde's sort.
            Ok(json_text(
                r#"{"name":"fixture-server","capabilities":["fixture"]}"#,
            ))
        })
    }
    fn request(
        &self,
        method: JsString,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            match method.to_utf8().ok().as_deref() {
                Some("describe_server") => {
                    // TS dispatchRequest calls gateway.describeServer() directly,
                    // so its call record has no params.
                    self.calls
                        .lock()
                        .unwrap()
                        .push(js(&json!({"method":"describe_server"})));
                    Ok(json_text(
                        r#"{"name":"fixture-server","capabilities":["fixture"]}"#,
                    ))
                }
                Some("permission_decide") => {
                    self.record("permission_decide", params);
                    Ok(json_text(r#"{"accepted":true}"#))
                }
                Some("elicitation_respond") => {
                    self.record("elicitation_respond", params);
                    Ok(json_text(r#"{"accepted":true}"#))
                }
                Some("abort_turn") => {
                    // TS fixture records this gateway call as `abort`.
                    self.record("abort", params);
                    Ok(json_text(r#"{"ok":true}"#))
                }
                Some("reconnect_interaction") => {
                    self.record("reconnect_interaction", params.clone());
                    let next =
                        WsBinding::from_json(&property_json(&params, "nextBinding")).unwrap();
                    // The real TS provider treats a falsy previous as absent;
                    // the connection still forwards explicit null unchanged.
                    let previous_json = property_json(&params, "previousBinding");
                    let previous = Some(&previous_json)
                        .filter(|value| truthy(&js(value)))
                        .map(WsBinding::from_json)
                        .transpose()
                        .map_err(RequestError::new)?;
                    // TS InteractionReconnect serializes outcome, binding,
                    // requests in that order; serde_json::Value sorts keys.
                    let result: Value = self.bindings.reconnect(
                        &property_text(&params, "sessionKey"),
                        next,
                        previous.as_ref(),
                    );
                    let mut ordered = JsObject::new();
                    ordered.insert("outcome".into(), js(&result["outcome"]));
                    if let Some(binding) = result.get("binding") {
                        ordered.insert("binding".into(), js(binding));
                    }
                    ordered.insert("requests".into(), js(&result["requests"]));
                    Ok(JsValue::Object(ordered))
                }
                Some("steer_turn") => Err(RequestError::new("plain fixture error")),
                Some("cancel_steer") => Err(RequestError {
                    code: "FIXTURE_ERROR".into(),
                    message: "structured fixture error".into(),
                    details: Some(Box::new(js(&json!({"nested":{"value":1}})))),
                    validation: None,
                }),
                Some("skill_validate") => Err(RequestError {
                    code: "validation_failed".into(),
                    message: "Validation failed.".into(),
                    details: None,
                    validation: Some(Box::new(json_text(
                        r#"{"valid":false,"issues":[{"code":"fixture","message":"fixture validation"}]}"#,
                    ))),
                }),
                Some("skill_read") => Err(RequestError {
                    code: "not_found".into(),
                    message: "fixture skill missing".into(),
                    details: None,
                    validation: None,
                }),
                Some(method) => Err(RequestError::new(format!(
                    "Unknown gateway method {method}."
                ))),
                // The method itself is a lone surrogate: build the TS message
                // from UTF-16 code units, never through to_utf8.
                None => Err(RequestError::new(unknown_method_message(&method))),
            }
        })
    }
    fn submit<'a>(
        &'a self,
        params: JsValue,
        emit: &'a (dyn Fn(JsValue) + Send + Sync),
    ) -> Call<'a, Result<(), RequestError>> {
        Box::pin(async move {
            self.record("submit_turn", params.clone());
            // Provider behavior belongs to TS replay. Barrier tests exercise
            // connection cleanup independently, including malformed mutants.
            if self.replay_bindings {
                self.bindings.reconnect(
                    &property_text(&params, "sessionKey"),
                    WsBinding::from_json(&property_json(&params, "interactionBinding")).unwrap(),
                    None,
                );
            }
            // The provider-side events of A7.2 are still serde Value; this
            // fixture crosses that boundary through ordered JSON text, because
            // serde Value (BTreeMap) cannot carry TS insertion order.
            if self.permission_event || self.submit_mode == "permission" {
                emit(
                    parse_json(r#"{"type":"permission_request","requestId":"fixture-permission"}"#)
                        .unwrap(),
                );
            }
            if self.submit_mode == "throw" {
                return Err(RequestError::new("fixture submit failed"));
            }
            if self.submit_mode == "empty" {
                return Ok(());
            }
            let completed =
                || parse_json(r#"{"type":"turn_completed","runId":"fixture-run"}"#).unwrap();
            let failed = || {
                parse_json(
                    r#"{"type":"error","code":"fixture_error","message":"fixture stream failed","recoverable":true}"#,
                )
                .unwrap()
            };
            // Provider-yielded error events: TS keeps lastError and lastCompleted
            // and sends lastError as the terminal event (GatewayWsConnection.ts:
            // 138-171). Omitted from the original recording, now replayed from TS.
            if self.submit_mode == "error" {
                emit(failed());
                return Ok(());
            }
            if self.submit_mode == "error-then-completed" {
                emit(failed());
                emit(completed());
                return Ok(());
            }
            if self.submit_mode == "completed-then-error" {
                emit(completed());
                emit(failed());
                return Ok(());
            }
            if self.hold.load(std::sync::atomic::Ordering::SeqCst) {
                let release = Arc::new(Semaphore::new(0));
                self.submit_gates.lock().unwrap().push(release.clone());
                self.started.add_permits(1);
                release.acquire().await.unwrap().forget();
            }
            emit(completed());
            if self.submit_mode == "completed-then-throw" {
                return Err(RequestError::new("fixture submit failed"));
            }
            Ok(())
        })
    }
    fn disconnect(
        &self,
        session: JsValue,
        binding: WsBinding,
    ) -> Call<'_, Result<bool, RequestError>> {
        Box::pin(async move {
            let mut params = JsObject::new();
            params.insert("sessionKey".into(), session.clone());
            params.insert("binding".into(), js(&binding.to_json()));
            self.record("disconnect", JsValue::Object(params));
            if self.fail_disconnect {
                return Err(RequestError::new("fixture disconnect failed"));
            }
            let owner = stringify_json(&session).unwrap().unwrap();
            self.bindings.disconnect(&owner, &binding);
            Ok(self.preserve)
        })
    }
    fn abort_on_close(&self, params: JsValue) -> Call<'_, Result<(), RequestError>> {
        Box::pin(async move {
            self.record("abort", params);
            Ok(())
        })
    }
}
fn unknown_method_message(method: &JsString) -> JsString {
    let mut units = "Unknown gateway method ".encode_utf16().collect::<Vec<_>>();
    units.extend_from_slice(method.units());
    units.extend(".".encode_utf16());
    JsString::from_utf16(units)
}
fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
/// Provider results are constructed from source text so insertion order is the
/// TS order; going through serde_json::Value would alphabetize nested keys.
fn json_text(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
// Only the provider's known binding fields need serde JSON. Session keys and
// spread index values stay JsValue: a lone surrogate cannot round-trip serde.
fn property_json(value: &JsValue, key: &str) -> Value {
    value
        .get(key)
        .and_then(|value| stringify_json(value).unwrap())
        .map(|text| serde_json::from_str(&text).unwrap())
        .unwrap_or(Value::Null)
}
fn property_text(value: &JsValue, key: &str) -> String {
    value
        .get(key)
        .and_then(|value| stringify_json(value).unwrap())
        .unwrap_or_default()
}
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(items) = value else {
        panic!("expected array")
    };
    items
}
// Compare complete JSON values without object key-order sensitivity, matching
// the pre-existing serde replay. String code units and every array item remain.
fn normalized(value: &JsValue, ids: &BTreeMap<String, String>) -> JsValue {
    match value {
        JsValue::String(s) => s
            .to_utf8()
            .ok()
            .and_then(|s| ids.get(&s))
            .map(|s| JsValue::from(s.as_str()))
            .unwrap_or_else(|| value.clone()),
        JsValue::Array(items) => JsValue::Array(items.iter().map(|v| normalized(v, ids)).collect()),
        JsValue::Object(items) => {
            let sorted: BTreeMap<_, _> = items
                .entries()
                .iter()
                .map(|(k, v)| (k.clone(), normalized(v, ids)))
                .collect();
            let mut object = JsObject::new();
            for (key, value) in sorted {
                object.insert(key, value);
            }
            JsValue::Object(object)
        }
        _ => value.clone(),
    }
}
/// A regression that rejects the frame must fail instead of parking forever on
/// a gate the request never reaches.
async fn wait_started(gateway: &Gateway, what: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(5), gateway.started.acquire())
        .await
        .unwrap_or_else(|_| panic!("{what} did not start"))
        .unwrap()
        .forget();
}
async fn replay(only: Option<&str>) {
    let oracle = crate::test_baseline::oracle(ORACLE);
    assert_eq!(
        oracle
            .get("sourceSha256")
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        650,
        "#303 exact import closure source count"
    );
    assert_eq!(array(oracle.get("scenarios").unwrap()).len(), 86);
    let mut ids = BTreeMap::new();
    let sequence = std::sync::atomic::AtomicU64::new(0);
    let mut replayed = 0;
    for (index, recorded) in array(oracle.get("scenarios").unwrap()).iter().enumerate() {
        // Steps and observations stay JsValue for the whole replay: a recorded
        // submit may carry a lone surrogate in its frame, which serde rejects.
        let control = without_key(recorded.as_object().unwrap(), "results");
        let name = control
            .get("name")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        // Issue the binding before filtering so a subset keeps the same
        // generation sequence as the full replay (TS increments per connection).
        let binding = WsBinding::issue(&sequence);
        if only.is_some_and(|suffix| !name.ends_with(suffix)) {
            continue;
        }
        replayed += 1;
        let text = Arc::new(MemoryText::default());
        let mut gateway = Gateway::new(
            control
                .get("preserve")
                .and_then(JsValue::as_bool)
                .unwrap_or(false),
        );
        gateway.permission_event = control
            .get("permissionEvent")
            .and_then(JsValue::as_bool)
            .unwrap_or(false);
        gateway.submit_mode = control
            .get("submitMode")
            .and_then(JsValue::as_string)
            .and_then(|mode| mode.to_utf8().ok())
            .unwrap_or_default()
            .to_owned();
        gateway.fail_disconnect = control
            .get("failDisconnect")
            .and_then(JsValue::as_bool)
            .unwrap_or(false);
        gateway.replay_bindings = true;
        let gateway = Arc::new(gateway);
        let connection = GatewayConnection::with_binding(
            text.clone(),
            gateway.clone(),
            "fixture-token".into(),
            "test".into(),
            binding,
        );
        ids.insert(
            connection.binding.connection_id.clone(),
            format!("@connection-{}", index + 1),
        );
        let connection = Arc::new(connection);
        *text.owner.lock().unwrap() = Some(Arc::downgrade(&connection));
        let mut held = None;
        let mut submissions = BTreeMap::new();
        let mut close_errors = Vec::new();
        for (step_index, step) in array(control.get("steps").unwrap()).iter().enumerate() {
            let op = step
                .get("op")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap();
            match op.as_str() {
                "message" => {
                    let raw = step
                        .get("raw")
                        .unwrap()
                        .as_string()
                        .unwrap()
                        .to_utf8()
                        .unwrap();
                    connection.receive(&raw).await.unwrap();
                }
                "notify" => connection.notify("fixture", Some(js(&json!({"value":1})))),
                "start_hello" => {
                    gateway
                        .hold_describe
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    let conn = connection.clone();
                    let raw = stringify_json(step.get("frame").unwrap()).unwrap().unwrap();
                    held = Some(tokio::spawn(
                        async move { conn.receive(&raw).await.unwrap() },
                    ));
                    wait_started(&gateway, "hello describe").await;
                }
                "finish_hello" => {
                    gateway.release.add_permits(1);
                    held.take().unwrap().await.unwrap();
                }
                "start" => {
                    gateway
                        .hold
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    let conn = connection.clone();
                    let raw = stringify_json(step.get("frame").unwrap()).unwrap().unwrap();
                    let task = tokio::spawn(async move { conn.receive(&raw).await.unwrap() });
                    wait_started(&gateway, "submit").await;
                    let release = gateway.submit_gates.lock().unwrap().last().unwrap().clone();
                    let slot = step
                        .get("slot")
                        .and_then(JsValue::as_string)
                        .and_then(|slot| slot.to_utf8().ok())
                        .unwrap_or_else(|| "default".into());
                    assert!(submissions.insert(slot, (release, task)).is_none());
                }
                "close" => connection.closed().await.unwrap(),
                "close_error" => close_errors.push(
                    connection
                        .closed()
                        .await
                        .unwrap_err()
                        .message
                        .to_utf8()
                        .unwrap(),
                ),
                "finish" => {
                    let slot = step
                        .get("slot")
                        .and_then(JsValue::as_string)
                        .and_then(|slot| slot.to_utf8().ok())
                        .unwrap_or_else(|| "default".into());
                    let (release, task) = submissions.remove(&slot).unwrap();
                    release.add_permits(1);
                    task.await.unwrap();
                }
                op => panic!("unknown step {op}"),
            }
            let (in_flight, interactions) = {
                let state = connection.state.lock().unwrap();
                (
                    session_values(&state.in_flight),
                    session_values(&state.interactions),
                )
            };
            let raw_frames = text.frames.lock().unwrap().clone();
            let mut actual = JsObject::new();
            actual.insert(
                "closes".into(),
                js(&Value::Array(text.closes.lock().unwrap().clone())),
            );
            actual.insert(
                "finalInFlight".into(),
                JsValue::Array(
                    text.final_in_flight
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|sessions| JsValue::Array(sessions.clone()))
                        .collect(),
                ),
            );
            actual.insert("inFlight".into(), JsValue::Array(in_flight));
            actual.insert("interactions".into(), JsValue::Array(interactions));
            if gateway.fail_disconnect {
                actual.insert("closeErrors".into(), js(&json!(close_errors)));
            }
            actual.insert("frames".into(), JsValue::Array(frame_values(&raw_frames)));
            actual.insert(
                "calls".into(),
                JsValue::Array(gateway.calls.lock().unwrap().clone()),
            );
            let actual = normalized(&JsValue::Object(actual), &ids);
            // Every step records the exact outbound text; only it is compared
            // semantically after removing framesText (key order is not part of
            // the parsed-object comparison).
            let expected_source = array(recorded.get("results").unwrap())[step_index]
                .as_object()
                .unwrap();
            let expected = normalized(
                &JsValue::Object(without_key(expected_source, "framesText")),
                &BTreeMap::new(),
            );
            assert_eq!(actual, expected, "{name:?} step {step_index}");
            // Byte-level comparison: insertion order, surrogate escaping and
            // number formatting all have to match TS JSON.stringify byte for
            // byte. Only this connection's masked UUID can occur in its frames.
            let expected_text = array(expected_source.get("framesText").unwrap())
                .iter()
                .map(|item| item.as_string().unwrap().to_utf8().unwrap())
                .collect::<Vec<_>>();
            let marker = format!("@connection-{}", index + 1);
            let actual_text = raw_frames
                .iter()
                .map(|raw| raw.replace(&connection.binding.connection_id, &marker))
                .collect::<Vec<_>>();
            assert_eq!(
                actual_text, expected_text,
                "{name:?} step {step_index} raw frames"
            );
        }
        assert!(submissions.is_empty());
    }
    assert_eq!(
        replayed,
        match only {
            None => 86,
            Some("-string-params") => 16,
            Some("-array-params") => 8,
            _ => panic!("unknown replay subset"),
        }
    );
}
#[test]
#[ignore = "requires PDRS_BASELINE and its installed TypeScript dependencies"]
fn connection_context_fixtures_regenerate_identically_from_ts() {
    crate::test_baseline::regenerates_identically("connection-context");
}
#[tokio::test]
async fn connection_state_frames_match_ts() {
    replay(None).await;
}
#[tokio::test]
async fn socket_close_matches_a5_trace() {
    replay(None).await;
}
#[tokio::test]
async fn non_object_string_params_match_ts() {
    replay(Some("-string-params")).await;
}
#[tokio::test]
async fn non_object_array_params_match_ts() {
    replay(Some("-array-params")).await;
}
#[test]
fn real_ts_binding_provider() {
    let recorded = crate::test_baseline::oracle(ORACLE);
    let oracle = json!({"bindingSteps":property_json(&recorded,"bindingSteps"),
                        "bindingResults":property_json(&recorded,"bindingResults")});
    assert_eq!(oracle["bindingSteps"].as_array().unwrap().len(), 27);
    assert_eq!(oracle["bindingResults"].as_array().unwrap().len(), 27);
    let bindings = BindingOwners::default();
    for (i, step) in oracle["bindingSteps"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let result: Result<Value, &str> = (|| {
            Ok(match step["op"].as_str().unwrap() {
                "reconnect" => {
                    let next = WsBinding::from_json(&step["next"])?;
                    let previous = step
                        .get("previous")
                        .filter(|value| truthy(&js(value)))
                        .map(WsBinding::from_json)
                        .transpose()?;
                    bindings.reconnect("owner", next, previous.as_ref())
                }
                "disconnect" => {
                    json!(bindings.disconnect("owner", &WsBinding::from_json(&step["binding"])?))
                }
                "current" => bindings
                    .current("owner")
                    .map(|b| b.to_json())
                    .unwrap_or(Value::Null),
                op => panic!("unknown {op}"),
            })
        })();
        let actual = result.unwrap_or_else(|error| json!({"error":error}));
        assert_eq!(actual, oracle["bindingResults"][i], "binding step {i}");
    }
}
#[test]
fn ws_binding_is_not_module_binding() {
    let module = pilotdeck_protocol::module::ModuleBinding {
        module_instance_id: "module".into(),
        connection_generation: "1".into(),
    };
    assert!(WsBinding::from_json(&json!({"connectionId":"a","generation":module.connection_generation.to_utf8().unwrap()})).is_err());
    assert!(
        WsBinding::from_json(&json!({"moduleInstanceId":"module","connectionGeneration":"1"}))
            .is_err()
    );
    let first = WsBinding::fresh();
    let second = WsBinding::fresh();
    assert_ne!(first.connection_id, second.connection_id);
    assert!(first.generation < second.generation);
}
/// An old disconnect reaches the action only after replacement was published.
/// Both the current-binding predicate and removal must use the same lock.
#[test]
fn old_disconnect_cannot_remove_replacement_across_threads() {
    let bindings = Arc::new(BindingOwners::default());
    let old = WsBinding::from_json(&json!({"connectionId":"old","generation":1})).unwrap();
    let new = WsBinding::from_json(&json!({"connectionId":"new","generation":2})).unwrap();
    bindings.reconnect("s", old.clone(), None);
    let gate = Arc::new(std::sync::Barrier::new(2));
    let worker = {
        let bindings = bindings.clone();
        let gate = gate.clone();
        let old = old.clone();
        std::thread::spawn(move || {
            gate.wait();
            gate.wait();
            assert!(!bindings.disconnect("s", &old));
        })
    };
    gate.wait();
    bindings.reconnect("s", new.clone(), Some(&old));
    gate.wait();
    worker.join().unwrap();
    assert_eq!(bindings.current("s"), Some(new));
}
#[test]
fn old_submit_cleanup_clears_session_across_threads() {
    let gateway = Arc::new(Gateway::new(false));
    gateway
        .hold
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let connection = Arc::new(GatewayConnection::new(
        Arc::new(MemoryText::default()),
        gateway.clone(),
        "fixture-token".into(),
        "test".into(),
    ));
    connection.state.lock().unwrap().authed = true;
    let gate = Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        let worker = {
            let connection = connection.clone();
            let gate = gate.clone();
            scope.spawn(move || {
            let mut old=connection.receive(r#"{"type":"request","id":"old","method":"submit_turn","params":{"sessionKey":"s"}}"#);
            let mut cx=std::task::Context::from_waker(std::task::Waker::noop());
            assert!(old.as_mut().poll(&mut cx).is_pending());
            gate.wait();gate.wait();drop(old);gate.wait();
        })
        };
        gate.wait();
        let new = connection.receive(
            r#"{"type":"request","id":"new","method":"submit_turn","params":{"sessionKey":"s"}}"#,
        );
        gate.wait();
        gate.wait();
        worker
            .join()
            .unwrap_or_else(|_| panic!("#818 submit cleanup worker thread"));
        // Snapshot under the lock, assert after it is released: a pending
        // SubmitCleanup locks the same state while unwinding, so holding the
        // guard in an assert would poison it and abort instead of reporting
        // the named failure (#818 fix-r1).
        let (cleared, retained) = {
            let state = connection.state.lock().unwrap();
            (
                state.in_flight.is_empty(),
                session_values(&state.interactions),
            )
        };
        // TS finally deletes by session even while the newer submit is pending.
        assert!(cleared, "#818 submit cleanup clears in-flight session");
        assert_eq!(
            retained,
            [js(&json!("s"))],
            "#818 submit cleanup retains interaction session"
        );
        drop(new);
        let cleared_after = connection.state.lock().unwrap().in_flight.is_empty();
        assert!(cleared_after, "#818 submit cleanup keeps in-flight cleared");
    });
}

// Fail fast on a held lock before exercising real synchronous reentry, so a
// regression produces a diagnostic instead of hanging the test process.
#[derive(Default)]
struct ReentrantText {
    owner: Mutex<Option<std::sync::Weak<GatewayConnection>>>,
    callbacks: std::sync::atomic::AtomicUsize,
}
impl ReentrantText {
    fn reenter(&self) {
        let owner = self
            .owner
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .upgrade()
            .unwrap();
        let authed = owner
            .state
            .try_lock()
            .expect("sink called with state lock held")
            .authed;
        if authed {
            drop(owner.receive(r#"{"type":"request","id":"reentrant","method":"unknown_method"}"#));
        } else {
            let hello = json!({"type":"hello","protocolVersion":GATEWAY_PROTOCOL_VERSION,"clientName":"ui","clientVersion":"test","token":"fixture-token"});
            drop(owner.receive(&hello.to_string()));
        }
        drop(owner.closed());
        self.callbacks
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
impl GatewayTextConnection for ReentrantText {
    fn send_text(&self, _: String) {
        self.reenter();
    }
    fn close(&self, _: u16, _: &str) {
        self.reenter();
    }
}
#[tokio::test]
async fn text_callbacks_can_synchronously_reenter_connection() {
    for invalid in [
        "{",
        "{}",
        r#"{"type":"hello","protocolVersion":"wrong","clientName":"ui","clientVersion":"test","token":"fixture-token"}"#,
        r#"{"type":"hello","protocolVersion":"1.2","clientName":"ui","clientVersion":"test","token":"wrong"}"#,
    ] {
        let text = Arc::new(ReentrantText::default());
        let connection = Arc::new(GatewayConnection::new(
            text.clone(),
            Arc::new(Gateway::new(false)),
            "fixture-token".into(),
            "test".into(),
        ));
        *text.owner.lock().unwrap() = Some(Arc::downgrade(&connection));
        connection.receive(invalid).await.unwrap();
        assert_eq!(text.callbacks.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    let text = Arc::new(ReentrantText::default());
    let connection = Arc::new(GatewayConnection::new(
        text.clone(),
        Arc::new(Gateway::new(false)),
        "fixture-token".into(),
        "test".into(),
    ));
    *text.owner.lock().unwrap() = Some(Arc::downgrade(&connection));
    let hello = json!({"type":"hello","protocolVersion":GATEWAY_PROTOCOL_VERSION,"clientName":"ui","clientVersion":"test","token":"fixture-token"});
    connection.receive(&hello.to_string()).await.unwrap();
    connection.notify("fixture", None);
    connection.receive("{}").await.unwrap();
    connection.receive(r#"{"type":"request","id":"running","method":"submit_turn","params":{"sessionKey":"s"}}"#).await.unwrap();
    assert_eq!(text.callbacks.load(std::sync::atomic::Ordering::SeqCst), 5);
}
#[test]
fn cancelled_receive_sends_no_invented_terminal() {
    let gateway = Arc::new(Gateway::new(false));
    gateway
        .hold
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let text = Arc::new(MemoryText::default());
    let connection =
        GatewayConnection::new(text.clone(), gateway, "fixture-token".into(), "test".into());
    connection.state.lock().unwrap().authed = true;
    let mut request = connection.receive(
        r#"{"type":"request","id":"running","method":"submit_turn","params":{"sessionKey":"s"}}"#,
    );
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(request.as_mut().poll(&mut cx).is_pending());
    drop(request);
    assert!(text.frames.lock().unwrap().is_empty());
    assert!(connection.state.lock().unwrap().in_flight.is_empty());
}

#[test]
fn session_sets_use_js_scalar_equality_and_object_identity() {
    let mut state = State::default();
    let number = SessionKey::new(js(&json!(5)));
    state.track_in_flight(&number);
    state.track_in_flight(&SessionKey::new(js(&json!(5.0))));
    state.track_in_flight(&SessionKey::new(js(&json!(true))));
    let object = SessionKey::new(js(&json!({})));
    state.track_interaction(&object);
    state.track_interaction(&object.clone());
    state.track_interaction(&SessionKey::new(js(&json!({}))));
    assert_eq!(
        session_values(&state.in_flight),
        [js(&json!(5)), js(&json!(true))]
    );
    assert_eq!(state.interactions.len(), 2);
    assert_eq!(state.sessions().len(), 4);
}

#[test]
fn sessions_preserve_in_flight_then_interaction_insertion_order() {
    // TS GatewayWsConnection.ts:49-58 unions the Sets in this order before close.
    let mut state = State::default();
    let b = SessionKey::new(js(&json!("b")));
    state.track_in_flight(&b);
    state.track_interaction(&SessionKey::new(js(&json!("a"))));
    assert_eq!(
        session_values(&state.sessions()),
        [js(&json!("b")), js(&json!("a"))]
    );
    // Real submit also puts b in interactions (TS:126-127); union deduplicates it.
    state.track_interaction(&b);
    assert_eq!(
        session_values(&state.sessions()),
        [js(&json!("b")), js(&json!("a"))]
    );
}

// The frames below are the verbatim `sent` strings replayed against the real
// baseline GatewayWsConnection.ts with Node v22 (see
// _tmp/20261022-issue-458/ts-probe-output.json). Only the per-connection UUID
// and the process-wide generation counter are placeholder-substituted; every
// other byte, including nested key order, must match.
const TS_HELLO_OK: &str = r#"{"type":"hello_ok","protocolVersion":"1.2","serverVersion":"test","serverInfo":{"name":"fixture-server","capabilities":["fixture"]},"interactionBinding":{"connectionId":"{connectionId}","generation":{generation}}}"#;
const TS_DESCRIBE_OK: &str = r#"{"type":"response","id":"1","ok":true,"result":{"name":"fixture-server","capabilities":["fixture"]}}"#;
const TS_SURROGATE_ID_OK: &str = r#"{"type":"response","id":"\ud800","ok":true,"result":{"name":"fixture-server","capabilities":["fixture"]}}"#;
const TS_UNKNOWN_SURROGATE_METHOD: &str = r#"{"type":"response","id":"1","ok":false,"error":{"code":"gateway_request_failed","message":"Unknown gateway method \ud800."}}"#;
const TS_ABORT_OK: &str = r#"{"type":"response","id":"1","ok":true,"result":{"ok":true}}"#;

fn assert_sent(text: &MemoryText, connection: &GatewayConnection, expected: &[&str]) {
    let frames = text.frames.lock().unwrap();
    assert_eq!(frames.len(), expected.len(), "sent frames: {frames:?}");
    let binding = &connection.binding;
    for (index, template) in expected.iter().enumerate() {
        let expected = template
            .replace("{connectionId}", &binding.connection_id)
            .replace("{generation}", &binding.generation().to_string());
        assert_eq!(frames[index], expected, "frame {index}");
    }
}
fn string_units(value: &JsValue) -> Vec<u16> {
    value.as_string().unwrap().units().to_vec()
}
/// Compare tracked sessions at UTF-16 code-unit level.
fn session_units(sessions: &[SessionKey]) -> Vec<Vec<u16>> {
    sessions
        .iter()
        .map(|key| string_units(key.value()))
        .collect()
}
async fn authed_connection() -> (Arc<MemoryText>, Arc<Gateway>, Arc<GatewayConnection>) {
    authed_with_mode("").await
}
async fn authed_with_mode(
    submit_mode: &str,
) -> (Arc<MemoryText>, Arc<Gateway>, Arc<GatewayConnection>) {
    let text = Arc::new(MemoryText::default());
    let mut gateway = Gateway::new(false);
    gateway.submit_mode = submit_mode.to_owned();
    let gateway = Arc::new(gateway);
    let connection = Arc::new(GatewayConnection::new(
        text.clone(),
        gateway.clone(),
        "fixture-token".into(),
        "test".into(),
    ));
    connection
        .receive(
            r#"{"type":"hello","protocolVersion":"1.2","clientName":"ui","clientVersion":"test","token":"fixture-token"}"#,
        )
        .await
        .unwrap();
    (text, gateway, connection)
}

// Ordinary ASCII request: pins the top-level type,id,ok,result insertion order
// that changed when frames stopped going through serde_json::Value.
#[tokio::test]
async fn ascii_describe_pins_ts_top_level_order() {
    let (text, _gateway, connection) = authed_connection().await;
    connection
        .receive(r#"{"type":"request","id":"1","method":"describe_server"}"#)
        .await
        .unwrap();
    assert_sent(&text, &connection, &[TS_HELLO_OK, TS_DESCRIBE_OK]);
    assert!(text.closes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn lone_surrogate_id_round_trips_in_response() {
    let (text, _gateway, connection) = authed_connection().await;
    connection
        .receive(r#"{"type":"request","id":"\ud800","method":"describe_server"}"#)
        .await
        .unwrap();
    assert_sent(&text, &connection, &[TS_HELLO_OK, TS_SURROGATE_ID_OK]);
    let sent = text.frames.lock().unwrap()[1].clone();
    let frame = parse_json(&sent).unwrap();
    assert_eq!(string_units(frame.get("id").unwrap()), [0xD800]);
    assert!(text.closes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn lone_surrogate_method_reaches_gateway_and_echoes_in_error() {
    let (text, gateway, connection) = authed_connection().await;
    connection
        .receive(r#"{"type":"request","id":"1","method":"\ud800"}"#)
        .await
        .unwrap();
    assert_sent(
        &text,
        &connection,
        &[TS_HELLO_OK, TS_UNKNOWN_SURROGATE_METHOD],
    );
    let sent = text.frames.lock().unwrap()[1].clone();
    let frame = parse_json(&sent).unwrap();
    assert_eq!(
        string_units(frame.get("error").unwrap().get("message").unwrap()),
        "Unknown gateway method "
            .encode_utf16()
            .chain([0xD800])
            .chain(".".encode_utf16())
            .collect::<Vec<_>>()
    );
    // Exactly the hello describe call: the gateway got the surrogate method,
    // it did not first collapse to a replacement character and dispatch.
    assert_eq!(gateway.calls.lock().unwrap().len(), 1);
    assert!(text.closes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn lone_surrogate_session_key_is_forwarded_unchanged() {
    let (text, gateway, connection) = authed_connection().await;
    connection
        .receive(
            r#"{"type":"request","id":"1","method":"abort_turn","params":{"sessionKey":"\ud83d"}}"#,
        )
        .await
        .unwrap();
    assert_sent(&text, &connection, &[TS_HELLO_OK, TS_ABORT_OK]);
    let calls = gateway.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    let params = calls[1].get("params").unwrap();
    assert_eq!(string_units(params.get("sessionKey").unwrap()), [0xD83D]);
    assert!(text.closes.lock().unwrap().is_empty());
}

// SessionKey equality is SameValueZero on UTF-16 code units: two separately
// parsed lone surrogates hit the same session, a different surrogate and U+FFFD
// do not. Reading the sets never goes through serde_json::Value.
#[test]
fn lone_surrogate_session_keys_compare_by_utf16_units() {
    let mut state = State::default();
    state.track_in_flight(&SessionKey::new(json_text(r#""\ud83d""#)));
    state.track_in_flight(&SessionKey::new(json_text(r#""\ud83d""#)));
    assert_eq!(session_units(&state.in_flight), vec![vec![0xD83D]]);
    state.track_interaction(&SessionKey::new(json_text(r#""\ud83e""#)));
    state.track_interaction(&SessionKey::new(json_text(r#""\ufffd""#)));
    assert_eq!(
        session_units(&state.interactions),
        vec![vec![0xD83E], vec![0xFFFD]]
    );
}

// The submit path must register the lone-surrogate session and close must
// forward it unchanged to disconnect and abort_on_close (no serde round-trip).
#[tokio::test]
async fn lone_surrogate_submit_session_is_tracked_and_forwarded_on_close() {
    let (text, gateway, connection) = authed_connection().await;
    gateway
        .hold
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let conn = connection.clone();
    let task = tokio::spawn(async move {
        conn.receive(
            r#"{"type":"request","id":"s1","method":"submit_turn","params":{"sessionKey":"\ud83d"}}"#,
        )
        .await
        .unwrap()
    });
    // Bound the wait: a regression that rejects the frame must fail this test
    // instead of leaving it parked forever on a gate the submit never reaches.
    wait_started(&gateway, "submit").await;
    {
        let state = connection.state.lock().unwrap();
        assert_eq!(session_units(&state.in_flight), vec![vec![0xD83D]]);
        assert_eq!(session_units(&state.interactions), vec![vec![0xD83D]]);
    }
    connection.closed().await.unwrap();
    let guard = gateway.submit_gates.lock().unwrap().last().unwrap().clone();
    guard.add_permits(1);
    task.await.unwrap();
    let calls = gateway.calls.lock().unwrap();
    let method = |index: usize| {
        calls[index]
            .get("method")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap()
    };
    assert_eq!(method(1), "submit_turn");
    assert_eq!(method(2), "disconnect");
    assert_eq!(method(3), "abort");
    for index in [2, 3] {
        let session = calls[index]
            .get("params")
            .unwrap()
            .get("sessionKey")
            .unwrap();
        assert_eq!(string_units(session), [0xD83D], "call {index}");
    }
    assert!(text.closes.lock().unwrap().is_empty());
}

/// The provider may yield a real `error` event instead of throwing. TS keeps
/// the last error and last completion and sends lastError as the terminal event
/// (GatewayWsConnection.ts:138-171). These frames are the exact golden bytes
/// recorded by the real TS `submit-{error,error-then-completed,
/// completed-then-error}` scenarios; only the connection UUID/generation are
/// placeholder-substituted.
#[tokio::test]
async fn submit_error_event_wins_terminal_frame_like_ts() {
    const TS_ERROR: &str = r#"{"type":"error","code":"fixture_error","message":"fixture stream failed","recoverable":true}"#;
    const TS_COMPLETED: &str = r#"{"type":"turn_completed","runId":"fixture-run"}"#;
    fn event_frame(seq: usize, is_final: bool, event: &str) -> String {
        format!(
            r#"{{"type":"event","id":"submit_turn","seq":{seq},"final":{is_final},"event":{event}}}"#
        )
    }
    let cases: [(&str, Vec<(&str, usize)>); 3] = [
        ("error", vec![("error", 0)]),
        ("error-then-completed", vec![("error", 0), ("completed", 1)]),
        ("completed-then-error", vec![("completed", 0), ("error", 1)]),
    ];
    for (mode, stream) in cases {
        let (text, _gateway, connection) = authed_with_mode(mode).await;
        connection
            .receive(
                r#"{"type":"request","id":"submit_turn","method":"submit_turn","params":{"sessionKey":"s","projectKey":"p"}}"#,
            )
            .await
            .unwrap();
        let mut expected = vec![TS_HELLO_OK.to_owned()];
        for (kind, seq) in &stream {
            expected.push(event_frame(
                *seq,
                false,
                if *kind == "error" {
                    TS_ERROR
                } else {
                    TS_COMPLETED
                },
            ));
        }
        // lastError always wins, and no duplicate terminal completion is sent.
        expected.push(event_frame(stream.len(), true, TS_ERROR));
        let expected = expected.iter().map(String::as_str).collect::<Vec<_>>();
        assert_sent(&text, &connection, &expected);
        assert!(text.closes.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn lone_surrogate_client_name_is_accepted_in_hello() {
    let text = Arc::new(MemoryText::default());
    let gateway = Arc::new(Gateway::new(false));
    let connection = Arc::new(GatewayConnection::new(
        text.clone(),
        gateway,
        "fixture-token".into(),
        "test".into(),
    ));
    connection
        .receive(
            r#"{"type":"hello","protocolVersion":"1.2","clientName":"\ud800","clientVersion":"test","token":"fixture-token"}"#,
        )
        .await
        .unwrap();
    {
        let state = connection.state.lock().unwrap();
        assert!(state.authed);
        assert_eq!(state.client_name.as_ref().unwrap().units(), [0xD800]);
    }
    assert_sent(&text, &connection, &[TS_HELLO_OK]);
    assert!(text.closes.lock().unwrap().is_empty());
}

const TS_DEEP_RESPONSE: &str = r#"{"type":"response","id":"deep-{depth}","ok":true,"result":{"name":"fixture-server","capabilities":["fixture"]}}"#;

/// Node 22 `JSON.parse` is iterative: depth 128 is not a boundary. The real TS
/// connection dispatches 200/1_000/30_000-deep params normally (see
/// `ts-depth-probe.json`); `parse_json` takes the same iterative path.
#[tokio::test]
async fn deeply_nested_valid_frames_are_dispatched_like_node_json_parse() {
    let (text, _gateway, connection) = authed_connection().await;
    let depths = [200usize, 1_000, 30_000];
    for depth in depths {
        let raw = [
            "{\"type\":\"request\",\"id\":\"deep-",
            &depth.to_string(),
            "\",\"method\":\"describe_server\",\"params\":{\"value\":",
            &"[".repeat(depth),
            "0",
            &"]".repeat(depth),
            "}}",
        ]
        .concat();
        connection.receive(&raw).await.unwrap();
    }
    let frames = text.frames.lock().unwrap().clone();
    assert_eq!(frames.len(), depths.len() + 1);
    for (index, depth) in depths.into_iter().enumerate() {
        let expected = TS_DEEP_RESPONSE.replace("{depth}", &depth.to_string());
        assert_eq!(frames[index + 1], expected, "depth {depth}");
    }
    assert!(text.closes.lock().unwrap().is_empty());
}

/// A malformed frame deeper than serde's typed recursion limit still closes
/// 4002 invalid_json, exactly like the TS catch around `JSON.parse`.
#[tokio::test]
async fn deeply_nested_malformed_frame_closes_invalid_json_like_ts() {
    let (text, _gateway, connection) = authed_connection().await;
    let raw = [
        "{\"type\":\"request\",\"id\":\"bad\",\"method\":\"describe_server\",\"params\":{\"value\":",
        &"[".repeat(1_000),
        "0",
        &"]".repeat(999),
        "}}",
    ]
    .concat();
    connection.receive(&raw).await.unwrap();
    assert_eq!(
        *text.closes.lock().unwrap(),
        vec![json!({"code":4002,"reason":"invalid_json"})]
    );
    assert_eq!(text.frames.lock().unwrap().len(), 1);
}
