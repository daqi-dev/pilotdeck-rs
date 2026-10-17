use super::ledger::field;
use super::*;
use pilotdeck_protocol::{
    JsObject, JsString, JsValue,
    module::{ModuleBinding, ModuleOutcome},
    parse_json, stringify_json,
};
use std::{
    fs, io,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/operation-ledger"
);
fn oracle() -> JsValue {
    parse_json(include_str!(
        "../../tests/fixtures/operation-ledger/oracle.json"
    ))
    .unwrap()
}
fn id() -> OperationIdentity {
    OperationIdentity {
        run_id: "run-1".into(),
        operation_id: "op-1".into(),
        request_id: "req-1".into(),
        session_id: "session-1".into(),
        turn_id: "turn-1".into(),
        binding: ModuleBinding {
            module_instance_id: "module-1".into(),
            connection_generation: "generation-1".into(),
        },
        idempotency_key: Some("once".into()),
    }
}
fn known() -> KnownTerminal {
    let fixture = oracle();
    let value = field(field(&fixture, "inputs"), "known");
    KnownTerminal {
        identity: id(),
        stream_id: Some("stream-1".into()),
        last_applied_sequence: 4.0,
        resolution: OperationResolution {
            outcome: ModuleOutcome::Completed,
            result: field(value, "result").clone(),
            messages: vec![],
            seed_state: value.get("seedState").cloned(),
        },
        code: Some("OK".into()),
        error: Some(JsValue::Null),
    }
}
fn unknown() -> UnknownTerminal {
    UnknownTerminal {
        identity: id(),
        stream_id: "stream-1".into(),
        last_applied_sequence: Some(2.0),
        code: Some("DISCONNECTED".into()),
        error: Some(parse_json(r#"{"message":"lost"}"#).unwrap()),
    }
}
#[derive(Default)]
struct Memory {
    entries: Vec<JsValue>,
    fail: bool,
}
impl TranscriptSink for Memory {
    fn append(&mut self, session: &JsString, turn: &JsString, mut event: JsObject) -> Result<()> {
        if std::mem::take(&mut self.fail) {
            return Err(io::Error::other("injected append failure").into());
        }
        event.insert("sessionId".into(), JsValue::String(session.clone()));
        event.insert("turnId".into(), JsValue::String(turn.clone()));
        self.entries.push(JsValue::Object(event));
        Ok(())
    }
}
fn memory() -> SessionOperationLedger<Memory> {
    SessionOperationLedger::new(id().session_id, Memory::default(), &[]).unwrap()
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../_tmp/20261017-a62/test-scratch")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> PathBuf {
        self.0.join("session.jsonl")
    }
    fn fixture(&self, name: &str) {
        fs::write(
            self.path(),
            fs::read(format!("{FIXTURES}/{name}.jsonl")).unwrap(),
        )
        .unwrap();
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn string(v: &JsValue) -> String {
    v.as_string().unwrap().to_utf8().unwrap()
}
fn recovery_json(recovered: Option<OperationRecovery>) -> JsValue {
    match recovered {
        None => JsValue::Null,
        Some(OperationRecovery::Incomplete) => parse_json(r#"{"state":"incomplete"}"#).unwrap(),
        Some(OperationRecovery::Terminal(resolution)) => {
            let mut r = JsObject::new();
            r.insert(
                "outcome".into(),
                match resolution.outcome {
                    ModuleOutcome::Completed => "completed",
                    ModuleOutcome::Failed => "failed",
                    ModuleOutcome::Cancelled => "cancelled",
                    _ => panic!(),
                }
                .into(),
            );
            r.insert("result".into(), resolution.result);
            r.insert("messages".into(), JsValue::Array(resolution.messages));
            if let Some(seed) = resolution.seed_state {
                r.insert("seedState".into(), seed);
            }
            crate::metadata::object([
                ("state", "terminal".into()),
                ("resolution", JsValue::Object(r)),
            ])
        }
        Some(OperationRecovery::ResultUnknown(terminal)) => {
            let id = terminal.identity;
            let mut input = JsObject::new();
            for (key, value) in [
                ("runId", id.run_id),
                ("operationId", id.operation_id),
                ("requestId", id.request_id),
                ("sessionId", id.session_id),
                ("turnId", id.turn_id),
            ] {
                input.insert(key.into(), JsValue::String(value));
            }
            input.insert(
                "binding".into(),
                crate::metadata::object([
                    (
                        "moduleInstanceId",
                        JsValue::String(id.binding.module_instance_id),
                    ),
                    (
                        "connectionGeneration",
                        JsValue::String(id.binding.connection_generation),
                    ),
                ]),
            );
            if let Some(key) = id.idempotency_key {
                input.insert("idempotencyKey".into(), JsValue::String(key));
            }
            input.insert("streamId".into(), JsValue::String(terminal.stream_id));
            input.insert(
                "lastAppliedSequence".into(),
                terminal
                    .last_applied_sequence
                    .map(JsValue::Number)
                    .unwrap_or(JsValue::Null),
            );
            if let Some(code) = terminal.code {
                input.insert("code".into(), JsValue::String(code));
            }
            if let Some(error) = terminal.error {
                input.insert("error".into(), error);
            }
            crate::metadata::object([
                ("state", "result_unknown".into()),
                ("unknown", JsValue::Object(input)),
            ])
        }
    }
}
#[test]
fn real_ts_fixtures_restore_records_diagnostics_and_sequence() {
    let fixtures = oracle();
    let JsValue::Array(cases) = field(&fixtures, "cases") else {
        panic!()
    };
    for case in cases {
        let scratch = Scratch::new();
        let name = string(field(case, "name"));
        scratch.fixture(&name);
        let ledger = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
        assert_eq!(
            recovery_json(ledger.recover(&id()).unwrap()),
            *field(case, "recovery"),
            "{name}"
        );
        let state = ledger.sink().snapshot_state();
        assert_eq!(
            state.sequence as f64,
            field(field(case, "state"), "sequence").as_number().unwrap(),
            "{name}"
        );
        assert_eq!(
            state.last_entry_id.as_ref(),
            field(field(case, "state"), "lastEntryId").as_string(),
            "{name}"
        );
        let JsValue::Array(expected) = field(case, "diagnostics") else {
            panic!()
        };
        assert_eq!(ledger.sink().diagnostics().len(), expected.len(), "{name}");
        for (actual, expected) in ledger.sink().diagnostics().iter().zip(expected) {
            assert_eq!(actual.code, string(field(expected, "code")));
            assert_eq!(actual.severity, string(field(expected, "severity")));
            assert_eq!(
                actual.line.unwrap() as f64,
                field(expected, "line").as_number().unwrap()
            );
        }
    }
}
#[test]
fn duplicate_operations_and_attempt_fences_match_ts() {
    let mut l = memory();
    let input = id();
    assert!(l.accept(&input, &"stream-1".into()).is_err());
    l.start(&input).unwrap();
    l.start(&input).unwrap();
    let mut retry = input.clone();
    retry.request_id = "request-2".into();
    retry.binding.connection_generation = "generation-2".into();
    assert!(l.start(&retry).is_err());
    assert!(l.accept(&retry, &"stream-1".into()).is_err());
    assert_eq!(
        l.recover(&retry).unwrap(),
        Some(OperationRecovery::Incomplete)
    );
    l.accept(&input, &"stream-1".into()).unwrap();
    l.accept(&input, &"stream-1".into()).unwrap();
    assert!(l.accept(&input, &"different".into()).is_err());
    l.terminal(&known()).unwrap();
    l.terminal(&known()).unwrap();
    assert_eq!(l.sink().entries.len(), 3);
    assert!(matches!(
        l.recover(&retry).unwrap(),
        Some(OperationRecovery::Terminal(_))
    ));
    for changed in ["run", "turn", "key", "operation"] {
        let mut different = input.clone();
        match changed {
            "run" => different.run_id = "other".into(),
            "turn" => different.turn_id = "other".into(),
            "key" => different.idempotency_key = None,
            _ => different.operation_id = "other".into(),
        }
        assert_eq!(l.recover(&different).unwrap(), None);
    }
    retry.session_id = "other".into();
    assert!(l.recover(&retry).is_err());
    assert!(l.start(&retry).is_err());
    assert!(
        l.terminal(&KnownTerminal {
            identity: retry.clone(),
            ..known()
        })
        .is_err()
    );
    assert!(
        l.result_unknown(&UnknownTerminal {
            identity: retry.clone(),
            ..unknown()
        })
        .is_err()
    );
    assert!(
        l.reconcile(&UnknownTerminal {
            identity: retry,
            ..unknown()
        })
        .is_err()
    );
}
#[test]
fn recovery_admission_does_not_execute_a_durable_operation_again() {
    // Storage contract used by TS client:594-632. This is not host integration.
    let scratch = Scratch::new();
    let mut executed = 0;
    {
        let mut l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
        if l.recover(&id()).unwrap().is_none() {
            l.start(&id()).unwrap();
            executed += 1;
        }
        l.accept(&id(), &"stream-1".into()).unwrap();
        l.terminal(&known()).unwrap();
    }
    let l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
    let mut retry = id();
    retry.request_id = "restart".into();
    if l.recover(&retry).unwrap().is_none() {
        executed += 1;
    }
    assert_eq!(executed, 1);
    assert!(matches!(
        l.recover(&retry).unwrap(),
        Some(OperationRecovery::Terminal(_))
    ));
    for name in ["incomplete", "unknown"] {
        let scratch = Scratch::new();
        scratch.fixture(name);
        let l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
        assert!(
            l.recover(&retry).unwrap().is_some(),
            "{name} must block re-execution"
        );
    }
}
#[test]
fn unknown_known_transitions_and_reconciliation_are_distinct() {
    let mut l = memory();
    l.start(&id()).unwrap();
    l.accept(&id(), &"stream-1".into()).unwrap();
    l.result_unknown(&unknown()).unwrap();
    l.result_unknown(&unknown()).unwrap();
    let mut changed = unknown();
    changed.error = None;
    assert!(l.result_unknown(&changed).is_err());
    assert!(l.reconcile(&unknown()).unwrap().is_none());
    l.terminal(&known()).unwrap();
    assert!(l.reconcile(&unknown()).unwrap().is_some());
    l.result_unknown(&changed).unwrap();
    assert!(matches!(
        l.recover(&id()).unwrap(),
        Some(OperationRecovery::Terminal(_))
    ));
    let mut known_changed = known();
    known_changed.code = None;
    known_changed.error = None;
    l.terminal(&known_changed).unwrap();
    assert_eq!(l.sink().entries.len(), 4);
    known_changed.last_applied_sequence += 1.0;
    assert!(l.terminal(&known_changed).is_err());
    let mut attempt = unknown();
    attempt.identity.request_id = "other".into();
    assert!(l.reconcile(&attempt).unwrap().is_none());
    let mut no_accept = memory();
    no_accept.start(&id()).unwrap();
    no_accept.result_unknown(&unknown()).unwrap();
    let mut no_stream = known();
    no_stream.stream_id = None;
    no_accept.terminal(&no_stream).unwrap();
    assert!(no_accept.reconcile(&unknown()).unwrap().is_none());
    // Restart learns streamId from the historical unknown event.
    let restored = SessionOperationLedger::new(
        id().session_id,
        Memory::default(),
        &no_accept.sink().entries,
    )
    .unwrap();
    assert!(restored.reconcile(&unknown()).unwrap().is_some());
}
#[test]
fn terminal_json_order_surrogates_and_validation_match_ts() {
    let mut l = memory();
    l.start(&id()).unwrap();
    l.terminal(&known()).unwrap();
    let mut reordered = known();
    let mut result = JsObject::new();
    for (k, v) in reordered
        .resolution
        .result
        .as_object()
        .unwrap()
        .entries()
        .iter()
        .rev()
    {
        result.insert(k.clone(), v.clone());
    }
    reordered.resolution.result = JsValue::Object(result);
    assert!(l.terminal(&reordered).is_err());
    let mut invalid = known();
    invalid.last_applied_sequence = -2.0;
    assert!(l.terminal(&invalid).is_err());
    invalid.last_applied_sequence = 0.5;
    assert!(l.terminal(&invalid).is_err());
    invalid.last_applied_sequence = f64::INFINITY;
    assert!(l.terminal(&invalid).is_err());
    invalid = known();
    invalid.resolution.result = parse_json(r#"{"sessionId":"wrong","turnId":"turn-1"}"#).unwrap();
    assert!(l.terminal(&invalid).is_err());
    let mut unknown_sequence = memory();
    unknown_sequence.start(&id()).unwrap();
    let mut unknown = unknown();
    unknown.last_applied_sequence = Some(-2.5);
    unknown_sequence.result_unknown(&unknown).unwrap();
}
#[test]
fn seed_projection_preserves_duplicates_and_drops_unknown_fields() {
    let seed = parse_json(r#"{"allowedReadFiles":["a","a"],"readFileState":{"a":{"kind":"text","mtimeMs":1,"ignored":true,"offset":"bad"}},"ignored":1}"#).unwrap();
    let normalized = projection::normalize_seed(Some(&seed)).unwrap().unwrap();
    assert_eq!(
        stringify_json(&normalized).unwrap().unwrap(),
        r#"{"allowedReadFiles":["a","a"],"readFileState":{"a":{"mtimeMs":1,"kind":"text"}}}"#
    );
    assert!(
        projection::normalize_seed(Some(&JsValue::Null))
            .unwrap()
            .is_none()
    );
    for bad in [
        r#"{"allowedReadFiles":[1]}"#,
        r#"{"readFileState":{"a":{"mtimeMs":1,"kind":"other"}}}"#,
        r#"{"writeSnapshots":{"a":{"mtimeMs":1}}}"#,
        "[]",
    ] {
        assert!(projection::normalize_seed(Some(&parse_json(bad).unwrap())).is_err());
    }
}
#[test]
fn failed_append_never_publishes_any_operation_transition() {
    let mut l = memory();
    l.sink_mut().fail = true;
    assert!(l.start(&id()).is_err());
    assert_eq!(l.recover(&id()).unwrap(), None);
    l.start(&id()).unwrap();
    l.sink_mut().fail = true;
    assert!(l.accept(&id(), &"stream-1".into()).is_err());
    l.accept(&id(), &"stream-2".into()).unwrap();
    let mut u = unknown();
    u.stream_id = "stream-2".into();
    l.sink_mut().fail = true;
    assert!(l.result_unknown(&u).is_err());
    assert_eq!(
        l.recover(&id()).unwrap(),
        Some(OperationRecovery::Incomplete)
    );
    l.result_unknown(&u).unwrap();
    let mut k = known();
    k.stream_id = Some("stream-2".into());
    l.sink_mut().fail = true;
    assert!(l.terminal(&k).is_err());
    assert!(matches!(
        l.recover(&id()).unwrap(),
        Some(OperationRecovery::ResultUnknown(_))
    ));
    l.terminal(&k).unwrap();
    assert!(matches!(
        l.recover(&id()).unwrap(),
        Some(OperationRecovery::Terminal(_))
    ));
}
#[test]
fn partial_fs_write_and_rename_failure_leave_disk_and_revision_unchanged() {
    for rename in [false, true] {
        let scratch = Scratch::new();
        let mut l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
        l.start(&id()).unwrap();
        let before = fs::read(scratch.path()).unwrap();
        let state = l.sink().snapshot_state();
        if rename {
            l.sink_mut().fail_rename = true;
        } else {
            l.sink_mut().fail_write = true;
        }
        assert!(matches!(
            l.accept(&id(), &"stream-1".into()),
            Err(LedgerError::Io(_))
        ));
        assert_eq!(fs::read(scratch.path()).unwrap(), before);
        assert_eq!(l.sink().snapshot_state(), state);
        assert_eq!(
            l.recover(&id()).unwrap(),
            Some(OperationRecovery::Incomplete)
        );
        assert!(
            fs::read_dir(&scratch.0).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
        l.accept(&id(), &"stream-2".into()).unwrap();
        assert_eq!(l.sink().snapshot_state().sequence, state.sequence + 1);
    }
}
#[test]
fn lock_contention_and_drop_release_cover_canonical_path_aliases() {
    let scratch = Scratch::new();
    let l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
    assert!(
        matches!(JsonlLedgerStore::open(scratch.path(), "session-1"), Err(LedgerError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock)
    );
    fs::create_dir(scratch.0.join("nested")).unwrap();
    assert!(
        matches!(JsonlLedgerStore::open(scratch.0.join("nested/../session.jsonl"), "session-1"), Err(LedgerError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock)
    );
    let inherited_descriptor = l.sink().clone_lock_for_test().unwrap();
    drop(l);
    let _l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
    drop(inherited_descriptor);
    assert!(scratch.0.join("session.jsonl.lock").is_file());
}
#[test]
fn half_written_tail_is_preserved_and_separated_before_append() {
    let scratch = Scratch::new();
    scratch.fixture("partial");
    let original = fs::read(scratch.path()).unwrap();
    let mut l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
    l.result_unknown(&unknown()).unwrap();
    let bytes = fs::read(scratch.path()).unwrap();
    assert!(bytes.starts_with(&original));
    assert_eq!(bytes[original.len()], b'\n');
    let actual = read_transcript(scratch.path()).unwrap();
    let expected = read_transcript(format!("{FIXTURES}/partial-continued.jsonl")).unwrap();
    assert_eq!(actual.diagnostics.len(), expected.diagnostics.len());
    assert_eq!(actual.entries.len(), expected.entries.len());
    for key in [
        "type",
        "sequence",
        "parentEntryId",
        "outcome",
        "lastAppliedSequence",
    ] {
        assert_eq!(
            field(actual.entries.last().unwrap(), key),
            field(expected.entries.last().unwrap(), key)
        );
    }
    drop(l);
    let l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
    assert!(matches!(
        l.recover(&id()).unwrap(),
        Some(OperationRecovery::ResultUnknown(_))
    ));
}
#[test]
fn old_ts_entries_keep_sequence_and_last_available_parent() {
    let scratch = Scratch::new();
    scratch.fixture("legacy");
    let mut l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
    let mut input = id();
    input.operation_id = "op-2".into();
    l.start(&input).unwrap();
    assert_eq!(l.sink().snapshot_state().sequence, 10);
    let entries = read_transcript(scratch.path()).unwrap().entries;
    assert_eq!(
        field(entries.last().unwrap(), "parentEntryId"),
        &JsValue::Null
    );
    drop(l);
    let path = scratch.path();
    let original = fs::read_to_string(&path).unwrap();
    let line = r#"{"type":"session_metadata","sessionId":"session-1","turnId":"","sequence":11,"createdAt":"2026-10-17","metadata":{}}"#;
    fs::write(&path, format!("{original}{line}\n")).unwrap();
    let mut l = JsonlLedgerStore::open(path, "session-1").unwrap();
    let parent = field(entries.last().unwrap(), "entryId")
        .as_string()
        .unwrap();
    assert_eq!(
        l.sink().snapshot_state().last_entry_id.as_ref(),
        Some(parent)
    );
    input.operation_id = "op-3".into();
    l.start(&input).unwrap();
    assert_eq!(l.sink().snapshot_state().sequence, 12);
}
#[test]
fn malformed_history_conflicts_fail_without_rewriting_bytes() {
    for mutation in [
        "identity",
        "stream",
        "terminal",
        "duplicate-sequence",
        "duplicate-id",
        "negative-sequence",
        "missing-result",
    ] {
        let scratch = Scratch::new();
        scratch.fixture("normal");
        let mut entries: Vec<JsValue> = read_transcript(scratch.path()).unwrap().entries;
        let index = if mutation == "identity" { 1 } else { 2 };
        let mut changed = entries[index].as_object().unwrap().clone();
        match mutation {
            "identity" => {
                changed.insert("requestId".into(), "conflict".into());
            }
            "stream" => {
                changed.insert("streamId".into(), "conflict".into());
            }
            "terminal" => {
                changed.insert("sequence".into(), JsValue::Number(4.0));
                changed.insert("entryId".into(), "entry-4".into());
                changed.insert("lastAppliedSequence".into(), JsValue::Number(5.0));
                entries.push(JsValue::Object(changed.clone()));
            }
            "duplicate-sequence" => {
                changed.insert("sequence".into(), JsValue::Number(2.0));
            }
            "duplicate-id" => {
                changed.insert("entryId".into(), "ts-entry-1".into());
            }
            "negative-sequence" => {
                changed.insert("sequence".into(), JsValue::Number(-1.0));
            }
            _ => {
                changed.insert("result".into(), JsValue::Null);
            }
        }
        if mutation != "terminal" {
            entries[index] = JsValue::Object(changed);
        }
        let bytes = entries
            .iter()
            .map(|e| stringify_json(e).unwrap().unwrap() + "\n")
            .collect::<String>();
        fs::write(scratch.path(), &bytes).unwrap();
        assert!(
            JsonlLedgerStore::open(scratch.path(), "session-1").is_err(),
            "{mutation}"
        );
        assert_eq!(fs::read_to_string(scratch.path()).unwrap(), bytes);
    }
}
#[test]
fn outside_writer_changes_are_not_overwritten() {
    let scratch = Scratch::new();
    let mut l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
    l.start(&id()).unwrap();
    let bytes = fs::read_to_string(scratch.path()).unwrap() + "{external debris}\n";
    fs::write(scratch.path(), &bytes).unwrap();
    assert!(l.accept(&id(), &"stream-1".into()).is_err());
    assert_eq!(fs::read_to_string(scratch.path()).unwrap(), bytes);
    assert_eq!(l.sink().snapshot_state().sequence, 1);
}
#[test]
fn empty_idempotency_key_is_distinct_online_but_omitted_on_disk() {
    let mut l = memory();
    let mut input = id();
    input.idempotency_key = Some("".into());
    l.start(&input).unwrap();
    let mut no_key = input.clone();
    no_key.idempotency_key = None;
    assert!(l.start(&no_key).is_err());
    assert_eq!(l.recover(&no_key).unwrap(), None);
    let restored = SessionOperationLedger::new(
        input.session_id.clone(),
        Memory::default(),
        &l.sink().entries,
    )
    .unwrap();
    assert_eq!(restored.recover(&input).unwrap(), None);
    assert_eq!(
        restored.recover(&no_key).unwrap(),
        Some(OperationRecovery::Incomplete)
    );
}
#[test]
#[ignore = "requires fixed TS baseline and Node/tsx; run with PDRS_LEDGER_BASELINE"]
fn real_ts_reads_rust_and_continues_its_transcript() {
    let baseline = std::env::var("PDRS_LEDGER_BASELINE").expect("set fixed baseline path");
    let scratch = Scratch::new();
    {
        let mut l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
        l.start(&id()).unwrap();
        l.accept(&id(), &"stream-1".into()).unwrap();
        l.result_unknown(&unknown()).unwrap();
        l.terminal(&known()).unwrap();
    }
    let output = std::process::Command::new("node")
        .arg("--import")
        .arg(format!("{baseline}/node_modules/tsx/dist/loader.mjs"))
        .arg(format!("{FIXTURES}/generate.mts"))
        .arg(&baseline)
        .arg(scratch.path())
        .arg("--verify-rust")
        .output()
        .unwrap();
    println!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success());
    let l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
    assert_eq!(l.sink().snapshot_state().sequence, 5);
    let mut other = id();
    other.operation_id = "op-2".into();
    other.request_id = "req-2".into();
    assert_eq!(
        l.recover(&other).unwrap(),
        Some(OperationRecovery::Incomplete)
    );
}

#[test]
fn seed_wire_order_is_preserved_before_clone_and_duplicate_comparison() {
    // Captured by generate.mts seed-order using the actual TS implementation.
    let mut l = memory();
    l.start(&id()).unwrap();
    let mut terminal = known();
    terminal.resolution.seed_state =
        Some(parse_json(r#"{"readFileState":{"a":{"kind":"text","mtimeMs":1}}}"#).unwrap());
    l.terminal(&terminal).unwrap();
    assert!(
        l.terminal(&terminal)
            .unwrap_err()
            .to_string()
            .contains("different durable terminal")
    );
    assert_eq!(
        stringify_json(field(l.sink().entries.last().unwrap(), "seedState"))
            .unwrap()
            .unwrap(),
        r#"{"readFileState":{"a":{"kind":"text","mtimeMs":1}}}"#
    );
    let Some(OperationRecovery::Terminal(recovered)) = l.recover(&id()).unwrap() else {
        panic!()
    };
    assert_eq!(
        stringify_json(recovered.seed_state.as_ref().unwrap())
            .unwrap()
            .unwrap(),
        r#"{"readFileState":{"a":{"mtimeMs":1,"kind":"text"}}}"#
    );
}

#[test]
fn nonfinite_unknown_sequence_survives_json_null_restart() {
    let scratch = Scratch::new();
    let mut l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
    l.start(&id()).unwrap();
    let mut terminal = unknown();
    terminal.last_applied_sequence = Some(f64::NAN);
    l.result_unknown(&terminal).unwrap();
    assert!(l.result_unknown(&terminal).is_err());
    drop(l);
    let l = JsonlLedgerStore::open(scratch.path(), "session-1").unwrap();
    let Some(OperationRecovery::ResultUnknown(recovered)) = l.recover(&id()).unwrap() else {
        panic!()
    };
    assert_eq!(recovered.last_applied_sequence, None);
}
