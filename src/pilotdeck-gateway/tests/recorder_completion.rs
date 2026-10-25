//! #741 independently replayable durable completion slice; host assembly is pending.
#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::storage::{
    JsonlTranscriptStore, StoreResult, TranscriptKey, TranscriptRead, TranscriptStore,
    recorder::{Recorder, RecorderError},
};
use pilotdeck_protocol::{JsValue, parse_json};
use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

struct FaultStore {
    disk: JsonlTranscriptStore,
    kind: JsValue,
    remaining: Mutex<usize>,
}
impl TranscriptStore for FaultStore {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        let mut remaining = self.remaining.lock().unwrap();
        if entry.get("type") == Some(&self.kind) && *remaining > 0 {
            *remaining -= 1;
            return Err(std::io::Error::other("injected write failure").into());
        }
        self.disk.append(key, entry)
    }
    fn read(&self, key: &TranscriptKey, limit: Option<u64>) -> StoreResult<TranscriptRead> {
        self.disk.read(key, limit)
    }
}
fn text(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn replay(name: &str) {
    let oracle = baseline::oracle(include_str!("fixtures/turn-runner/oracle.json"));
    let JsValue::Array(cases) = oracle.get("recorderCases").unwrap() else {
        panic!("cases")
    };
    let row = cases
        .iter()
        .find(|row| row.get("name") == Some(&name.into()))
        .unwrap();
    let scratch = std::env::temp_dir().join(format!("pdrs-completion-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&scratch).unwrap();
    let key = TranscriptKey {
        project_root: scratch.clone(),
        pilot_home: scratch.join("home"),
        session_id: "s".into(),
        transcript_path: scratch.join("s.jsonl"),
    };
    let store = Arc::new(FaultStore {
        disk: JsonlTranscriptStore::new(),
        kind: row.get("fault").unwrap().clone(),
        remaining: Mutex::new(row.get("failures").unwrap().as_number().unwrap() as usize),
    });
    let ids = AtomicUsize::new(0);
    let recorder = Recorder::with_sources(
        store.clone(),
        key.clone(),
        Arc::new(|| Ok("2026-10-26T00:00:00.000Z".into())),
        Arc::new(move || Ok(format!("id-{}", ids.fetch_add(1, Ordering::SeqCst) + 1))),
    );
    let JsValue::Array(operations) = row.get("operations").unwrap() else {
        panic!("operations")
    };
    let mut outcomes = Vec::new();
    for op in operations {
        let turn = op.get("turn").cloned().unwrap_or_else(|| "t".into());
        let result = match text(op.get("op").unwrap()).as_str() {
            "start" => recorder.start_turn(&"s".into(), turn.as_string().unwrap()),
            "request" => recorder
                .record_model_request(
                    &"s".into(),
                    turn.as_string().unwrap(),
                    parse_json(r#"{"messages":[]}"#).unwrap(),
                )
                .map(|_| ()),
            "complete" => {
                let value = op.get("result").unwrap();
                recorder.complete_turn(
                    value.get("sessionId").unwrap().as_string().unwrap(),
                    value.get("turnId").unwrap().as_string().unwrap(),
                    value.clone(),
                )
            }
            "invalid-complete" => {
                let value = match text(op.get("kind").unwrap()).as_str() {
                    "null" => JsValue::Null,
                    "undefined" => JsValue::Undefined,
                    "number" => JsValue::Number(42.0),
                    other => panic!("unknown malformed value {other}"),
                };
                let before = fs::read(&key.transcript_path).unwrap_or_default();
                let outcome = recorder.complete_turn(&"s".into(), &"t".into(), value);
                assert_eq!(
                    fs::read(&key.transcript_path).unwrap_or_default(),
                    before,
                    "{name}: malformed completion must not write"
                );
                match text(op.get("kind").unwrap()).as_str() {
                    "null" | "undefined" => assert!(matches!(outcome, Err(RecorderError::Type(_)))),
                    "number" if outcome.is_err() => {
                        assert!(matches!(outcome, Err(RecorderError::State(_))))
                    }
                    _ => {}
                }
                outcome
            }
            "restore" => recorder.restore(&store.read(&key, None).unwrap().entries),
            other => panic!("unknown operation {other}"),
        };
        outcomes.push(JsValue::String(
            result
                .map_or_else(|e| e.to_string(), |_| "ok".into())
                .into(),
        ));
    }
    assert_eq!(
        JsValue::Array(outcomes),
        *row.get("outcomes").unwrap(),
        "{name}: outcomes"
    );
    assert_eq!(
        fs::read(&key.transcript_path).unwrap_or_default(),
        text(row.get("transcript").unwrap()).as_bytes(),
        "{name}: JSONL bytes"
    );
    fs::remove_dir_all(scratch).unwrap();
}
macro_rules! scenario {
    ($test:ident, $name:literal) => {
        #[test]
        fn $test() {
            replay($name);
        }
    };
}
scenario!(completion_success, "success");
scenario!(completion_error, "error");
scenario!(completion_aborted, "aborted");
scenario!(completion_max_turns, "max_turns");
scenario!(completion_without_step, "no-step");
scenario!(completion_without_start, "unstarted");
scenario!(completion_step_write_retry, "step-once");
scenario!(completion_result_write_retry, "result-once");
scenario!(completion_persistent_failure, "result-always");
scenario!(completion_new_turn_resets_deduplication, "new-turn");
scenario!(completion_restore_clears_deduplication, "restore");
scenario!(
    completion_failed_start_preserves_deduplication,
    "restart-fails"
);
scenario!(completion_null_fresh, "invalid-null-fresh");
scenario!(completion_null_started, "invalid-null-started");
scenario!(completion_null_completed, "invalid-null-completed");
scenario!(completion_undefined_fresh, "invalid-undefined-fresh");
scenario!(completion_undefined_started, "invalid-undefined-started");
scenario!(
    completion_undefined_completed,
    "invalid-undefined-completed"
);
scenario!(completion_number_fresh, "invalid-number-fresh");
scenario!(completion_number_started, "invalid-number-started");
scenario!(completion_number_completed, "invalid-number-completed");
#[test]
#[ignore = "requires the pinned TS baseline and Node 22/tsx; spawns real stdio sidecars"]
fn turn_runner_regenerates_identically() {
    baseline::regenerates_identically("turn-runner");
}
