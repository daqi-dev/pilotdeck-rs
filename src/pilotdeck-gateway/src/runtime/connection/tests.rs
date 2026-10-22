use super::binding::BindingOwners;
use super::*;
use pilotdeck_protocol::{JsObject, parse_json, stringify_json};
use std::collections::BTreeMap;
use tokio::sync::Semaphore;

const ORACLE: &str = include_str!("../../../tests/fixtures/connection-context/oracle.json");
#[derive(Default)]
struct MemoryText {
    frames: Mutex<Vec<Value>>,
    closes: Mutex<Vec<Value>>,
    final_in_flight: Mutex<Vec<Vec<Value>>>,
    owner: Mutex<Option<std::sync::Weak<GatewayConnection>>>,
}
impl GatewayTextConnection for MemoryText {
    fn send_text(&self, message: String) {
        let frame: Value = serde_json::from_str(&message).unwrap();
        if frame["final"] == true
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
        self.frames.lock().unwrap().push(frame);
    }
    fn close(&self, code: u16, reason: &str) {
        self.closes
            .lock()
            .unwrap()
            .push(json!({"code":code,"reason":reason}));
    }
}
fn session_values(sessions: &[SessionKey]) -> Vec<Value> {
    sessions.iter().map(|key| key.value().clone()).collect()
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
    fn describe_server(&self) -> Call<'_, Result<Value, RequestError>> {
        Box::pin(async {
            self.calls
                .lock()
                .unwrap()
                .push(js(&json!({"method":"describe_server"})));
            if self.hold_describe.load(std::sync::atomic::Ordering::SeqCst) {
                self.started.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            Ok(json!({"name":"fixture-server","capabilities":["fixture"]}))
        })
    }
    fn request(&self, method: String, params: JsValue) -> Call<'_, Result<Value, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                "permission_decide" | "elicitation_respond" => {
                    self.record(&method, params);
                    Ok(json!({"accepted":true}))
                }
                "reconnect_interaction" => {
                    self.record(&method, params.clone());
                    let next =
                        WsBinding::from_json(&property_json(&params, "nextBinding")).unwrap();
                    // The real TS provider treats a falsy previous as absent;
                    // the connection still forwards explicit null unchanged.
                    let previous_json = property_json(&params, "previousBinding");
                    let previous = Some(&previous_json)
                        .filter(|value| truthy(value))
                        .map(WsBinding::from_json)
                        .transpose()
                        .map_err(RequestError::new)?;
                    Ok(self.bindings.reconnect(
                        &property_json(&params, "sessionKey").to_string(),
                        next,
                        previous.as_ref(),
                    ))
                }
                "steer_turn" => Err(RequestError::new("plain fixture error")),
                "cancel_steer" => Err(RequestError {
                    code: "FIXTURE_ERROR".into(),
                    message: "structured fixture error".into(),
                    details: Some(json!({"nested":{"value":1}})),
                    validation: None,
                }),
                "skill_validate" => Err(RequestError {
                    code: "validation_failed".into(),
                    message: "Validation failed.".into(),
                    details: None,
                    validation: Some(
                        json!({"valid":false,"issues":[{"code":"fixture","message":"fixture validation"}]}),
                    ),
                }),
                "skill_read" => Err(RequestError {
                    code: "not_found".into(),
                    message: "fixture skill missing".into(),
                    details: None,
                    validation: None,
                }),
                _ => Err(RequestError::new(format!(
                    "Unknown gateway method {method}."
                ))),
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
                    &property_json(&params, "sessionKey").to_string(),
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
        session: Value,
        binding: WsBinding,
    ) -> Call<'_, Result<bool, RequestError>> {
        Box::pin(async move {
            self.record(
                "disconnect",
                js(&json!({"sessionKey":session,"binding":binding.to_json()})),
            );
            if self.fail_disconnect {
                return Err(RequestError::new("fixture disconnect failed"));
            }
            self.bindings.disconnect(&session.to_string(), &binding);
            Ok(self.preserve)
        })
    }
    fn abort_on_close(&self, params: Value) -> Call<'_, Result<(), RequestError>> {
        Box::pin(async move {
            self.record("abort", js(&params));
            Ok(())
        })
    }
}
fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
// Only the provider's known session/binding fields need serde JSON. Spread
// index values never cross this conversion and remain lossless UTF-16.
fn property_json(value: &JsValue, key: &str) -> Value {
    value
        .get(key)
        .and_then(|value| stringify_json(value).unwrap())
        .map(|text| serde_json::from_str(&text).unwrap())
        .unwrap_or(Value::Null)
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
                "notify" => connection.notify("fixture", Some(json!({"value":1}))),
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
                "close_error" => close_errors.push(connection.closed().await.unwrap_err().message),
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
            let mut actual = json!({"frames":*text.frames.lock().unwrap(), "closes":*text.closes.lock().unwrap(),
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
                        .filter(|value| truthy(value))
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
    let number = SessionKey::new(json!(5));
    state.track_in_flight(&number);
    state.track_in_flight(&SessionKey::new(json!(5.0)));
    state.track_in_flight(&SessionKey::new(json!(true)));
    let object = SessionKey::new(json!({}));
    state.track_interaction(&object);
    state.track_interaction(&object.clone());
    state.track_interaction(&SessionKey::new(json!({})));
    assert_eq!(session_values(&state.in_flight), [json!(5), json!(true)]);
    assert_eq!(state.interactions.len(), 2);
    assert_eq!(state.sessions().len(), 4);
}

#[test]
fn sessions_preserve_in_flight_then_interaction_insertion_order() {
    // TS GatewayWsConnection.ts:49-58 unions the Sets in this order before close.
    let mut state = State::default();
    let b = SessionKey::new(json!("b"));
    state.track_in_flight(&b);
    state.track_interaction(&SessionKey::new(json!("a")));
    assert_eq!(session_values(&state.sessions()), [json!("b"), json!("a")]);
    // Real submit also puts b in interactions (TS:126-127); union deduplicates it.
    state.track_interaction(&b);
    assert_eq!(session_values(&state.sessions()), [json!("b"), json!("a")]);
}
