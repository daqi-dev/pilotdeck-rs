use super::*;
use pilotdeck_protocol::{parse_json, stringify_json};
use serde_json::{Value, json};
use tokio::sync::watch;
const ORACLE: &str = include_str!("../../tests/fixtures/replacement/oracle.json");
#[derive(Default)]
struct Probe {
    events: Mutex<Vec<Value>>,
    active: Mutex<Option<JsString>>,
    failure: Mutex<Option<String>>,
    panic: Mutex<Option<(String, bool)>>,
    gates: Mutex<HashMap<String, watch::Sender<bool>>>,
}
impl Probe {
    fn call(self: &Arc<Self>, name: &str, input: JsValue) -> BoxFuture<ReplacementResult> {
        let panic = self.panic.lock().unwrap().clone();
        if panic
            .as_ref()
            .is_some_and(|(port, sync)| port == name && *sync)
        {
            panic!("{name} failed");
        }
        self.events
            .lock()
            .unwrap()
            .push(json!({"call": name, "input": wire(&input)}));
        let gate = self
            .gates
            .lock()
            .unwrap()
            .get(name)
            .map(|gate| gate.subscribe());
        let (probe, name) = (self.clone(), name.to_owned());
        Box::pin(async move {
            if let Some(mut gate) = gate {
                let _ = gate.wait_for(|open| *open).await;
            }
            let panic = probe.panic.lock().unwrap().clone();
            if panic.as_ref().is_some_and(|(port, _)| port == &name) {
                panic!("{name} failed");
            }
            if probe.failure.lock().unwrap().as_ref() == Some(&name) {
                return Err(ReplacementError::new(format!("{name} failed")));
            }
            Ok(js(match name.as_str() {
                "replace" => {
                    json!({"transactionId": format!("tx-{}", text(&string(&input, "sessionKey"))), "persisted": true})
                }
                "fork" => json!({"forked": wire(&input)}),
                "finalize" => {
                    json!({"finalized": wire(input.get("action").unwrap()), "transactionId": wire(input.get("transactionId").unwrap())})
                }
                _ => Value::Null,
            }))
        })
    }
    fn coordinator(self: &Arc<Self>, config: &str) -> ReplacementCoordinator {
        let operation = |name: &'static str| -> StorageOperation {
            let probe = self.clone();
            Arc::new(move |input| probe.call(name, input))
        };
        let storage = ReplacementStorage {
            fork_session: (config != "none").then(|| operation("fork")),
            replace_last_turn: (config != "none").then(|| operation("replace")),
            finalize_last_turn_replacement: (config == "all").then(|| operation("finalize")),
        };
        let probe = self.clone();
        ReplacementCoordinator::new(
            storage,
            Arc::new(self.clone()),
            10,
            Some(Arc::new(move |warn, error| {
                probe
                    .events
                    .lock()
                    .unwrap()
                    .push(json!({"warn": warn, "error": error.message}));
            })),
        )
    }
}
impl ReplacementSession for Arc<Probe> {
    fn active_turn_run_id(&self, _: &JsString) -> Option<JsString> {
        let panic = self.panic.lock().unwrap().clone();
        assert!(
            !panic.is_some_and(|(name, _)| name == "active"),
            "active failed"
        );
        self.active.lock().unwrap().clone()
    }
    fn has_active_turn(&self, _: &JsString) -> bool {
        let panic = self.panic.lock().unwrap().clone();
        assert!(
            !panic.is_some_and(|(name, _)| name == "hasActive"),
            "hasActive failed"
        );
        self.active.lock().unwrap().is_some()
    }
    fn abort_expected_turn(
        &self,
        key: &JsString,
        run: &JsString,
    ) -> BoxFuture<Result<(), ReplacementError>> {
        let operation = self.call(
            "abort",
            js(json!({"sessionKey": text(key), "runId": text(run)})),
        );
        Box::pin(async move { operation.await.map(|_| ()) })
    }
    fn close_session(&self, key: &JsString) -> BoxFuture<Result<(), ReplacementError>> {
        let operation = self.call("close", js(json!({"sessionKey": text(key)})));
        Box::pin(async move { operation.await.map(|_| ()) })
    }
}
fn text(value: &JsString) -> String {
    value.to_utf8().unwrap()
}
fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
fn wire(value: &JsValue) -> Value {
    stringify_json(value)
        .unwrap()
        .map_or(Value::Null, |json| serde_json::from_str(&json).unwrap())
}
fn result(result: ReplacementResult) -> Value {
    match result {
        Ok(value) => json!({"ok": wire(&value)}),
        Err(error) => json!({"error": error.code, "message": error.message,
            "details": error.details.as_deref().map(wire)}),
    }
}
async fn settle() {
    for _ in 0..80 {
        tokio::task::yield_now().await;
    }
}
async fn replay(name: &str) {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let oracle = wire(&oracle);
    let case = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap();
    let probe = Arc::new(Probe::default());
    let mut coordinator = probe.coordinator("all");
    let mut values = Vec::new();
    let mut waiting = HashMap::new();
    let key = JsString::from("s");
    for step in case["steps"].as_array().unwrap() {
        let op = step["op"].as_str().unwrap();
        let run: JsString = step["run"].as_str().unwrap_or("").into();
        let mut operation = None;
        match op {
            "config" => { coordinator.dispose(); coordinator = probe.coordinator(step["name"].as_str().unwrap()); }
            "active" => *probe.active.lock().unwrap() = step["run"].as_str().map(Into::into),
            "fail" => *probe.failure.lock().unwrap() = step["name"].as_str().map(Into::into),
            "panic" => *probe.panic.lock().unwrap() = step["name"].as_str().map(|name| (name.into(), step["sync"].as_bool().unwrap())),
            "bypassGate" => { let name = step["name"].as_str().unwrap(); let mut gates = probe.gates.lock().unwrap(); let old = gates.remove(name).unwrap(); gates.insert(format!("old-{name}"), old); }
            "gate" => { probe.gates.lock().unwrap().insert(step["name"].as_str().unwrap().into(), watch::channel(false).0); }
            "ungate" => { probe.gates.lock().unwrap().remove(step["name"].as_str().unwrap()).unwrap().send_replace(true); }
            "replace" => operation = Some(coordinator.replace_last_turn(js(step["input"].clone()))),
            "finalize" => operation = Some(coordinator.finalize_last_turn_replacement(js(step["input"].clone()))),
            "fork" => operation = Some(coordinator.fork_session(js(step["input"].clone()))),
            "commit" => { let commit = coordinator.commit_accepted_input(&key, &run); operation = Some(Box::pin(async move { commit.await; Ok(JsValue::Null) })); }
            "await" => {
                let task: tokio::task::JoinHandle<ReplacementResult> = waiting.remove(step["id"].as_str().unwrap()).unwrap();
                values.push(json!({"op": op, "id": step["id"], "result": result(task.await.unwrap())}));
            }
            "claim" => {
                let claim = match coordinator.claim_for_submit(&key, &run) {
                    SubmitClaim::None => "none", SubmitClaim::Claimed => "claimed", SubmitClaim::Conflict => "conflict",
                };
                values.push(json!({"op": op, "value": claim}));
            }
            "release" => coordinator.release_submit_claim(&key, &run),
            "reserve" => values.push(json!({"op": op, "result": result(coordinator.reserve_transcript_write(&key, "edit").map(|_| JsValue::Null))})),
            "releaseWrite" => coordinator.release_transcript_write(&key),
            "has" => values.push(json!({"op": op, "value": coordinator.has_transcript_write_reservation(&key)})),
            "dispose" => coordinator.dispose(),
            "expire" => { tokio::time::advance(Duration::from_millis(10)).await; }
            _ => panic!("unknown operation {op}"),
        }
        if let Some(operation) = operation {
            if let Some(id) = step["start"].as_str() {
                waiting.insert(id.to_owned(), tokio::spawn(operation));
            } else {
                values.push(json!({"op": op, "result": result(operation.await)}));
            }
        }
        settle().await;
        assert_eq!(
            values.as_slice(),
            &case["values"].as_array().unwrap()[..values.len()],
            "{name}: results differ from TS after {op}"
        );
    }
    coordinator.dispose();
    assert!(waiting.is_empty(), "unjoined operations");
    assert_eq!(
        json!(values),
        case["values"],
        "{name}: results differ from TS"
    );
    assert_eq!(
        json!(*probe.events.lock().unwrap()),
        case["events"],
        "{name}: call order differs from TS"
    );
}
macro_rules! cases {
    ($($name:ident),* $(,)?) => { $(
        #[tokio::test(start_paused = true)]
        async fn $name() { replay(stringify!($name)).await; }
    )* };
}
cases!(
    replace_abort_close_commit,
    claim_release_rollback,
    concurrent_replace_and_writer,
    different_sessions_independent,
    dispose_during_replace,
    replace_failure_abort,
    replace_failure_close,
    replace_failure_replace,
    active_conflict_and_empty,
    invalid_replacement_ids,
    writer_reservations,
    transaction_identity,
    finalize_active_retry,
    rollback_failure_close,
    rollback_failure_finalize,
    finalize_blocks_claim,
    timeout_rollback,
    timeout_active_retry,
    timeout_failure_retry,
    timeout_close_failure,
    claim_cancels_timeout,
    release_rearms_timeout,
    dispose_cancels_timeout,
    missing_ports,
    fork_forward_and_failure,
    stale_finalize_failure_preserves_new_claim,
    stale_timeout_failure_preserves_new_claim,
    optional_project
);

#[tokio::test(start_paused = true)]
async fn stale_timer_cannot_consume_rearmed_transaction() {
    let probe = Arc::new(Probe::default());
    let coordinator = probe.coordinator("all");
    coordinator
        .replace_last_turn(js(json!({"sessionKey":"s", "replacementTurnId":"new"})))
        .await
        .unwrap();
    let key = JsString::from("s");
    let token = coordinator.0.state.lock().unwrap().pending[&key]
        .timer
        .as_ref()
        .unwrap()
        .0
        .clone();
    assert_eq!(
        coordinator.claim_for_submit(&key, &"new".into()),
        SubmitClaim::Claimed
    );
    coordinator.release_submit_claim(&key, &"new".into());
    // Model an old timer already executing when abort() was called on another
    // executor thread. TS's synchronous prefixes cannot interleave this way.
    coordinator.rollback_expired(key.clone(), "tx-s".into(), token);
    settle().await;
    assert_eq!(
        coordinator.claim_for_submit(&key, &"new".into()),
        SubmitClaim::Claimed
    );
    assert!(
        !probe
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event["call"] == "finalize")
    );
    coordinator.dispose();
}

#[tokio::test(start_paused = true)]
async fn dropped_caller_does_not_strand_reservation() {
    let probe = Arc::new(Probe::default());
    let gate = watch::channel(false).0;
    probe
        .gates
        .lock()
        .unwrap()
        .insert("close".into(), gate.clone());
    let coordinator = probe.coordinator("all");
    let replacing =
        coordinator.replace_last_turn(js(json!({"sessionKey":"s", "replacementTurnId":"new"})));
    assert!(coordinator.has_transcript_write_reservation(&"s".into()));
    drop(replacing);
    gate.send_replace(true);
    settle().await;
    assert_eq!(
        coordinator.claim_for_submit(&"s".into(), &"new".into()),
        SubmitClaim::Claimed
    );
    assert!(!coordinator.has_transcript_write_reservation(&"s".into()));
    coordinator.dispose();
}

#[test]
#[ignore = "requires PDRS_BASELINE and Node v22 with baseline dependencies"]
fn replacement_fixture_regenerates_identically() {
    crate::test_baseline::regenerates_identically("replacement");
}

cases!(
    panic_active_getter,
    panic_finalize_getter,
    abandoned_submit_claim,
    stale_finalize_success_clears_new_pending,
    stale_commit_success_clears_new_pending,
    stale_timeout_success_clears_new_pending,
    stale_replace_failure_clears_new_pending,
    stale_replace_success_overwrites_new_pending,
    panic_replace_abort_sync,
    panic_replace_close_sync,
    panic_replace_replace_sync,
    panic_rollback_close_sync,
    panic_rollback_finalize_sync,
    panic_commit_sync,
    panic_timeout_close_sync,
    panic_timeout_finalize_sync,
    panic_fork_sync,
    panic_replace_abort_poll,
    panic_replace_close_poll,
    panic_replace_replace_poll,
    panic_rollback_close_poll,
    panic_rollback_finalize_poll,
    panic_commit_poll,
    panic_timeout_close_poll,
    panic_timeout_finalize_poll,
    panic_fork_poll,
);

// Tokio cancellation and executor panics have no JS future-drop equivalent.
// Exercise these Rust ownership boundaries separately from the TS replays.
async fn panic_after_dispose_with_dropped_waiter(kind: &str) {
    let probe = Arc::new(Probe::default());
    let coordinator = probe.coordinator("all");
    let key = JsString::from("s");
    let request = || js(json!({"sessionKey":"s", "replacementTurnId":"new"}));
    if kind != "replace" {
        coordinator.replace_last_turn(request()).await.unwrap();
    }
    let name = if kind == "replace" {
        "replace"
    } else {
        "finalize"
    };
    let gate = watch::channel(false).0;
    probe
        .gates
        .lock()
        .unwrap()
        .insert(name.into(), gate.clone());
    *probe.panic.lock().unwrap() = Some((name.into(), false));
    match kind {
        "replace" => drop(coordinator.replace_last_turn(request())),
        "finalize" => drop(coordinator.finalize_last_turn_replacement(js(json!({
            "sessionKey":"s", "transactionId":"tx-s", "action":"commit"
        })))),
        "commit" => {
            assert_eq!(
                coordinator.claim_for_submit(&key, &"new".into()),
                SubmitClaim::Claimed
            );
            drop(coordinator.commit_accepted_input(&key, &"new".into()));
        }
        _ => unreachable!(),
    }
    settle().await;
    gate.send_replace(true);
    settle().await;
    {
        let state = coordinator.0.state.lock().unwrap();
        assert!(state.reservations.is_empty());
        if kind == "finalize" {
            assert!(state.pending[&key].phase == Phase::Prepared);
            assert!(state.pending[&key].timer.is_some());
        } else {
            assert!(state.pending.is_empty());
        }
    }
    coordinator.dispose();
    *probe.panic.lock().unwrap() = None;
    coordinator.reserve_transcript_write(&key, "edit").unwrap();
    coordinator.release_transcript_write(&key);
    coordinator.replace_last_turn(request()).await.unwrap();
    coordinator.dispose();
    // With a dropped waiter a task panic has no JoinHandle observer; a normal
    // error continuation is visible through the accepted-input warning.
    if kind == "commit" {
        assert!(
            probe
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event["warn"]
                    == "[pilotdeck] failed to remove accepted replacement backup:")
        );
    }
}
#[tokio::test(start_paused = true)]
async fn panic_disposed_dropped_replace() {
    panic_after_dispose_with_dropped_waiter("replace").await;
}
#[tokio::test(start_paused = true)]
async fn panic_disposed_dropped_finalize() {
    panic_after_dispose_with_dropped_waiter("finalize").await;
}
#[tokio::test(start_paused = true)]
async fn panic_disposed_dropped_commit() {
    panic_after_dispose_with_dropped_waiter("commit").await;
}

#[tokio::test(start_paused = true)]
async fn panic_timeout_getter_rearms_without_poisoning_state() {
    let probe = Arc::new(Probe::default());
    let coordinator = probe.coordinator("all");
    let key = JsString::from("s");
    coordinator
        .replace_last_turn(js(json!({"sessionKey":"s", "replacementTurnId":"new"})))
        .await
        .unwrap();
    *probe.panic.lock().unwrap() = Some(("hasActive".into(), true));
    tokio::time::advance(Duration::from_millis(10)).await;
    settle().await;
    *probe.panic.lock().unwrap() = None;
    assert_eq!(
        coordinator.claim_for_submit(&key, &"new".into()),
        SubmitClaim::Claimed
    );
    coordinator.release_submit_claim(&key, &"new".into());
    tokio::time::advance(Duration::from_millis(10)).await;
    settle().await;
    coordinator.reserve_transcript_write(&key, "edit").unwrap();
    coordinator.release_transcript_write(&key);
    assert!(
        probe
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event["warn"]
                == "[pilotdeck] failed to roll back expired replacement transaction:")
    );
    coordinator.dispose();
}

#[tokio::test]
async fn unexpected_task_panic_returns_error_to_waiter() {
    let error = running(async {
        panic!("unexpected task panic");
    })
    .await
    .unwrap_err();
    assert!(error.message.contains("unexpected task panic"));
}
