#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::{
    host::{
        HostError, HostFuture, HostHandler, ModuleError,
        modules::{
            ModuleRouter,
            tool_context::{HostToolCheckpoint, SteerAuthorizations},
            turn::{Handler, Ports},
        },
    },
    runtime::steer::*,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Barrier, Notify};

fn obj<const N: usize>(fields: [(&str, JsValue); N]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    object
}
fn val<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    JsValue::Object(obj(fields))
}
fn field(value: &JsValue, key: &str) -> JsValue {
    value.get(key).cloned().unwrap_or(JsValue::Undefined)
}
fn string(value: &JsValue) -> JsString {
    value.as_string().unwrap().clone()
}
fn text(value: &JsValue) -> String {
    string(value).to_utf8().unwrap()
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("not array: {value:?}"),
    }
}
fn list(messages: Vec<SteerMessage>) -> JsValue {
    JsValue::Array(
        messages
            .iter()
            .map(|m| JsValue::Object(m.to_object()))
            .collect(),
    )
}
fn drained(result: DrainResult) -> JsValue {
    val([
        ("messages", list(result.messages)),
        ("closed", JsValue::Bool(result.closed)),
    ])
}
fn steer(value: &JsValue) -> SteerMessage {
    SteerMessage {
        item_id: string(&field(value, "itemId")),
        message: field(value, "message"),
        allowed_read_files: value
            .get("allowedReadFiles")
            .map(|v| array(v).iter().map(string).collect()),
    }
}
fn message(id: &str) -> SteerMessage {
    SteerMessage {
        item_id: id.into(),
        message: val([
            ("role", "user".into()),
            (
                "content",
                JsValue::Array(vec![val([("type", "text".into()), ("text", id.into())])]),
            ),
            ("metadata", val([("queueItemId", id.into())])),
        ]),
        allowed_read_files: Some(vec![format!("/{id}").into()]),
    }
}
fn equal(actual: &JsValue, expected: &JsValue, label: &str) {
    // JSON normalization still omits undefined fields, but key order is part of
    // the contract: recorded mutations are spread into session JSONL entries.
    let actual = stringify_json(actual).unwrap().unwrap();
    let expected = stringify_json(expected).unwrap().unwrap();
    assert_eq!(
        actual.as_bytes(),
        expected.as_bytes(),
        "{label}\nactual={actual}\nexpected={expected}"
    );
}
fn outcome(result: Result<JsValue, HostError>) -> JsValue {
    match result {
        Ok(value) => val([("ok", JsValue::Bool(true)), ("value", value)]),
        Err(error) => val([
            ("ok", JsValue::Bool(false)),
            (
                "error",
                JsValue::String(
                    error
                        .module_error
                        .map(|e| e.message)
                        .unwrap_or_else(|| error.message.into()),
                ),
            ),
        ]),
    }
}
fn map<T: Send + 'static>(
    future: HostFuture<T>,
    convert: impl FnOnce(T) -> JsValue + Send + 'static,
) -> HostFuture<JsValue> {
    Box::pin(async move { future.await.map(convert) })
}
fn success() -> JsValue {
    outcome(Ok(JsValue::Null))
}

#[derive(Clone)]
struct Gate {
    kind: Option<JsString>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
#[derive(Default)]
struct Recording {
    mutations: Vec<JsValue>,
    gate: Option<Gate>,
    failure: Option<JsString>,
}
#[derive(Default)]
struct Recorder(Mutex<Recording>);
impl Recorder {
    fn block(&self, kind: JsString) -> Gate {
        let gate = Gate {
            kind: Some(kind),
            entered: Arc::default(),
            release: Arc::default(),
        };
        self.0.lock().unwrap().gate = Some(gate.clone());
        gate
    }
    fn mutations(&self) -> JsValue {
        JsValue::Array(self.0.lock().unwrap().mutations.clone())
    }
}
impl MutationRecorder for Recorder {
    fn record(&self, turn_id: JsString, mutation: JsObject) -> HostFuture<()> {
        let (fail, gate) = {
            let mut state = self.0.lock().unwrap();
            state.mutations.push(val([
                ("turn", JsValue::String(turn_id)),
                ("mutation", JsValue::Object(mutation.clone())),
            ]));
            let kind = mutation
                .get("mutation")
                .and_then(JsValue::as_string)
                .unwrap();
            let fail = state.failure.as_ref() == Some(kind);
            if fail {
                state.failure = None;
            }
            let gate = state
                .gate
                .as_mut()
                .filter(|g| g.kind.as_ref() == Some(kind))
                .map(|g| {
                    g.kind = None;
                    g.clone()
                });
            (fail, gate)
        };
        Box::pin(async move {
            if fail {
                return Err(ModuleError::new("fixture storage failure").into());
            }
            if let Some(gate) = gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            Ok(())
        })
    }
}
fn invoke(mailbox: &SteerMailbox, step: &JsValue) -> HostFuture<JsValue> {
    let turn = string(&field(step, "turn"));
    let id = step.get("id").map(string).unwrap_or_default();
    let value = match text(&field(step, "op")).as_str() {
        "start" => {
            mailbox.start(turn);
            JsValue::Null
        }
        "enqueue" => {
            return map(mailbox.enqueue(turn, steer(&field(step, "input"))), |r| {
                let mut result = obj([("accepted", JsValue::Bool(r.accepted))]);
                if let Some(reason) = r.reason {
                    result.insert("reason".into(), reason.as_str().into());
                }
                JsValue::Object(result)
            });
        }
        "cancel" => {
            return map(mailbox.cancel(turn, id), |r| {
                let mut result = obj([("cancelled", JsValue::Bool(r.cancelled))]);
                if let Some(reason) = r.reason {
                    result.insert("reason".into(), reason.as_str().into());
                }
                JsValue::Object(result)
            });
        }
        "claim" => return map(mailbox.claim(turn, id), |()| JsValue::Null),
        "ack" => {
            mailbox.ack(&turn, &id);
            JsValue::Null
        }
        "drain" => list(mailbox.drain(&turn)),
        "drainOrClose" => drained(mailbox.drain_or_close(&turn)),
        "close" => return map(mailbox.close(turn), list),
        "finish" => list(mailbox.finish(&turn)),
        op => panic!("unhandled operation: {op}"),
    };
    Box::pin(async move { Ok(value) })
}

#[tokio::test]
async fn real_ts_mailbox_sequences_and_interleavings() {
    let oracle = baseline::oracle(include_str!("fixtures/steer/oracle.json"));
    let cases = field(&oracle, "mailboxCases");
    assert_eq!(array(&cases).len(), 15);
    let mut steps_run = 0;
    for case in array(&cases) {
        let recorder = Arc::new(Recorder::default());
        let mailbox = SteerMailbox::new(Some(recorder.clone()));
        let mut pending: HashMap<String, HostFuture<JsValue>> = HashMap::new();
        let mut gate: Option<Gate> = None;
        let mut results = Vec::new();
        for step in array(&field(case, "steps")) {
            steps_run += 1;
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                match text(&field(step, "op")).as_str() {
                    "block" => {
                        gate = Some(recorder.block(string(&field(step, "mutation"))));
                    }
                    "wait" => gate.as_ref().unwrap().entered.notified().await,
                    "release" => gate.as_ref().unwrap().release.notify_one(),
                    "fail" => {
                        recorder.0.lock().unwrap().failure = Some(string(&field(step, "mutation")));
                    }
                    "begin" => {
                        assert!(
                            pending
                                .insert(
                                    text(&field(step, "key")),
                                    invoke(&mailbox, &field(step, "action"))
                                )
                                .is_none()
                        );
                    }
                    "join" => {
                        return outcome(pending.remove(&text(&field(step, "key"))).unwrap().await);
                    }
                    _ => return outcome(invoke(&mailbox, step).await),
                }
                success()
            })
            .await
            .unwrap_or_else(|_| panic!("timeout {case:?} at {step:?}"));
            results.push(result);
        }
        assert!(pending.is_empty());
        equal(
            &val([
                ("results", JsValue::Array(results)),
                ("mutations", recorder.mutations()),
            ]),
            &field(case, "expected"),
            &text(&field(case, "name")),
        );
    }
    assert_eq!(steps_run, 192);
}

#[tokio::test]
async fn real_ts_turn_dispatcher_authorizations() {
    let oracle = baseline::oracle(include_str!("fixtures/steer/oracle.json"));
    let cases = field(&oracle, "dispatcherCases");
    assert_eq!(array(&cases).len(), 18);
    let mut steps_run = 0;
    for case in array(&cases) {
        let current = Arc::new(Mutex::new(JsValue::Undefined));
        let trace = Arc::new(Mutex::new(Vec::new()));
        let callback = {
            let current = current.clone();
            let trace = trace.clone();
            move |kind: &str| -> Result<JsObject, HostError> {
                trace.lock().unwrap().push(JsValue::from(kind));
                let step = current.lock().unwrap();
                if let Some(error) = step.get("failure") {
                    return Err(ModuleError::new(string(error)).into());
                }
                Ok(field(&step, "result").as_object().unwrap().clone())
            }
        };
        let callback = Arc::new(callback);
        let mut ports = Ports::default();
        for port in array(&field(case, "ports")) {
            let callback = callback.clone();
            if text(port) == "drain" {
                ports.drain_steer = Some(Arc::new(move || {
                    let result = callback("drain").map(|v| {
                        array(v.get("messages").unwrap())
                            .iter()
                            .map(|m| m.as_object().unwrap().clone())
                            .collect()
                    });
                    Box::pin(async move { result })
                }));
            } else {
                ports.drain_or_close_steer = Some(Arc::new(move || {
                    let result = callback("close");
                    Box::pin(async move { result })
                }));
            }
        }
        let checkpoint = HostToolCheckpoint::new(None, &["seed".into()]).unwrap();
        let handler = Handler::new(ports, SteerAuthorizations::new(checkpoint.clone()));
        let methods = handler.methods().map_or(JsValue::Null, |m| {
            JsValue::Array(m.into_iter().map(JsValue::from).collect())
        });
        let router = ModuleRouter {
            turn: handler.clone(),
            ..ModuleRouter::default()
        };
        let mut results = Vec::new();
        let mut grants = Vec::new();
        for step in array(&field(case, "steps")) {
            steps_run += 1;
            *current.lock().unwrap() = step.clone();
            let result = if text(&field(step, "kind")) == "apply" {
                handler.apply_steer_authorization(&string(&field(step, "id")));
                Ok(JsValue::Null)
            } else {
                router
                    .module_call(field(step, "call"))
                    .await
                    .map(JsValue::Object)
            };
            results.push(outcome(result));
            grants.push(JsValue::Array(
                checkpoint
                    .tool_context_state()
                    .allowed_read_files
                    .into_iter()
                    .map(JsValue::String)
                    .collect(),
            ));
        }
        router.dispose().await.unwrap();
        equal(
            &val([
                ("results", JsValue::Array(results)),
                ("grants", JsValue::Array(grants)),
                ("trace", JsValue::Array(trace.lock().unwrap().clone())),
                ("methods", methods),
            ]),
            &field(case, "expected"),
            &text(&field(case, "name")),
        );
    }
    assert_eq!(steps_run, 35);
}

struct ClaimTrace {
    trace: Arc<Mutex<Vec<JsValue>>>,
    fail: bool,
}
impl MutationRecorder for ClaimTrace {
    fn record(&self, _turn: JsString, mutation: JsObject) -> HostFuture<()> {
        let claim = mutation
            .get("mutation")
            .unwrap()
            .as_string()
            .unwrap()
            .equals_str("claim");
        if claim {
            self.trace.lock().unwrap().push(JsValue::Array(vec![
                "claim".into(),
                mutation.get("itemId").unwrap().clone(),
            ]));
        }
        let fail = claim && self.fail;
        Box::pin(async move {
            if fail {
                Err(ModuleError::new("claim failed").into())
            } else {
                Ok(())
            }
        })
    }
}
#[tokio::test]
async fn real_ts_turn_runner_tracking_and_durable_order() {
    let oracle = baseline::oracle(include_str!("fixtures/steer/oracle.json"));
    let cases = field(&oracle, "runnerCases");
    assert_eq!(array(&cases).len(), 3);
    for case in array(&cases) {
        let mode = text(&field(case, "mode"));
        let trace = Arc::new(Mutex::new(Vec::new()));
        let mailbox = SteerMailbox::new(Some(Arc::new(ClaimTrace {
            trace: trace.clone(),
            fail: mode == "claim-fails",
        })));
        mailbox.start("t".into());
        let initial =
            parse_json(r#"{"role":"user","content":[{"type":"text","text":"start"}]}"#).unwrap();
        let turn = TurnSteers::new(mailbox.clone(), "t".into(), vec![initial]);
        let steers: Vec<_> = array(&field(case, "steers")).iter().map(steer).collect();
        for message in &steers[..2] {
            mailbox.enqueue("t".into(), message.clone()).await.unwrap();
        }
        let first = list(turn.drain());
        mailbox
            .enqueue("t".into(), steers[2].clone())
            .await
            .unwrap();
        let last = drained(turn.drain_or_close());
        let record = |message: JsValue| -> HostFuture<()> {
            let trace = trace.clone();
            let fail = mode == "durable-fails";
            Box::pin(async move {
                let id = message
                    .get("metadata")
                    .and_then(|m| m.get("queueItemId"))
                    .cloned()
                    .unwrap_or(JsValue::Null);
                trace
                    .lock()
                    .unwrap()
                    .push(JsValue::Array(vec!["durable".into(), id]));
                if fail {
                    Err(ModuleError::new("durable failed").into())
                } else {
                    Ok(())
                }
            })
        };
        // Move owned captures into the callback, so it runs only after claim.
        let first_record = record(steers[0].message.clone());
        let applied = turn
            .on_durable_message(steers[0].message.clone(), move |_| first_record)
            .await;
        let mut after = JsValue::Array(Vec::new());
        if applied.is_ok() {
            for _ in 0..2 {
                turn.on_steer_applied(&"a".into());
                trace
                    .lock()
                    .unwrap()
                    .push(JsValue::Array(vec!["ack".into(), "a".into()]));
            }
            let assistant = parse_json(r#"{"role":"assistant","content":[]}"#).unwrap();
            let next_record = record(assistant.clone());
            turn.on_durable_message(assistant, move |_| next_record)
                .await
                .unwrap();
            after = JsValue::Array(turn.messages());
        }
        let unapplied = JsValue::Array(
            turn.unacknowledged()
                .into_iter()
                .map(|m| JsValue::String(m.item_id))
                .collect(),
        );
        // Observe ack's mailbox effect, independently of the callback trace.
        let remaining = mailbox.close("t".into()).await.unwrap();
        equal(
            &JsValue::Array(
                remaining
                    .into_iter()
                    .map(|m| JsValue::String(m.item_id))
                    .collect(),
            ),
            &unapplied,
            "only unapplied messages remain in the mailbox",
        );
        equal(
            &val([
                ("trace", JsValue::Array(trace.lock().unwrap().clone())),
                ("drains", JsValue::Array(vec![first, last])),
                ("afterApplied", after),
                ("unapplied", unapplied),
            ]),
            &field(case, "expected"),
            &mode,
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_enqueues_are_deduplicated_and_drain_is_single_delivery() {
    let recorder = Arc::new(Recorder::default());
    let mailbox = SteerMailbox::new(Some(recorder.clone()));
    mailbox.start("t".into());
    let barrier = Arc::new(Barrier::new(65));
    let mut tasks = Vec::new();
    for _ in 0..64 {
        let mailbox = mailbox.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            mailbox.enqueue("t".into(), message("same")).await.unwrap()
        }));
    }
    barrier.wait().await;
    for task in tasks {
        assert!(task.await.unwrap().accepted);
    }
    assert_eq!(array(&recorder.mutations()).len(), 1);
    let a = mailbox.clone();
    let b = mailbox.clone();
    let (a, b) = tokio::join!(
        tokio::spawn(async move { a.drain(&"t".into()) }),
        tokio::spawn(async move { b.drain(&"t".into()) })
    );
    assert_eq!(a.unwrap().len() + b.unwrap().len(), 1);
    mailbox.claim("t".into(), "same".into()).await.unwrap();
    mailbox.ack(&"t".into(), &"same".into());
    assert!(mailbox.close("t".into()).await.unwrap().is_empty());
}
#[tokio::test]
async fn dropping_a_waiter_does_not_cancel_an_enqueued_durable_mutation() {
    let recorder = Arc::new(Recorder::default());
    let mailbox = SteerMailbox::new(Some(recorder.clone()));
    mailbox.start("t".into());
    let gate = recorder.block("cancel".into());
    let pending = mailbox.cancel("t".into(), "a".into());
    gate.entered.notified().await;
    drop(pending);
    let enqueue = mailbox.enqueue("t".into(), message("a"));
    gate.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(5), enqueue)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.reason, Some(SteerReason::Cancelled));
    assert_eq!(array(&recorder.mutations()).len(), 1);
}
#[tokio::test]
async fn durable_failure_keeps_authority_pending_until_success_and_ack() {
    let mailbox = SteerMailbox::default();
    mailbox.start("t".into());
    mailbox.enqueue("t".into(), message("a")).await.unwrap();
    let turn = TurnSteers::new(mailbox.clone(), "t".into(), Vec::new());
    let checkpoint = HostToolCheckpoint::default();
    let handler = Handler::new(
        Ports::for_turn(turn.clone()),
        SteerAuthorizations::new(checkpoint.clone()),
    );
    handler
        .call(val([(
            "payload",
            val([("operation", "drain_steer".into())]),
        )]))
        .await
        .unwrap();
    assert!(
        checkpoint
            .tool_context_state()
            .allowed_read_files
            .is_empty()
    );
    let fail = handler
        .persist_and_apply_steer(turn.clone(), "a".into(), message("a").message, |_| {
            Box::pin(async { Err(ModuleError::new("disk full").into()) })
        })
        .await;
    assert!(fail.is_err());
    assert!(turn.messages().is_empty());
    assert_eq!(turn.unacknowledged().len(), 1);
    assert!(
        checkpoint
            .tool_context_state()
            .allowed_read_files
            .is_empty()
    );
    let before = checkpoint.tool_context_state();
    let in_record = checkpoint.clone();
    let tracked = turn.clone();
    handler
        .persist_and_apply_steer(turn.clone(), "a".into(), message("a").message, move |_| {
            assert!(in_record.tool_context_state().allowed_read_files.is_empty());
            assert!(tracked.messages().is_empty());
            Box::pin(async { Ok(()) })
        })
        .await
        .unwrap();
    assert!(before.allowed_read_files.is_empty());
    assert_eq!(
        checkpoint.tool_context_state().allowed_read_files,
        vec![JsString::from("/a")]
    );
    assert_eq!(turn.messages().len(), 1);
    assert!(turn.unacknowledged().is_empty());
    assert!(mailbox.close("t".into()).await.unwrap().is_empty());
    handler.apply_steer_authorization(&"a".into());
    turn.on_steer_applied(&"a".into());
    assert_eq!(turn.messages().len(), 1);
}
