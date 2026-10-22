use super::binding::BindingOwners;
use super::*;
use pilotdeck_protocol::{JsObject, parse_json, stringify_json};
use serde_json::json;
use std::collections::BTreeMap;
use tokio::sync::Semaphore;

const ORACLE: &str = include_str!("../../../tests/fixtures/connection-context/oracle.json");
#[derive(Default)]
struct MemoryText {
    // Raw text: an outbound id may contain a lone surrogate escape, which
    // serde_json::Value cannot represent.
    frames: Mutex<Vec<String>>,
    closes: Mutex<Vec<Value>>,
    final_in_flight: Mutex<Vec<Vec<Value>>>,
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
fn session_values(sessions: &[SessionKey]) -> Vec<Value> {
    sessions
        .iter()
        .map(|key| serde_json::from_str(&stringify_json(key.value()).unwrap().unwrap()).unwrap())
        .collect()
}
fn frame_values(frames: &[String]) -> Vec<Value> {
    frames
        .iter()
        .map(|raw| serde_json::from_str(raw).unwrap())
        .collect()
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
                    self.record("abort_turn", params);
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
                    Ok(js(&self.bindings.reconnect(
                        &property_text(&params, "sessionKey"),
                        next,
                        previous.as_ref(),
                    )))
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
                    validation: Some(Box::new(js(
                        &json!({"valid":false,"issues":[{"code":"fixture","message":"fixture validation"}]}),
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
        emit: &'a (dyn Fn(Value) + Send + Sync),
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
            if self.permission_event || self.submit_mode == "permission" {
                emit(json!({"type":"permission_request","requestId":"fixture-permission"}));
            }
            if self.submit_mode == "throw" {
                return Err(RequestError::new("fixture submit failed"));
            }
            if self.submit_mode == "empty" {
                return Ok(());
            }
            if self.hold.load(std::sync::atomic::Ordering::SeqCst) {
                let release = Arc::new(Semaphore::new(0));
                self.submit_gates.lock().unwrap().push(release.clone());
                self.started.add_permits(1);
                release.acquire().await.unwrap().forget();
            }
            emit(json!({"type":"turn_completed","runId":"fixture-run"}));
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
async fn replay(only: Option<&str>) {
    let oracle = crate::test_baseline::oracle(ORACLE);
    assert_eq!(
        oracle
            .get("sourceSha256")
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        10
    );
    assert_eq!(array(oracle.get("scenarios").unwrap()).len(), 75);
    let mut ids = BTreeMap::new();
    let sequence = std::sync::atomic::AtomicU64::new(0);
    let mut replayed = 0;
    for (index, recorded) in array(oracle.get("scenarios").unwrap()).iter().enumerate() {
        // Control metadata has ordinary strings; observations may contain lone
        // surrogate code units and must never go through serde_json::Value.
        let mut control = JsObject::new();
        for (key, value) in recorded.as_object().unwrap().entries() {
            if !key.equals_str("results") {
                control.insert(key.clone(), value.clone());
            }
        }
        let scenario: Value =
            serde_json::from_str(&stringify_json(&JsValue::Object(control)).unwrap().unwrap())
                .unwrap();
        let binding = WsBinding::issue(&sequence);
        if only.is_some_and(|suffix| !scenario["name"].as_str().unwrap().ends_with(suffix)) {
            continue;
        }
        replayed += 1;
        let text = Arc::new(MemoryText::default());
        let mut gateway = Gateway::new(scenario["preserve"].as_bool().unwrap_or(false));
        gateway.permission_event = scenario["permissionEvent"].as_bool().unwrap_or(false);
        gateway.submit_mode = scenario["submitMode"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        gateway.fail_disconnect = scenario["failDisconnect"].as_bool().unwrap_or(false);
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
        for (step_index, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
            match step["op"].as_str().unwrap() {
                "message" => connection
                    .receive(step["raw"].as_str().unwrap())
                    .await
                    .unwrap(),
                "notify" => connection.notify("fixture", Some(js(&json!({"value":1})))),
                "start_hello" => {
                    gateway
                        .hold_describe
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    let conn = connection.clone();
                    let raw = step["frame"].to_string();
                    held = Some(tokio::spawn(
                        async move { conn.receive(&raw).await.unwrap() },
                    ));
                    gateway.started.acquire().await.unwrap().forget();
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
                    let raw = step["frame"].to_string();
                    let task = tokio::spawn(async move { conn.receive(&raw).await.unwrap() });
                    gateway.started.acquire().await.unwrap().forget();
                    let release = gateway.submit_gates.lock().unwrap().last().unwrap().clone();
                    let slot = step["slot"].as_str().unwrap_or("default");
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
                    let slot = step["slot"].as_str().unwrap_or("default");
                    let (release, task) = submissions.remove(slot).unwrap();
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
            let frames = frame_values(&text.frames.lock().unwrap());
            let mut actual = json!({"frames":frames, "closes":*text.closes.lock().unwrap(),
                "finalInFlight":*text.final_in_flight.lock().unwrap(),
                "inFlight":in_flight,"interactions":interactions});
            if gateway.fail_disconnect {
                actual["closeErrors"] = json!(close_errors);
            }
            let mut actual = js(&actual).as_object().unwrap().clone();
            actual.insert(
                "calls".into(),
                JsValue::Array(gateway.calls.lock().unwrap().clone()),
            );
            let actual = normalized(&JsValue::Object(actual), &ids);
            let expected = normalized(
                &array(recorded.get("results").unwrap())[step_index],
                &BTreeMap::new(),
            );
            assert_eq!(actual, expected, "{} step {}", scenario["name"], step_index);
        }
        assert!(submissions.is_empty());
    }
    assert_eq!(
        replayed,
        match only {
            None => 75,
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
        worker.join().unwrap();
        // TS finally deletes by session even while the newer submit is pending.
        assert!(connection.state.lock().unwrap().in_flight.is_empty());
        assert_eq!(
            session_values(&connection.state.lock().unwrap().interactions),
            [json!("s")]
        );
        drop(new);
        assert!(connection.state.lock().unwrap().in_flight.is_empty());
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
    assert_eq!(session_values(&state.in_flight), [json!(5), json!(true)]);
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
    assert_eq!(session_values(&state.sessions()), [json!("b"), json!("a")]);
    // Real submit also puts b in interactions (TS:126-127); union deduplicates it.
    state.track_interaction(&b);
    assert_eq!(session_values(&state.sessions()), [json!("b"), json!("a")]);
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
/// Compare tracked sessions at UTF-16 code-unit level: the replay helper
/// `session_values` goes through serde_json and would panic on lone surrogates.
fn session_units(sessions: &[SessionKey]) -> Vec<Vec<u16>> {
    sessions
        .iter()
        .map(|key| string_units(key.value()))
        .collect()
}
async fn authed_connection() -> (Arc<MemoryText>, Arc<Gateway>, Arc<GatewayConnection>) {
    let text = Arc::new(MemoryText::default());
    let gateway = Arc::new(Gateway::new(false));
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
    gateway.started.acquire().await.unwrap().forget();
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
