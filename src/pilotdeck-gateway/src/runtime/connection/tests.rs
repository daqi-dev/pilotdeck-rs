use super::binding::BindingOwners;
use super::*;
use std::collections::BTreeMap;
use tokio::sync::Semaphore;

const ORACLE: &str = include_str!("../../../tests/fixtures/connection-context/oracle.json");
#[derive(Default)]
struct MemoryText {
    frames: Mutex<Vec<Value>>,
    closes: Mutex<Vec<Value>>,
    final_in_flight: Mutex<Vec<Vec<String>>>,
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
            let sessions = owner.state.lock().unwrap().in_flight.clone();
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
struct Gateway {
    calls: Mutex<Vec<Value>>,
    bindings: BindingOwners,
    preserve: bool,
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
            hold: std::sync::atomic::AtomicBool::new(false),
            hold_describe: std::sync::atomic::AtomicBool::new(false),
            started: Semaphore::new(0),
            release: Semaphore::new(0),
            submit_gates: Mutex::new(vec![]),
        }
    }
    fn record(&self, method: &str, params: Value) {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"method":method,"params":params}));
    }
}
impl ConnectionGateway for Gateway {
    fn describe_server(&self) -> Call<'_, Result<Value, RequestError>> {
        Box::pin(async {
            self.calls
                .lock()
                .unwrap()
                .push(json!({"method":"describe_server"}));
            if self.hold_describe.load(std::sync::atomic::Ordering::SeqCst) {
                self.started.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            Ok(json!({"name":"fixture-server","capabilities":["fixture"]}))
        })
    }
    fn request(&self, method: String, params: Value) -> Call<'_, Result<Value, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                "permission_decide" | "elicitation_respond" => {
                    self.record(&method, params);
                    Ok(json!({"accepted":true}))
                }
                "reconnect_interaction" => {
                    self.record(&method, params.clone());
                    let next = WsBinding::from_json(&params["nextBinding"]).unwrap();
                    let previous = params
                        .get("previousBinding")
                        .map(|v| WsBinding::from_json(v).unwrap());
                    Ok(self.bindings.reconnect(
                        params["sessionKey"].as_str().unwrap(),
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
        params: Value,
        emit: &'a (dyn Fn(Value) + Send + Sync),
    ) -> Call<'a, Result<(), RequestError>> {
        Box::pin(async move {
            self.record("submit_turn", params);
            if self.hold.load(std::sync::atomic::Ordering::SeqCst) {
                let release = Arc::new(Semaphore::new(0));
                self.submit_gates.lock().unwrap().push(release.clone());
                self.started.add_permits(1);
                release.acquire().await.unwrap().forget();
            }
            emit(json!({"type":"turn_completed","runId":"fixture-run"}));
            Ok(())
        })
    }
    fn disconnect(
        &self,
        session: String,
        binding: WsBinding,
    ) -> Call<'_, Result<bool, RequestError>> {
        Box::pin(async move {
            self.record(
                "disconnect",
                json!({"sessionKey":session,"binding":binding.to_json()}),
            );
            self.bindings.disconnect(&session, &binding);
            Ok(self.preserve)
        })
    }
    fn abort_on_close(&self, params: Value) -> Call<'_, Result<(), RequestError>> {
        Box::pin(async move {
            self.record("abort", params);
            Ok(())
        })
    }
}
fn mask(value: Value, ids: &BTreeMap<String, String>) -> Value {
    match value {
        Value::String(s) => ids.get(&s).map(|s| json!(s)).unwrap_or(Value::String(s)),
        Value::Array(items) => Value::Array(items.into_iter().map(|v| mask(v, ids)).collect()),
        Value::Object(items) => {
            Value::Object(items.into_iter().map(|(k, v)| (k, mask(v, ids))).collect())
        }
        value => value,
    }
}
async fn replay() {
    crate::test_baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle["sourceSha256"].as_object().unwrap().len(), 10);
    let mut ids = BTreeMap::new();
    let sequence = std::sync::atomic::AtomicU64::new(0);
    for (index, scenario) in oracle["scenarios"].as_array().unwrap().iter().enumerate() {
        let text = Arc::new(MemoryText::default());
        let gateway = Arc::new(Gateway::new(
            scenario["preserve"].as_bool().unwrap_or(false),
        ));
        let connection = GatewayConnection::with_binding(
            text.clone(),
            gateway.clone(),
            "fixture-token".into(),
            "test".into(),
            WsBinding::issue(&sequence),
        );
        ids.insert(
            connection.binding.connection_id.clone(),
            format!("@connection-{}", index + 1),
        );
        let connection = Arc::new(connection);
        *text.owner.lock().unwrap() = Some(Arc::downgrade(&connection));
        let mut held = None;
        let mut submissions = BTreeMap::new();
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
                (state.in_flight.clone(), state.interactions.clone())
            };
            let actual = mask(
                json!({"frames":*text.frames.lock().unwrap(), "closes":*text.closes.lock().unwrap(),
                "calls":*gateway.calls.lock().unwrap(), "finalInFlight":*text.final_in_flight.lock().unwrap(),
                "inFlight":in_flight,"interactions":interactions}),
                &ids,
            );
            assert_eq!(
                actual, scenario["results"][step_index],
                "{} step {}",
                scenario["name"], step_index
            );
        }
        assert!(submissions.is_empty());
    }
}
#[test]
#[ignore = "requires PDRS_BASELINE and its installed TypeScript dependencies"]
fn connection_context_fixtures_regenerate_identically_from_ts() {
    crate::test_baseline::regenerates_identically("connection-context");
}
#[tokio::test]
async fn connection_state_frames_match_ts() {
    replay().await;
}
#[tokio::test]
async fn socket_close_matches_a5_trace() {
    replay().await;
}
#[test]
fn real_ts_binding_provider() {
    crate::test_baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
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
                    let previous = step.get("previous").map(WsBinding::from_json).transpose()?;
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
        assert_eq!(connection.state.lock().unwrap().interactions, ["s"]);
        drop(new);
        assert!(connection.state.lock().unwrap().in_flight.is_empty());
    });
}
