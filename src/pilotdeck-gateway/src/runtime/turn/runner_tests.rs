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
        "complete-once" | "complete-always" => "turn_result",
        _ => "",
    };
    let f = Fixture::new(fault, if name == "complete-once" { 1 } else { 99 });
    let expected: Vec<_> = array(row.get("events").unwrap())
        .into_iter()
        .filter(|e| {
            !["user_prompt_submitted", "model_event"]
                .iter()
                .any(|k| e.get("type") == Some(&(*k).into()))
        })
        .collect();
    let mut events = vec![];
    let result: RecorderResult<()> = async {
        let (runner, start) = f.runner().start().await?;
        events.extend(start);
        let mut prepared = input();
        if let Some(raw) = row.get("rawInput") {
            prepared.history = array(row.get("history").unwrap());
            prepared.synthetic = array(row.get("synthetic").unwrap());
            prepared.metadata = object([("allowPlanModeTools", JsValue::Bool(false))])
                .as_object()
                .cloned();
            if let Some(content) = raw.get("content") {
                prepared.input = AgentInput::Blocks(array(content));
            }
        }
        let (mut runner, accepted) = runner.accept(prepared).await?;
        events.extend(accepted);
        if let Some(messages) = row.get("loopMessages") {
            assert_eq!(runner.messages(), array(messages));
        }
        if runner.is_complete() || name == "consumer-drop" {
            return Ok(());
        }
        if ["loop-fails-twice", "status-fails", "loop-null-hint"].contains(&name) {
            for _ in 0..2 {
                let (next, failed) = runner
                    .loop_failed(
                        if name == "loop-null-hint" {parse_json(r#"{"code":"agent_invalid_state","message":"loop failed","userHint":null}"#).unwrap()} else {parse_json(r#"{"code":"agent_invalid_state","message":"loop failed"}"#).unwrap()},
                    )
                    .await?;
                runner = next;
                events.extend(failed);
            }
        }
        let (finished, done) = if name == "loop-aborted-error" { runner.fail(parse_json(r#"{"code":"agent_aborted","message":"injected abort"}"#).unwrap()).await? } else if name == "loop-throws" {
            runner
                .fail(
                    parse_json(
                        r#"{"code":"agent_invalid_state","message":"injected loop failure"}"#,
                    )
                    .unwrap(),
                )
                .await?
        } else {
            let result = if name.starts_with("complete-") {
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
            runner.complete(result).await?
        };
        assert!(finished.is_complete());
        events.extend(done);
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
async fn invalid_transitions_and_identity_never_write() {
    let f = Fixture::new("", 0);
    assert!(
        matches!(f.runner().accept(input()).await,Err(e) if e.to_string()=="Invalid turn recording transition.")
    );
    assert!(
        matches!(f.runner().fail(object([])).await,Err(e) if e.to_string()=="Invalid turn recording transition.")
    );
    assert!(!f.key.transcript_path.exists());
    for wrong in [
        r#"{"type":"success","sessionId":"other","turnId":"t"}"#,
        r#"{"type":"success","sessionId":"s","turnId":"other"}"#,
    ] {
        let f = Fixture::new("", 0);
        let (runner, _) = f.runner().start().await.unwrap();
        let (runner, _) = runner.accept(input()).await.unwrap();
        let before = fs::read(&f.key.transcript_path).unwrap();
        assert!(
            matches!(runner.complete(parse_json(wrong).unwrap()).await,Err(e) if e.to_string()=="Turn result identity mismatch.")
        );
        assert_eq!(fs::read(&f.key.transcript_path).unwrap(), before);
    }
}

#[test]
#[ignore = "requires Node 22 and pinned TS; spawns real sidecars"]
fn runner_regenerates_identically() {
    baseline::regenerates_identically("turn-runner");
}
