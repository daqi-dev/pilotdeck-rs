use super::*;
use crate::runtime::turn::coordinator::{TurnEventCoordinator, TurnSinkRegistration};
use crate::runtime::turn::publish::{FileRegistry, RegistryFuture, TurnPublisher};
use crate::runtime::turn::replay::ActiveTurnSnapshotInput;
use crate::storage::{FileRunRegistry, RunAcceptInput, RunRegistryError};
use pilotdeck_protocol::{JsObject, JsString, parse_json, stringify_json};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ORACLE: &str = include_str!("../../../tests/fixtures/turn-read/oracle.json");

fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
fn json(value: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap()
}
/// Renders with JS key order, so replay comparisons stay order-sensitive —
/// a port with the right fields in the wrong order fails here.
fn render(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}
/// The generator's `Object.keys` recording of a result.
fn keys(value: &JsValue) -> JsValue {
    match value.as_object() {
        Some(object) => JsValue::Array(
            object
                .entries()
                .iter()
                .map(|(key, _)| JsValue::String(key.clone()))
                .collect(),
        ),
        None => JsValue::Null,
    }
}
/// The generator's error recording: `name`, `code` when the error has one,
/// then `message`.
fn error_value(error: &TurnReadError) -> JsValue {
    let mut object = JsObject::new();
    object.insert("name".into(), JsValue::String(error.name.clone().into()));
    if let Some(code) = &error.code {
        object.insert("code".into(), JsValue::String(code.clone().into()));
    }
    object.insert("message".into(), JsValue::String(error.message.clone()));
    JsValue::Object(object)
}
fn array<'a>(value: &'a JsValue, key: &str) -> &'a [JsValue] {
    match value.get(key) {
        Some(JsValue::Array(items)) => items,
        _ => &[],
    }
}
fn sub<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn text<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsString> {
    value.get(key).and_then(JsValue::as_string)
}
fn run_ref(input: &JsValue) -> RunRef {
    RunRef {
        session_key: text(input, "sessionKey").cloned().unwrap_or_default(),
        run_id: text(input, "runId").cloned().unwrap_or_default(),
        project_key: text(input, "projectKey").cloned(),
    }
}
/// The generator masks its process's registry timestamps; the replay masks
/// its own before comparing. Values are replaced in place, so key order and
/// the generator's `Object.keys` recording both survive.
fn mask_timestamps(record: &JsValue) -> JsValue {
    let mut masked = record.clone();
    if let JsValue::Object(object) = &mut masked {
        for key in ["acceptedAt", "updatedAt"] {
            if object
                .get(key)
                .is_some_and(|value| value.as_string().is_some())
            {
                object.insert(key.into(), JsValue::String("<date>".into()));
            }
        }
    }
    masked
}
/// `{ keys, result }` for a present record (timestamps masked), `{ keys: null }`
/// for a miss and `{ error }` for a rejection — the generator's outcome
/// shapes, compared with [`render`] so order and omission both matter.
fn record_outcome(result: Result<Option<JsValue>, TurnReadError>) -> JsValue {
    match result {
        Ok(Some(record)) => {
            let mut outcome = JsObject::new();
            outcome.insert("keys".into(), keys(&record));
            outcome.insert("result".into(), mask_timestamps(&record));
            JsValue::Object(outcome)
        }
        Ok(None) => {
            let mut outcome = JsObject::new();
            outcome.insert("keys".into(), JsValue::Null);
            JsValue::Object(outcome)
        }
        Err(error) => {
            let mut outcome = JsObject::new();
            outcome.insert("error".into(), error_value(&error));
            JsValue::Object(outcome)
        }
    }
}

/// The host history reader driven by the oracle's `history` plan: each entry
/// mutates the coordinator synchronously (inside the read, after the epoch's
/// `before` snapshot) and then returns its history or throws its error. An
/// unplanned call panics, so a port that retries too often cannot pass.
struct ScriptedHistory {
    coordinator: Arc<TurnEventCoordinator>,
    plan: Mutex<VecDeque<JsValue>>,
    current_run: Mutex<Option<JsString>>,
    registrations: Mutex<Vec<TurnSinkRegistration>>,
    calls: AtomicUsize,
    seen: Mutex<Vec<JsValue>>,
}
impl ScriptedHistory {
    fn new(coordinator: Arc<TurnEventCoordinator>, case: &JsValue) -> Self {
        Self {
            coordinator,
            plan: Mutex::new(array(case, "history").to_vec().into()),
            current_run: Mutex::new(text(sub(case, "setup"), "startRun").cloned()),
            registrations: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(vec![]),
        }
    }
}
impl SessionHistoryReader for ScriptedHistory {
    fn read(
        &self,
        input: &JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, TurnReadError>> + Send>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(input.clone());
        let outcome = (|| {
            let entry = self
                .plan
                .lock()
                .unwrap()
                .pop_front()
                .expect("history call was not planned");
            for mutation in array(&entry, "mutate") {
                match mutation {
                    JsValue::String(mutation) if mutation.equals_str("retain") => {
                        let run = self
                            .current_run
                            .lock()
                            .unwrap()
                            .clone()
                            .expect("retain needs a started run");
                        self.coordinator.retain_terminal(&"s".into(), &run);
                    }
                    JsValue::Object(_) if mutation.get("start").is_some() => {
                        let run = mutation
                            .get("start")
                            .and_then(JsValue::as_string)
                            .cloned()
                            .unwrap();
                        let registration = self.coordinator.start(&"s".into(), &run, |_| {});
                        self.registrations.lock().unwrap().push(registration);
                        *self.current_run.lock().unwrap() = Some(run);
                    }
                    JsValue::Object(_) => {
                        let event = mutation
                            .get("record")
                            .cloned()
                            .unwrap_or(JsValue::Undefined);
                        self.coordinator.record(&"s".into(), &event);
                    }
                    other => panic!("unknown mutation {}", render(other)),
                }
            }
            if let Some(error) = entry.get("error") {
                return Err(TurnReadError {
                    code: None,
                    name: text(error, "name").unwrap().to_utf8().unwrap(),
                    message: text(error, "message").unwrap().clone(),
                });
            }
            Ok(entry.get("history").cloned().unwrap_or(JsValue::Undefined))
        })();
        Box::pin(async move { outcome })
    }
}

async fn replay_read_case(case: &JsValue) -> Result<(), String> {
    let coordinator = Arc::new(TurnEventCoordinator::new());
    let mut registrations = Vec::new();
    if let Some(run) = text(sub(case, "setup"), "startRun") {
        registrations.push(coordinator.start(&"s".into(), run, |_| {}));
        for event in array(sub(case, "setup"), "events") {
            coordinator.record(&"s".into(), event);
        }
    }
    let history = ScriptedHistory::new(coordinator.clone(), case);
    let input = sub(case, "setup")
        .get("input")
        .cloned()
        .unwrap_or(JsValue::Undefined);
    let configured = !matches!(
        sub(case, "setup").get("configured"),
        Some(JsValue::Bool(false))
    );
    let reader = configured.then_some(&history as &dyn SessionHistoryReader);
    let outcome = match read_session_messages(&coordinator, reader, &input).await {
        Ok(result) => {
            let mut outcome = JsObject::new();
            outcome.insert(
                "historyCalls".into(),
                JsValue::Number(history.calls.load(Ordering::SeqCst) as f64),
            );
            outcome.insert(
                "seenInputs".into(),
                JsValue::Array(history.seen.lock().unwrap().clone()),
            );
            outcome.insert("resultKeys".into(), keys(&result));
            outcome.insert("result".into(), result);
            JsValue::Object(outcome)
        }
        Err(error) => {
            let mut outcome = JsObject::new();
            outcome.insert(
                "historyCalls".into(),
                JsValue::Number(history.calls.load(Ordering::SeqCst) as f64),
            );
            outcome.insert(
                "seenInputs".into(),
                JsValue::Array(history.seen.lock().unwrap().clone()),
            );
            outcome.insert("error".into(), error_value(&error));
            JsValue::Object(outcome)
        }
    };
    drop(registrations);
    let (actual, expected) = (render(&outcome), render(sub(case, "outcome")));
    if actual != expected {
        return Err(format!(
            "{}\nactual: {}\nexpected: {}",
            text(case, "name").unwrap().to_utf8().unwrap(),
            actual,
            expected
        ));
    }
    Ok(())
}

fn replay_snapshot_case(case: &JsValue) -> Result<(), String> {
    let coordinator = TurnEventCoordinator::new();
    let mut registrations = Vec::new();
    if let Some(run) = text(sub(case, "setup"), "startRun") {
        registrations.push(coordinator.start(&"s".into(), run, |_| {}));
        for event in array(sub(case, "setup"), "events") {
            coordinator.record(&"s".into(), event);
        }
    }
    let mut results = Vec::new();
    for call in array(case, "calls") {
        let input = ActiveTurnSnapshotInput {
            session_key: text(call, "sessionKey").cloned().unwrap_or_default(),
            include_events: !matches!(call.get("includeEvents"), Some(JsValue::Bool(false))),
        };
        let result = active_turn_snapshot(&coordinator, &input).to_json();
        let mut recorded = JsObject::new();
        recorded.insert("keys".into(), keys(&result));
        recorded.insert("result".into(), result);
        results.push(JsValue::Object(recorded));
    }
    drop(registrations);
    let (actual, expected) = (
        render(&JsValue::Array(results)),
        render(sub(case, "results")),
    );
    if actual != expected {
        return Err(format!(
            "{}\nactual: {}\nexpected: {}",
            text(case, "name").unwrap().to_utf8().unwrap(),
            actual,
            expected
        ));
    }
    Ok(())
}

/// A registry read port that always rejects with the oracle's recorded
/// failure, the counterpart of the generator's throwing wrapper.
struct FailingReader {
    code: String,
    message: String,
}
impl FailingReader {
    fn error(&self) -> RunRegistryError {
        match self.code.as_str() {
            "terminal" => RunRegistryError::Terminal(self.message.clone()),
            "conflict" => RunRegistryError::Conflict(self.message.clone()),
            "not_found" => RunRegistryError::NotFound(self.message.clone()),
            _ => RunRegistryError::Invalid(self.message.clone()),
        }
    }
}
impl TurnRunReader for FailingReader {
    fn get(&self, _run: &RunRef) -> RegistryFuture<Option<RunRecord>> {
        let error = self.error();
        Box::pin(async move { Err(error) })
    }
    fn events(
        &self,
        _run: &RunRef,
        _after_seq: Option<f64>,
        _limit: Option<f64>,
    ) -> RegistryFuture<Vec<JsValue>> {
        let error = self.error();
        Box::pin(async move { Err(error) })
    }
}

/// The generator's per-call snapshot surgery, applied before the method runs:
/// `snapshot` writes raw file bytes (a corrupt registry), `recordPatch`
/// assigns fields onto a parsed record in place (a non-numeric `lastSeq`).
/// Both registries reload from disk on every read, so the next call observes
/// exactly what the TS generator left.
fn apply_call_snapshot_surgery(path: &std::path::Path, call: &JsValue) {
    if let Some(JsValue::String(text)) = call.get("snapshot") {
        std::fs::write(path, text.to_utf8().unwrap()).unwrap();
    }
    if let Some(patch) = call.get("recordPatch") {
        let index = patch
            .get("record")
            .and_then(JsValue::as_number)
            .unwrap_or_default() as usize;
        let snapshot = parse_json(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut records = snapshot.get("records").cloned().unwrap();
        let record = match &mut records {
            JsValue::Array(items) => match &mut items[index] {
                JsValue::Object(record) => record,
                _ => panic!("recordPatch needs an object record"),
            },
            _ => panic!("recordPatch needs a records array"),
        };
        for (key, value) in patch
            .get("set")
            .and_then(JsValue::as_object)
            .unwrap()
            .entries()
        {
            record.insert(key.clone(), value.clone());
        }
        let mut object = snapshot.as_object().cloned().unwrap();
        object.insert("records".into(), records);
        std::fs::write(path, render(&JsValue::Object(object))).unwrap();
    }
}

/// The generator's in-memory records port (`setup.inMemory`): `get` serves
/// each call's `record` in call order, `events` returns none — the gap
/// results it exists for return before `events` is reached. The deep-array
/// case rides it because the file registry's own V8 clone recursion cannot
/// survive the depths under test.
struct InMemoryReader {
    records: Mutex<VecDeque<JsValue>>,
}
impl TurnRunReader for InMemoryReader {
    fn get(&self, _run: &RunRef) -> RegistryFuture<Option<RunRecord>> {
        let record = self
            .records
            .lock()
            .unwrap()
            .pop_front()
            .expect("in-memory record was not set");
        Box::pin(async move { Ok(Some(RunRecord::from_json(record))) })
    }
    fn events(
        &self,
        _run: &RunRef,
        _after_seq: Option<f64>,
        _limit: Option<f64>,
    ) -> RegistryFuture<Vec<JsValue>> {
        Box::pin(async { Ok(vec![]) })
    }
}

async fn replay_run_case(case: &JsValue) -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("pdrs-turn-read-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let registry_path = root.join("registry.json");
    let file = FileRunRegistry::open(&registry_path).unwrap();
    for seed in array(sub(case, "setup"), "seed") {
        let run = run_ref(seed);
        file.accept(&RunAcceptInput {
            run: run.clone(),
            request_material: text(seed, "material").cloned().unwrap(),
        })
        .unwrap();
        for event in array(seed, "events") {
            file.append(&run, event).unwrap();
        }
    }
    let reader: Option<Arc<dyn TurnRunReader>> = if matches!(
        sub(case, "setup").get("noRegistry"),
        Some(JsValue::Bool(true))
    ) {
        None
    } else if let Some(failure) = sub(case, "setup").get("registryFailure") {
        Some(Arc::new(FailingReader {
            code: text(failure, "code").unwrap().to_utf8().unwrap(),
            message: text(failure, "message").unwrap().to_utf8().unwrap(),
        }))
    } else if matches!(
        sub(case, "setup").get("inMemory"),
        Some(JsValue::Bool(true))
    ) {
        Some(Arc::new(InMemoryReader {
            records: Mutex::new(
                array(case, "calls")
                    .iter()
                    .map(|call| {
                        call.get("record")
                            .cloned()
                            .expect("in-memory call needs a record")
                    })
                    .collect(),
            ),
        }))
    } else {
        Some(Arc::new(FileRegistry::new(file).unwrap()))
    };
    let mut outcomes = Vec::new();
    for call in array(case, "calls") {
        apply_call_snapshot_surgery(&registry_path, call);
        let input = sub(call, "input");
        let outcome = match text(call, "method").unwrap().equals_str("runEvents") {
            false => {
                let result = if text(call, "method").unwrap().equals_str("runGet") {
                    run_get(reader.as_deref(), &run_ref(input)).await
                } else {
                    run_reattach(reader.as_deref(), &run_ref(input)).await
                };
                record_outcome(result)
            }
            true => {
                let input = RunEventsInput {
                    run: run_ref(input),
                    after_seq: input.get("afterSeq").cloned(),
                    limit: input.get("limit").cloned(),
                };
                match run_events(reader.as_deref(), &input).await {
                    Ok(events) => {
                        let mut outcome = JsObject::new();
                        outcome.insert("keys".into(), keys(&events));
                        outcome.insert("result".into(), events);
                        JsValue::Object(outcome)
                    }
                    Err(error) => {
                        let mut outcome = JsObject::new();
                        outcome.insert("error".into(), error_value(&error));
                        JsValue::Object(outcome)
                    }
                }
            }
        };
        outcomes.push(outcome);
    }
    let (actual, expected) = (
        render(&JsValue::Array(outcomes)),
        render(sub(case, "outcomes")),
    );
    if actual != expected {
        return Err(format!(
            "{}\nactual: {}\nexpected: {}",
            text(case, "name").unwrap().to_utf8().unwrap(),
            actual,
            expected
        ));
    }
    drop(reader);
    std::fs::remove_dir_all(&root).unwrap();
    Ok(())
}

async fn replay_named(kind: &str, name: &str) {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let case = array(&oracle, kind)
        .iter()
        .find(|case| text(case, "name").is_some_and(|case| case.equals_str(name)))
        .unwrap_or_else(|| panic!("no {kind} case named {name}"));
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        match kind {
            "readCases" => replay_read_case(case).await,
            "snapshotCases" => replay_snapshot_case(case),
            "runCases" => replay_run_case(case).await,
            other => panic!("unknown case group {other}"),
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{name} deadlocked"));
    result.unwrap();
}
macro_rules! scenario {
    ($test:ident, $kind:literal, $name:literal) => {
        #[tokio::test]
        async fn $test() {
            replay_named($kind, $name).await;
        }
    };
}
scenario!(
    read_stable_active_merges_history_and_stream,
    "readCases",
    "read-stable-active-merges-history-and-stream"
);
scenario!(
    read_stable_missing_turn,
    "readCases",
    "read-stable-missing-turn"
);
scenario!(
    read_terminal_during_read_retries_then_merges_terminal_stream,
    "readCases",
    "read-terminal-during-read-retries-then-merges-terminal-stream"
);
scenario!(
    read_replaced_run_during_read_retries,
    "readCases",
    "read-replaced-run-during-read-retries"
);
scenario!(
    read_epoch_never_settles_error,
    "readCases",
    "read-epoch-never-settles-error"
);
scenario!(
    read_history_error_propagates_without_retry,
    "readCases",
    "read-history-error-propagates-without-retry"
);
scenario!(read_not_configured, "readCases", "read-not-configured");
scenario!(
    read_history_stream_key_replaced_in_place,
    "readCases",
    "read-history-stream-key-replaced-in-place"
);
scenario!(
    snapshot_delegation_events_and_states,
    "snapshotCases",
    "snapshot-delegation-events-and-states"
);
scenario!(
    run_get_maps_durable_records,
    "runCases",
    "run-get-maps-durable-records"
);
scenario!(
    run_get_missing_cross_session_and_cross_project,
    "runCases",
    "run-get-missing-cross-session-and-cross-project"
);
scenario!(run_events_paging, "runCases", "run-events-paging");
scenario!(
    run_events_validation_and_not_found,
    "runCases",
    "run-events-validation-and-not-found"
);
scenario!(
    run_capability_unavailable,
    "runCases",
    "run-capability-unavailable"
);
scenario!(
    run_registry_failure_propagates,
    "runCases",
    "run-registry-failure-propagates"
);
scenario!(
    run_events_non_numeric_last_seq,
    "runCases",
    "run-events-non-numeric-last-seq"
);
scenario!(
    run_corrupt_snapshot_errors,
    "runCases",
    "run-corrupt-snapshot-errors"
);
scenario!(
    run_not_found_lone_surrogate_runid,
    "runCases",
    "run-not-found-lone-surrogate-runid"
);
scenario!(
    run_events_toprimitive_throw_last_seq,
    "runCases",
    "run-events-toprimitive-throw-last-seq"
);
scenario!(
    run_events_deep_array_last_seq,
    "runCases",
    "run-events-deep-array-last-seq"
);
#[test]
#[ignore = "requires fixed TS baseline and its existing tsx installation"]
fn real_ts_turn_read_regenerates_identically() {
    crate::test_baseline::regenerates_identically("turn-read");
}

/// A7.2-READ-LASTSEQ-DEPTH-001: the pathological-depth criterion of the
/// decision card — at 100,000 nesting layers the ungated iterative join (see
/// [`last_seq_to_number`]) returns bounded, neither overflowing the Rust
/// stack nor hanging. TS already threw its V8 `RangeError` around 2,540
/// layers (measured, caller-stack dependent), and this side deliberately
/// does not replicate that cliff, so there is no TS golden past the depths
/// the oracle pins; the child keeps the 2001-layer scenario's result shape
/// (`gap:true`, the stored deep `lastSeq` preserved as `nextSeq`).
///
/// The probe runs in a subprocess because an in-process timeout cannot see
/// it: `InMemoryReader::get` returns an immediately ready future, so the
/// join, the clone, and the gap comparison all run synchronously inside one
/// poll and never yield — a `tokio::time::timeout` only fires between polls
/// (Tokio 1.48 contract), so a slow-but-correct or spinning conversion would
/// slip past it. The parent restarts this test binary for the child below
/// and enforces a killable wall-clock deadline instead; the child must also
/// print the completion marker, so a filter that matches nothing (this is a
/// unit-test binary, where `--exact` needs the full module path) cannot pass
/// as a vacuous exit-0.
/// #942 fix-r2 (r3 review Claude-concurrency P2): the bounded probe and its
/// single-thread ambient regression each race a 100,000-layer child; run in
/// parallel they contend for the same CPU/memory and the 10 s wall-clock
/// deadline flakes under load. Serialize the two within this test binary
/// (the ambient regression's inner re-run is a separate process, so it only
/// ever shares this lock with its own siblings — never with itself).
static DEPTH_PROBE_SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn run_events_pathological_depth_last_seq_is_bounded() {
    // Held for the whole probe, including the child wait, so the ambient
    // regression below cannot start its own heavy child until this one is
    // reaped. A poisoned lock means one of the two already panicked.
    let _serial = DEPTH_PROBE_SERIAL
        .lock()
        .expect("#942 depth-probe serialization lock");
    // Reap on every exit path, including failed try_wait and assertion
    // unwinding.
    struct ReapedChild(Child);
    impl Drop for ReapedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    const DEADLINE: Duration = Duration::from_secs(10);
    const CHILD: &str = "runtime::turn::read::tests::run_events_pathological_depth_last_seq_child";
    const MARKER: &str = "PDRS_LASTSEQ_DEPTH_CHILD_OK";
    let start = Instant::now();
    let mut child = ReapedChild(
        Command::new(std::env::current_exe().expect("#942 lastseq-depth test binary path"))
            // #942 fix-r1: pin the multithreaded libtest layout. Single-threaded
            // libtest prints `test <name> ... ` before the body runs, so the
            // child's marker lands inside the receipt line under an ambient
            // RUST_TEST_THREADS=1; the explicit flag overrides that env default
            // and the name+result line is emitted atomically after the body.
            .args([
                "--exact",
                CHILD,
                "--ignored",
                "--nocapture",
                "--test-threads=2",
            ])
            .env("PDRS_TURN_READ_LASTSEQ_DEPTH_CHILD", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("#942 lastseq-depth child spawn"),
    );
    let status = loop {
        match child.0.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => panic!("pathological-depth child wait failed: {error}"),
        }
        assert!(
            start.elapsed() < DEADLINE,
            "100,000-layer lastSeq join exceeded 10 s; child killed and reaped"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = child
        .0
        .stdout
        .take()
        .map(|mut pipe| {
            use std::io::Read;
            let mut out = String::new();
            let _ = pipe.read_to_string(&mut out);
            out
        })
        .unwrap_or_default();
    let stderr = child
        .0
        .stderr
        .take()
        .map(|mut pipe| {
            use std::io::Read;
            let mut out = String::new();
            let _ = pipe.read_to_string(&mut out);
            out
        })
        .unwrap_or_default();
    assert!(
        status.success(),
        "100,000-layer lastSeq join child failed: {status}\n{stderr}"
    );
    // #942: exit 0 alone is not a receipt — a renamed child makes this
    // `--exact` filter match nothing and libtest still exits 0.
    assert!(
        crate::test_exact_receipt::single_pass(&status, CHILD, &stdout),
        "#942 lastseq-depth rerun matched no test: {status}\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains(MARKER),
        "pathological-depth child exited without the completion marker; \
         the probe itself did not run (empty or wrong filter?)\n{stdout}{stderr}"
    );
    assert!(
        start.elapsed() < DEADLINE,
        "100,000-layer lastSeq join exceeded 10 s"
    );
}

/// #942 fix-r1 regression (r2 review Codex P1): an ambient
/// `RUST_TEST_THREADS=1` used to break the child receipt above —
/// single-threaded libtest prints `test <name> ... ` before the body runs,
/// so the child's marker spliced itself into the line `single_pass` keys on
/// and the parent failed at its `#942 lastseq-depth rerun matched no test`
/// assertion while the child had actually passed (`1 passed`, exit 0). Every
/// receipt-fed rerun now pins `--test-threads=2`; this re-runs the whole
/// parent test with the hostile ambient variable set, exactly the original
/// failing command, so a regression back to the env-default layout fails
/// here instead of on a quiet single-threaded CI box.
#[test]
fn run_events_pathological_depth_last_seq_is_bounded_under_single_thread_ambient() {
    // See DEPTH_PROBE_SERIAL above: same-binary serialization with the heavy
    // probe (the inner re-run takes the lock in its own process, never here).
    let _serial = DEPTH_PROBE_SERIAL
        .lock()
        .expect("#942 depth-probe serialization lock");
    const PARENT: &str =
        "runtime::turn::read::tests::run_events_pathological_depth_last_seq_is_bounded";
    let output =
        Command::new(std::env::current_exe().expect("#942 lastseq-depth ambient test binary"))
            .args(["--exact", PARENT, "--nocapture"])
            .env("RUST_TEST_THREADS", "1")
            .stdin(Stdio::null())
            .output()
            .expect("#942 lastseq-depth ambient child spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        crate::test_exact_receipt::single_pass(&output.status, PARENT, &stdout),
        "#942 fix-r1 single-thread ambient broke the receipt: {}\n{stdout}\n{stderr}",
        output.status
    );
}

/// Subprocess half of [`run_events_pathological_depth_last_seq_is_bounded`]:
/// only ever spawned by the parent with `PDRS_TURN_READ_LASTSEQ_DEPTH_CHILD`
/// set, so a direct run — including `--include-ignored` — fails fast instead
/// of counting as a standalone pass. Keeps the result assertions the parent
/// owes the decision card: `gap:true` with the stored deep `lastSeq` returned
/// unchanged as `nextSeq`. The deep value is built, cloned, and compared
/// without recursion: construction wraps bottom-up in a loop, and
/// `JsValue`'s Clone / Drop / PartialEq are already iterative.
#[tokio::test]
#[ignore = "subprocess helper for the bounded pathological-depth probe"]
async fn run_events_pathological_depth_last_seq_child() {
    std::env::var_os("PDRS_TURN_READ_LASTSEQ_DEPTH_CHILD").expect("only invoked by parent");
    fn deep_array(layers: usize) -> JsValue {
        let mut value = JsValue::Array(vec![JsValue::Number(1.0)]);
        for _ in 1..layers {
            value = JsValue::Array(vec![value]);
        }
        value
    }
    let record = {
        let mut record = JsObject::new();
        record.insert("sessionKey".into(), "s".into());
        record.insert("projectKey".into(), "p".into());
        record.insert("runId".into(), "r1".into());
        record.insert("state".into(), "running".into());
        record.insert("revision".into(), JsValue::Number(0.0));
        record.insert("lastSeq".into(), deep_array(100_000));
        record.insert("acceptedAt".into(), JsValue::Number(0.0));
        record.insert("updatedAt".into(), JsValue::Number(0.0));
        JsValue::Object(record)
    };
    let reader = InMemoryReader {
        records: Mutex::new(vec![record].into()),
    };
    let input = RunEventsInput {
        run: RunRef {
            session_key: "s".into(),
            run_id: "r1".into(),
            project_key: Some("p".into()),
        },
        after_seq: Some(JsValue::Number(6.0)),
        limit: None,
    };
    let result = run_events(Some(&reader), &input)
        .await
        .expect("deep lastSeq gap result");
    let expected = {
        let mut gap = JsObject::new();
        gap.insert("events".into(), JsValue::Array(vec![]));
        gap.insert("nextSeq".into(), deep_array(100_000));
        gap.insert("gap".into(), JsValue::Bool(true));
        JsValue::Object(gap)
    };
    assert_eq!(result, expected);
    // The parent asserts on this marker, so an exit-0 that never ran the
    // probe (empty `--exact` filter) cannot count as a pass.
    println!("PDRS_LASTSEQ_DEPTH_CHILD_OK");
}

// ─────────────────────────────── concurrency
// The golden corpus is a single-threaded replay; these pin the two port-only
// promises of the module note: a read holds no lock across the history await
// (a concurrent publish proceeds), and an epoch that settles under a live
// publication is retried to a consistent merged snapshot without deadlock.

/// The history read parks on a gate; while it is parked, a concurrent
/// publisher must still be able to record and emit through the coordinator
/// and the publication queue. If the read held the coordinator or replay
/// state lock across the await, the emitter would deadlock against it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_session_messages_does_not_block_concurrent_publish_while_history_is_pending() {
    struct GatedHistory {
        gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        calls: AtomicUsize,
    }
    impl SessionHistoryReader for GatedHistory {
        fn read(
            &self,
            _input: &JsValue,
        ) -> Pin<Box<dyn Future<Output = Result<JsValue, TurnReadError>> + Send>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let gate = self.gate.lock().unwrap().take().expect("one planned read");
            Box::pin(async move {
                gate.await.expect("gate released");
                Ok(js(&json!({"messages": [{"id": "m1"}], "total": 1})))
            })
        }
    }
    let coordinator = Arc::new(TurnEventCoordinator::new());
    let stream = Arc::new(TurnPublisher::start(
        RunRef {
            session_key: "s".into(),
            run_id: "r".into(),
            project_key: None,
        },
        None,
        coordinator.clone(),
    ));
    let (release, gate) = tokio::sync::oneshot::channel();
    let history = Arc::new(GatedHistory {
        gate: Mutex::new(Some(gate)),
        calls: AtomicUsize::new(0),
    });
    let reader = {
        let coordinator = coordinator.clone();
        let history = history.clone();
        let input = js(&json!({"sessionKey": "s"}));
        tokio::spawn(async move {
            read_session_messages(&coordinator, Some(history.as_ref()), &input)
                .await
                .expect("stable epoch read")
        })
    };
    // Park the reader first: its before-snapshot and the await of the gated
    // history read must both have happened before the emitter starts.
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    let emitter = {
        let stream = stream.clone();
        tokio::spawn(async move {
            for index in 0..32 {
                assert!(stream.gateway_event(js(
                    &json!({"type":"agent_status","runId":"r","event":"pump","index":index})
                )));
            }
            for index in 0..32 {
                assert!(stream.emit(&js(
                    &json!({"type":"agent_status","event":"external","index":index})
                )));
            }
        })
    };
    // The emitter must finish while the reader is still parked on the gate.
    tokio::time::timeout(Duration::from_secs(10), emitter)
        .await
        .expect("concurrent publish was blocked by the pending read")
        .unwrap();
    release.send(()).expect("reader still waiting");
    let result = tokio::time::timeout(Duration::from_secs(10), reader)
        .await
        .expect("read deadlocked")
        .unwrap();
    assert_eq!(history.calls.load(Ordering::SeqCst), 1);
    let result_keys = json(&keys(&result));
    assert_eq!(result_keys, json!(["messages", "total", "stream"]));
    let result = json(&result);
    let events = result["stream"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 64);
    // The pump events keep their recorded order; the external emits carry the
    // publisher's run id, exactly like a concurrent TS host.
    assert_eq!(events[0]["event"], json!("pump"));
    assert_eq!(events[32]["runId"], json!("r"));
    assert_eq!(events[32]["event"], json!("external"));
    drop(stream);
}

/// A turn that settles while the durable history is being read is retried to
/// a merged terminal snapshot, through the real publication pipeline and the
/// durable registry — no deadlock, and the result's stream is the terminal
/// replay the drain left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_session_messages_settles_when_concurrent_publish_finishes_the_turn() {
    struct SettlingHistory {
        settle: Mutex<Option<tokio::task::JoinHandle<()>>>,
        calls: AtomicUsize,
    }
    impl SessionHistoryReader for SettlingHistory {
        fn read(
            &self,
            _input: &JsValue,
        ) -> Pin<Box<dyn Future<Output = Result<JsValue, TurnReadError>> + Send>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                let settle = self.settle.lock().unwrap().take().expect("planned settle");
                Box::pin(async move {
                    settle.await.expect("settling task");
                    Ok(js(
                        &json!({"messages": [{"id": "m1"}, {"id": "m2"}], "total": 2}),
                    ))
                })
            } else {
                Box::pin(async {
                    Ok(js(
                        &json!({"messages": [{"id": "m1"}, {"id": "m2"}], "total": 2}),
                    ))
                })
            }
        }
    }
    let root = std::env::temp_dir().join(format!("pdrs-turn-read-settle-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = FileRunRegistry::open(root.join("registry.json")).unwrap();
    let run = RunRef {
        session_key: "s".into(),
        run_id: "r".into(),
        project_key: None,
    };
    file.accept(&RunAcceptInput {
        run: run.clone(),
        request_material: "{\"message\":\"hello\"}".into(),
    })
    .unwrap();
    let registry = Arc::new(FileRegistry::new(file).unwrap());
    let coordinator = Arc::new(TurnEventCoordinator::new());
    let stream = Arc::new(TurnPublisher::start(
        run,
        Some(registry.clone()),
        coordinator.clone(),
    ));
    let settle = {
        let stream = stream.clone();
        tokio::spawn(async move {
            assert!(stream.gateway_event(js(
                &json!({"type":"agent_status","runId":"r","event":"tick"})
            )));
            let finisher = {
                let stream = stream.clone();
                tokio::spawn(async move { stream.finish().await })
            };
            while stream.next().await.is_some() {}
            finisher.await.expect("finisher");
        })
    };
    let history = SettlingHistory {
        settle: Mutex::new(Some(settle)),
        calls: AtomicUsize::new(0),
    };
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        read_session_messages(
            &coordinator,
            Some(&history),
            &js(&json!({"sessionKey": "s"})),
        ),
    )
    .await
    .expect("read deadlocked against the concurrent publish")
    .expect("epoch settles within three attempts");
    assert_eq!(history.calls.load(Ordering::SeqCst), 2);
    let result_keys = json(&keys(&result));
    assert_eq!(result_keys, json!(["messages", "total", "stream"]));
    let result = json(&result);
    assert_eq!(result["messages"].as_array().unwrap().len(), 2);
    assert_eq!(result["stream"]["terminal"], json!(true));
    assert_eq!(result["stream"]["runId"], json!("r"));
    assert_eq!(
        result["stream"]["events"],
        json!([{"type": "agent_status", "runId": "r", "event": "tick"}])
    );
    drop(stream);
    drop(registry);
    std::fs::remove_dir_all(&root).unwrap();
}
