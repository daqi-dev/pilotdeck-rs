use super::*;
use crate::{
    runtime::project::{ProjectRuntimeRegistry, ResourceBundle},
    storage::{JsonlTranscriptStore, StoreResult, TranscriptKey, TranscriptRead, TranscriptStore},
};
use pilotdeck_protocol::parse_json;
use std::{
    fs,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::test_baseline as baseline;
struct Store {
    disk: JsonlTranscriptStore,
    fault: String,
    remaining: AtomicUsize,
    gate: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
}
impl TranscriptStore for Store {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        if let Some((started, release)) = self.gate.lock().unwrap().take() {
            started.send(()).unwrap();
            release
                .recv_timeout(Duration::from_secs(10))
                .expect("bounded I/O gate");
        }
        if entry.get("type") == Some(&self.fault.as_str().into())
            && self
                .remaining
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
        {
            return Err(std::io::Error::other("injected write failure").into());
        }
        self.disk.append(key, entry)
    }
    fn read(&self, key: &TranscriptKey, limit: Option<u64>) -> StoreResult<TranscriptRead> {
        self.disk.read(key, limit)
    }
}
struct Fixture {
    root: std::path::PathBuf,
    key: TranscriptKey,
    store: Arc<Store>,
    registry: ProjectRuntimeRegistry,
}
impl Fixture {
    fn new(fault: &str, failures: usize) -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-runner-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let key = TranscriptKey {
            project_root: root.clone(),
            pilot_home: root.join("home"),
            session_id: "s".into(),
            transcript_path: root.join("s.jsonl"),
        };
        let registry =
            ProjectRuntimeRegistry::new(&root, Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(())));
        Self {
            root,
            key,
            registry,
            store: Arc::new(Store {
                disk: JsonlTranscriptStore::new(),
                fault: fault.into(),
                remaining: AtomicUsize::new(failures),
                gate: Mutex::new(None),
            }),
        }
    }
    fn runner(&self) -> TurnRunner {
        let ids = AtomicUsize::new(0);
        let writer = Recorder::new(self.store.clone(), self.key.clone())
            .writer()
            .clone();
        let duplicate = self.store.fault == "duplicate";
        *writer.test_sources.lock().unwrap() = Some((
            Arc::new(|| Ok("2026-10-26T00:00:00.000Z".into())),
            Arc::new(move || {
                let id = ids.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(format!("id-{}", if duplicate && id == 2 { 1 } else { id }))
            }),
        ));
        let recorder = Arc::new(Recorder::from_writer(writer));
        let mut runner = TurnRunner::new(
            recorder,
            Arc::new(self.registry.acquire(None).unwrap()),
            "s".into(),
            "t".into(),
        );
        runner.now = Arc::new(|| "2026-10-26T00:00:00.000Z".into());
        runner
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn array(v: &JsValue) -> Vec<JsValue> {
    let JsValue::Array(v) = v else {
        panic!("array")
    };
    v.clone()
}
fn text(v: &JsValue) -> String {
    v.as_string().unwrap().to_utf8().unwrap()
}
fn input() -> TurnInput {
    TurnInput {
        input: AgentInput::Text("probe".into()),
        history: vec![],
        synthetic: vec![],
        metadata: None,
    }
}

async fn replay(name: &str) {
    let oracle = baseline::oracle(include_str!(
        "../../../tests/fixtures/turn-runner/oracle.json"
    ));
    let rows = array(oracle.get("matrix").unwrap());
    let row = rows
        .iter()
        .find(|r| r.get("name") == Some(&name.into()))
        .unwrap();
    let fault = match name {
        "start-fails" => "turn_started",
        "accepted-fails" => "accepted_input",
        "accepted-duplicate" => "duplicate",
        "status-fails" => "agent_status_message",
        "complete-once" | "complete-always" | "loop-failed-complete-once" => "turn_result",
        _ => "",
    };
    let f = Fixture::new(
        fault,
        if name.ends_with("complete-once") {
            1
        } else {
            99
        },
    );
    let mut expected: Vec<_> = array(row.get("events").unwrap())
        .into_iter()
        .filter(|e| {
            !["user_prompt_submitted", "model_event"]
                .iter()
                .any(|k| e.get("type") == Some(&(*k).into()))
        })
        .collect();
    let mut events = vec![];
    let result: RecorderResult<()> = async {
        let mut recording = Recording::new(f.runner(), Arc::new(()));
        events.extend(advance(&mut recording, RecordingAction::Start).await?);
        let mut prepared = input();
        if let Some(raw) = row.get("rawInput") {
            prepared = TurnInput::from_agent_input(
                raw,
                row.get("submitOptions").unwrap_or(&object([("allowPlanModeTools", JsValue::Bool(false))])),
                array(row.get("history").unwrap()),
                array(row.get("synthetic").unwrap()),
            )?;
            let accepted = text(row.get("transcript").unwrap()).lines()
                .map(|line| parse_json(line).unwrap())
                .find(|entry| entry.get("type") == Some(&"accepted_input".into())).unwrap();
            assert_eq!(prepared.metadata.clone().map(JsValue::Object), accepted.get("metadata").cloned(), "{name}: projected metadata before writer normalization");
        }
        events.extend(advance(&mut recording, RecordingAction::Accept(prepared)).await?);
        let runner = recording.runner().unwrap();
        if let Some(messages) = row.get("loopMessages") {
            assert_eq!(runner.messages(), array(messages));
        }
        if runner.is_complete() || name == "consumer-drop" {
            return Ok(());
        }
        if row.get("openStep") == Some(&JsValue::Bool(true)) {
            runner.recorder.record_model_request(&"s".into(), &"t".into(), object([("messages", JsValue::Array(vec![]))]))?;
        }
        if let Some(error) = row.get("failureError") {
            let mut error = error.clone();
            if row.get("failureMessageUndefined") == Some(&JsValue::Bool(true)) {
                let JsValue::Object(fields) = &mut error else { panic!("error object") };
                fields.insert("message".into(), JsValue::Undefined);
                // TS yields the original loop error; JSON drops its undefined
                // property. Restore only that input in the raw event assertion.
                let event = expected.iter_mut().find(|e| e.get("type") == Some(&"turn_failed".into())).unwrap();
                let JsValue::Object(fields) = event else { panic!("event object") };
                fields.insert("error".into(), error.clone());
            }
            let failures = advance(&mut recording, RecordingAction::LoopFailed(error)).await?;
            if name != "loop-null-message" {
                assert!(failures[0].get("text").is_none(), "wire status must omit undefined text");
                assert!(failures[0].get("detail").unwrap().get("message").is_none());
            }
            events.extend(failures);
        }
        if ["loop-fails-twice", "status-fails", "loop-null-hint"].contains(&name) || name.starts_with("loop-failed-") {
            for _ in 0..if name.starts_with("loop-failed-") { 1 } else { 2 } {
                events.extend(advance(&mut recording, RecordingAction::LoopFailed(
                    if name == "loop-null-hint" {parse_json(r#"{"code":"agent_invalid_state","message":"loop failed","userHint":null}"#).unwrap()} else {parse_json(r#"{"code":"agent_invalid_state","message":"loop failed"}"#).unwrap()},
                )).await?);
            }
        }
        let action = if name == "loop-aborted-error" {
            RecordingAction::Fail(parse_json(r#"{"code":"agent_aborted","message":"injected abort"}"#).unwrap())
        } else if name == "loop-throws" {
            RecordingAction::Fail(parse_json(r#"{"code":"agent_invalid_state","message":"injected loop failure"}"#).unwrap())
        } else {
            let result = if row.get("rawResultUndefined") == Some(&JsValue::Bool(true)) {
                JsValue::Undefined
            } else if let Some(raw) = row.get("rawResult") {
                raw.clone()
            } else if name.starts_with("complete-") {
                rows.iter()
                    .find(|r| r.get("name") == Some(&"success".into()))
                    .unwrap()
                    .get("events")
                    .map(array)
                    .unwrap()
                    .last()
                    .unwrap()
                    .get("result")
                    .unwrap()
                    .clone()
            } else {
                expected.last().unwrap().get("result").unwrap().clone()
            };
            RecordingAction::Complete(result)
        };
        events.extend(advance(&mut recording, action).await?);
        assert!(recording.runner().unwrap().is_complete());
        if row.get("rawResult").is_some() || row.get("rawResultUndefined") == Some(&JsValue::Bool(true)) {
            assert!(recording.runner().unwrap().recorder.record_model_request(&"s".into(), &"t".into(), object([])).is_err(), "invalid result must close recorder turn");
        }
        Ok(())
    }
    .await;
    assert_eq!(
        result.err().map(|e| e.to_string()),
        if row.get("error") == Some(&JsValue::Null) {
            None
        } else {
            Some(text(row.get("error").unwrap()))
        },
        "{name}: error"
    );
    assert_eq!(events, expected, "{name}: runner-owned events");
    assert_eq!(
        fs::read_to_string(&f.key.transcript_path).unwrap_or_default(),
        text(row.get("transcript").unwrap()),
        "{name}: JSONL bytes"
    );
    assert_eq!(
        f.registry.resolve(None).unwrap().lease_count(),
        0,
        "{name}: lease released"
    );
}
macro_rules! case {
    ($test:ident,$name:literal) => {
        #[tokio::test]
        async fn $test() {
            replay($name).await;
        }
    };
}
case!(runner_success, "success");
case!(runner_start_write_failure, "start-fails");
case!(runner_accepted_write_failure, "accepted-fails");
case!(runner_repeated_loop_failure, "loop-fails-twice");
case!(runner_loop_exception, "loop-throws");
case!(runner_consumer_drop, "consumer-drop");
case!(runner_completion_retry, "complete-once");
case!(runner_completion_persistent_failure, "complete-always");
case!(runner_input_text_boundaries, "input-text");
case!(runner_input_blocks_boundaries, "input-blocks");
case!(runner_accepted_validation_error, "accepted-duplicate");
case!(runner_aborted_exception, "loop-aborted-error");
case!(runner_best_effort_status, "status-fails");
case!(runner_nullish_hint, "loop-null-hint");

#[tokio::test(flavor = "current_thread")]
async fn cancelled_transition_keeps_worker_lease_until_io_exits() {
    let f = Fixture::new("", 0);
    let runtime = f.registry.resolve(None).unwrap();
    let (started, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    *f.store.gate.lock().unwrap() = Some((started, wait));
    let task = tokio::spawn(f.runner().start());
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(matches!(task.await,Err(e) if e.is_cancelled()));
    assert_eq!(
        runtime.lease_count(),
        1,
        "worker must retain the actual lease"
    );
    f.registry.invalidate(None);
    assert_ne!(
        runtime.state(),
        crate::runtime::project::RuntimeState::Disposed
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), runtime.wait_disposed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.lease_count(), 0);
}

#[tokio::test]
async fn invalid_transitions_never_write() {
    let f = Fixture::new("", 0);
    assert!(
        matches!(f.runner().accept(input()).await,Err(e) if e.to_string()=="Invalid turn recording transition.")
    );
    assert!(
        matches!(f.runner().fail(object([])).await,Err(e) if e.to_string()=="Invalid turn recording transition.")
    );
    assert!(!f.key.transcript_path.exists());
}

async fn invalid_result_difference(name: &str) {
    let oracle = baseline::oracle(include_str!(
        "../../../tests/fixtures/turn-runner/oracle.json"
    ));
    let rows = array(oracle.get("matrix").unwrap());
    let row = rows
        .iter()
        .find(|r| r.get("name") == Some(&name.into()))
        .unwrap();
    let f = Fixture::new("", 0);
    let (runner, _) = f.runner().start().await.unwrap();
    let (runner, _) = runner.accept(input()).await.unwrap();
    let step = row.get("openStep") == Some(&JsValue::Bool(true));
    if step {
        runner
            .recorder
            .record_model_request(
                &"s".into(),
                &"t".into(),
                object([("messages", JsValue::Array(vec![]))]),
            )
            .unwrap();
    }
    let raw = row.get("rawResult").unwrap().clone();
    // Preserve the TS discrepancy, including its original completion payload.
    assert_eq!(
        array(row.get("events").unwrap())
            .last()
            .unwrap()
            .get("result"),
        Some(&raw)
    );
    let mut raw = raw;
    if row.get("rawTurnUndefined") == Some(&JsValue::Bool(true)) {
        let JsValue::Object(fields) = &mut raw else {
            panic!("raw result object")
        };
        fields.insert("turnId".into(), JsValue::Undefined);
    }
    let (runner, events) = runner.complete(raw).await.unwrap();
    assert!(runner.is_complete());
    assert_eq!(
        events
            .iter()
            .map(|e| text(e.get("type").unwrap()))
            .collect::<Vec<_>>(),
        ["agent_status", "turn_failed", "turn_completed"]
    );
    let result = events[2].get("result").unwrap();
    assert_eq!(result.get("type"), Some(&"error".into()));
    assert_eq!(result.get("sessionId"), Some(&"s".into()));
    assert_eq!(result.get("turnId"), Some(&"t".into()));
    assert_eq!(
        events[1].get("error"),
        Some(&object([
            ("code", "agent_invalid_state".into()),
            ("message", "Turn result identity mismatch.".into())
        ]))
    );
    let entries: Vec<_> = fs::read_to_string(&f.key.transcript_path)
        .unwrap()
        .lines()
        .map(|line| parse_json(line).unwrap())
        .collect();
    assert!(
        entries.iter().all(
            |e| e.get("sessionId") == Some(&"s".into()) && e.get("turnId") == Some(&"t".into())
        )
    );
    let terminal: Vec<_> = entries
        .iter()
        .filter(|e| e.get("type") == Some(&"turn_result".into()))
        .collect();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].get("result"), Some(result));
    let closed: Vec<_> = entries
        .iter()
        .filter(|e| e.get("type") == Some(&"step_completed".into()))
        .collect();
    assert_eq!(closed.len(), usize::from(step));
    if step {
        assert_eq!(closed[0].get("outcome"), Some(&"failed".into()));
    }
    assert!(
        runner
            .recorder
            .record_model_request(&"s".into(), &"t".into(), object([]))
            .is_err(),
        "turn removed from recorder"
    );
    drop(runner);
    assert_eq!(f.registry.resolve(None).unwrap().lease_count(), 0);
}
case!(runner_invalid_result_turn_idle, "invalid-result-turn-idle");
case!(runner_invalid_result_turn_step, "invalid-result-turn-step");
#[tokio::test]
async fn runner_invalid_result_session_idle() {
    invalid_result_difference("invalid-result-session-idle").await;
}
#[tokio::test]
async fn runner_invalid_result_session_step() {
    invalid_result_difference("invalid-result-session-step").await;
}
#[tokio::test]
async fn runner_invalid_result_number_idle() {
    invalid_result_difference("invalid-result-number-idle").await;
}
#[tokio::test]
async fn runner_invalid_result_number_step() {
    invalid_result_difference("invalid-result-number-step").await;
}
#[tokio::test]
async fn runner_invalid_result_array_idle() {
    invalid_result_difference("invalid-result-array-idle").await;
}
#[tokio::test]
async fn runner_invalid_result_array_step() {
    invalid_result_difference("invalid-result-array-step").await;
}
case!(runner_invalid_result_null_idle, "invalid-result-null-idle");
case!(runner_invalid_result_null_step, "invalid-result-null-step");
case!(runner_loop_missing_message, "loop-missing-message");
case!(runner_loop_undefined_message, "loop-undefined-message");
case!(runner_loop_null_message, "loop-null-message");

case!(
    runner_loop_failed_complete_once,
    "loop-failed-complete-once"
);
case!(runner_loop_failed_invalid_idle, "loop-failed-invalid-idle");
case!(runner_loop_failed_invalid_step, "loop-failed-invalid-step");
case!(
    runner_invalid_result_undefined_idle,
    "invalid-result-undefined-idle"
);
case!(
    runner_invalid_result_undefined_step,
    "invalid-result-undefined-step"
);
case!(
    runner_invalid_result_turn_number_idle,
    "invalid-result-turn-number-idle"
);
case!(
    runner_invalid_result_turn_number_step,
    "invalid-result-turn-number-step"
);
case!(
    runner_invalid_result_turn_null_idle,
    "invalid-result-turn-null-idle"
);
case!(
    runner_invalid_result_turn_null_step,
    "invalid-result-turn-null-step"
);
case!(
    runner_invalid_result_turn_bool_idle,
    "invalid-result-turn-bool-idle"
);
case!(
    runner_invalid_result_turn_bool_step,
    "invalid-result-turn-bool-step"
);
case!(
    runner_invalid_result_turn_array_idle,
    "invalid-result-turn-array-idle"
);
case!(
    runner_invalid_result_turn_array_step,
    "invalid-result-turn-array-step"
);
case!(
    runner_invalid_result_turn_object_idle,
    "invalid-result-turn-object-idle"
);
case!(
    runner_invalid_result_turn_object_step,
    "invalid-result-turn-object-step"
);
case!(
    runner_invalid_result_turn_coercion_idle,
    "invalid-result-turn-coercion-idle"
);
case!(
    runner_invalid_result_turn_coercion_step,
    "invalid-result-turn-coercion-step"
);
macro_rules! difference_case {
    ($test:ident, $name:literal) => {
        #[tokio::test]
        async fn $test() {
            invalid_result_difference($name).await;
        }
    };
}
difference_case!(
    runner_invalid_result_missing_session_idle,
    "invalid-result-missing-session-idle"
);
difference_case!(
    runner_invalid_result_missing_session_step,
    "invalid-result-missing-session-step"
);
difference_case!(
    runner_invalid_result_missing_turn_idle,
    "invalid-result-missing-turn-idle"
);
difference_case!(
    runner_invalid_result_missing_turn_step,
    "invalid-result-missing-turn-step"
);
difference_case!(
    runner_invalid_result_session_number_idle,
    "invalid-result-session-number-idle"
);
difference_case!(
    runner_invalid_result_session_number_step,
    "invalid-result-session-number-step"
);
difference_case!(
    runner_invalid_result_session_null_idle,
    "invalid-result-session-null-idle"
);
difference_case!(
    runner_invalid_result_session_null_step,
    "invalid-result-session-null-step"
);
difference_case!(
    runner_invalid_result_session_bool_idle,
    "invalid-result-session-bool-idle"
);
difference_case!(
    runner_invalid_result_session_bool_step,
    "invalid-result-session-bool-step"
);
difference_case!(
    runner_invalid_result_session_array_idle,
    "invalid-result-session-array-idle"
);
difference_case!(
    runner_invalid_result_session_array_step,
    "invalid-result-session-array-step"
);
difference_case!(
    runner_invalid_result_session_object_idle,
    "invalid-result-session-object-idle"
);
difference_case!(
    runner_invalid_result_session_object_step,
    "invalid-result-session-object-step"
);

case!(
    runner_invalid_result_turn_surrogate_idle,
    "invalid-result-turn-surrogate-idle"
);
case!(
    runner_invalid_result_turn_surrogate_step,
    "invalid-result-turn-surrogate-step"
);
difference_case!(
    runner_invalid_result_turn_undefined_idle,
    "invalid-result-turn-undefined-idle"
);
difference_case!(
    runner_invalid_result_turn_undefined_step,
    "invalid-result-turn-undefined-step"
);

#[test]
#[ignore = "requires Node 22 and pinned TS; spawns real sidecars"]
fn runner_regenerates_identically() {
    baseline::regenerates_identically("turn-runner");
}

async fn advance(
    recording: &mut Recording,
    action: RecordingAction,
) -> RecorderResult<Vec<JsValue>> {
    recording.begin(action)?;
    recording.settle().await
}
case!(recording_original_text, "input-text-original");
case!(recording_metadata_all, "input-metadata-all");
case!(recording_metadata_false, "input-metadata-false");
case!(recording_metadata_null, "input-metadata-null");
case!(recording_metadata_absent, "input-metadata-absent");
case!(recording_metadata_array, "input-metadata-array");
case!(recording_metadata_string, "input-metadata-string");
case!(recording_metadata_number, "input-metadata-number");

struct Owner(Arc<AtomicUsize>);
impl Drop for Owner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
async fn recording_gate(phase: u8, abandon: bool) {
    let f = Fixture::new("", 0);
    let released = Arc::new(AtomicUsize::new(0));
    let owner = Arc::new(Owner(released.clone()));
    let mut recording = Recording::new(f.runner(), owner.clone());
    if phase > 0 {
        advance(&mut recording, RecordingAction::Start)
            .await
            .unwrap();
    }
    if phase > 1 {
        advance(&mut recording, RecordingAction::Accept(input()))
            .await
            .unwrap();
    }
    let before = fs::read_to_string(&f.key.transcript_path).unwrap_or_default();
    let result =
        parse_json(r#"{"type":"success","sessionId":"s","turnId":"t","stopReason":"completed"}"#)
            .unwrap();
    recording
        .begin(match phase {
            0 => RecordingAction::Start,
            1 => RecordingAction::Accept(input()),
            _ => RecordingAction::Complete(result),
        })
        .unwrap();
    let (started, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    *f.store.gate.lock().unwrap() = Some((started, wait));
    {
        let waiting = recording.settle();
        tokio::pin!(waiting);
        tokio::select! {
            entered = tokio::time::timeout(Duration::from_secs(5), entered) => entered.unwrap().unwrap(),
            result = &mut waiting => panic!("write unexpectedly settled: {result:?}"),
        }
        // Drop only the waiter, leaving the transition in Recording.
    }
    assert_eq!(
        fs::read_to_string(&f.key.transcript_path).unwrap_or_default(),
        before
    );
    assert!(recording.runner().is_none());
    assert_eq!(
        recording
            .begin(RecordingAction::Start)
            .unwrap_err()
            .to_string(),
        "Recording transition is pending."
    );
    // The predecessor has not been released. A cancelled second waiter must
    // neither detach nor restart it; one explicit poll proves it is pending.
    {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        let mut waiting = Box::pin(recording.settle());
        assert!(matches!(
            waiting
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
    }
    let runtime = f.registry.resolve(None).unwrap();
    f.registry.invalidate(None);
    drop(owner); // caller's shared active guard
    if abandon {
        drop(recording);
        assert_eq!(
            released.load(Ordering::SeqCst),
            0,
            "worker owns the same guard"
        );
        assert_eq!(runtime.lease_count(), 1);
        release.send(()).unwrap();
    } else {
        assert_eq!(released.load(Ordering::SeqCst), 0);
        release.send(()).unwrap();
        let events = tokio::time::timeout(Duration::from_secs(5), recording.settle())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type"),
            Some(&["turn_started", "input_accepted", "turn_completed"][phase as usize].into())
        );
        assert_eq!(recording.runner().unwrap().is_complete(), phase == 2);
        if phase == 2 {
            assert!(recording.begin(RecordingAction::Start).is_err());
        }
        assert!(
            recording.settle().await.is_err(),
            "settled transition cannot replay events"
        );
        assert_eq!(
            released.load(Ordering::SeqCst),
            0,
            "runner still owns the guard"
        );
        drop(recording);
    }
    tokio::time::timeout(Duration::from_secs(5), runtime.wait_disposed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(released.load(Ordering::SeqCst), 1);
    let kind = ["turn_started", "accepted_input", "turn_result"][phase as usize];
    let entries = fs::read_to_string(&f.key.transcript_path).unwrap();
    assert_eq!(
        entries
            .lines()
            .filter(|line| parse_json(line).unwrap().get("type") == Some(&kind.into()))
            .count(),
        1,
        "exactly one durable transition"
    );
}
macro_rules! gate_case {
    ($name:ident, $phase:literal, $drop:literal) => {
        #[tokio::test]
        async fn $name() {
            recording_gate($phase, $drop).await;
        }
    };
}
gate_case!(recording_cancel_start_then_resume, 0, false);
gate_case!(recording_cancel_accept_then_resume, 1, false);
gate_case!(recording_cancel_complete_then_resume, 2, false);
gate_case!(recording_drop_start_worker, 0, true);
gate_case!(recording_drop_accept_worker, 1, true);
gate_case!(recording_drop_complete_worker, 2, true);

#[tokio::test]
async fn recording_unpolled_drop_and_failed_transition_cannot_restart() {
    for begin in [false, true] {
        let f = Fixture::new("", 0);
        let released = Arc::new(AtomicUsize::new(0));
        let mut recording = Recording::new(f.runner(), Arc::new(Owner(released.clone())));
        if begin {
            recording.begin(RecordingAction::Start).unwrap();
        }
        drop(recording);
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert!(!f.key.transcript_path.exists());
        assert_eq!(f.registry.resolve(None).unwrap().lease_count(), 0);
    }
    let f = Fixture::new("turn_started", 1);
    let mut recording = Recording::new(f.runner(), Arc::new(()));
    assert!(recording.settle().await.is_err());
    assert!(
        advance(&mut recording, RecordingAction::Start)
            .await
            .is_err()
    );
    assert!(recording.runner().is_none());
    assert!(recording.settle().await.is_err());
    assert!(
        recording.begin(RecordingAction::Start).is_err(),
        "no retry of consumed runner"
    );
    assert!(!f.key.transcript_path.exists());
}

#[test]
fn recording_input_rejects_non_agent_input_and_drops_model_prototype() {
    for raw in [
        r#"{"messages":[]}"#,
        r#"{"type":"unknown","text":"x"}"#,
        r#"{"type":"text","text":null}"#,
        r#"{"type":"blocks","content":{}}"#,
    ] {
        assert!(
            TurnInput::from_agent_input(&parse_json(raw).unwrap(), &object([]), vec![], vec![])
                .is_err()
        );
    }
    let mut model = JsObject::new();
    model.insert("model".into(), "chosen".into());
    model.set_prototype(object([("inherited", JsValue::Bool(true))]));
    let raw =
        parse_json(r#"{"type":"text","text":"raw\ud800","metadata":{"permissionMode":"wrong"}}"#)
            .unwrap();
    let options = object([
        ("modelSelection", JsValue::Object(model)),
        ("allowPlanModeTools", JsValue::Undefined),
    ]);
    let parsed = TurnInput::from_agent_input(&raw, &options, vec![], vec![]).unwrap();
    let metadata = parsed.metadata.unwrap();
    assert_eq!(metadata.entries().len(), 1);
    assert_eq!(
        metadata.get("modelSelection"),
        Some(&object([("model", "chosen".into())]))
    );
    assert!(
        metadata
            .get("modelSelection")
            .unwrap()
            .as_object()
            .unwrap()
            .prototype()
            .is_none()
    );
}
