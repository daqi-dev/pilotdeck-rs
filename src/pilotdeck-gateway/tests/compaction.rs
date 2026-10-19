//! A6.6-pr2 compaction persistence contract tests. Scenario outcomes and
//! transcript bytes come from the real fixed-baseline
//! AgentSessionEventRecorder / JsonlTranscriptWriter / module dispatcher
//! (tests/fixtures/compaction/generate.mts); the durableContextRuntime and
//! TurnRunner line ranges cited there define the call sequence only.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;
use pilotdeck_gateway::host::modules::turn::{self, Handler, Ports};
use pilotdeck_gateway::host::{HostError, ModuleError};
use pilotdeck_gateway::storage::recorder::{Recorder, RecorderError};
use pilotdeck_gateway::storage::{JsonlTranscriptStore, TranscriptKey, read_transcript};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use scratch::remove_owned;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const ORACLE: &str = include_str!("fixtures/compaction/oracle.json");
const NOW: &str = "2026-10-19T00:00:00.000Z";
const PROJECT_ROOT: &str = "/pdrs-fixture/project";
const SESSION: &str = "s1";
static UNDEFINED: JsValue = JsValue::Undefined;

fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap_or(&UNDEFINED)
}
fn s(value: &JsValue) -> String {
    value.as_string().expect("string").to_utf8().expect("utf8")
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("expected array"),
    }
}
fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (key, value) in fields {
        if !matches!(value, JsValue::Undefined) {
            o.insert(key.into(), value);
        }
    }
    JsValue::Object(o)
}
fn text(value: &str) -> JsValue {
    JsValue::String(value.into())
}
fn oracle() -> JsValue {
    assert!(
        !ORACLE.contains(".generate-"),
        "oracle.json leaks a generation path"
    );
    baseline::oracle(ORACLE)
}

/// Exclusive per-run directory. Deletes are proven to target only that tree.
struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = PathBuf::from("/tmp");
        fs::create_dir_all(&base).unwrap();
        loop {
            let token = format!(
                "{:06x}",
                (std::process::id() as usize
                    ^ NEXT
                        .fetch_add(1, Ordering::Relaxed)
                        .wrapping_mul(0x9e37_79b9))
                    & 0xff_ffff
            );
            let dir = base.join(format!("pdrs-a66p2-{name}-{token}"));
            match fs::create_dir(&dir) {
                Ok(()) => return Self(dir),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create scratch {}: {error}", dir.display()),
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = remove_owned(&self.0, &self.0, true);
    }
}
fn entry_id(index: usize) -> String {
    format!("{index:08x}-0000-4000-8000-{index:012x}")
}
fn clock() -> pilotdeck_gateway::storage::recorder::RecorderClock {
    Arc::new(|| Ok(NOW.to_string()))
}
struct Ctx {
    path: PathBuf,
    recorder: Arc<Recorder>,
}
impl Ctx {
    fn new(dir: &Path) -> Self {
        let path = dir.join(format!("{SESSION}.jsonl"));
        let entries = Arc::new(AtomicUsize::new(0));
        let operations = Arc::new(AtomicUsize::new(0));
        let recorder = Recorder::with_sources(
            Arc::new(JsonlTranscriptStore::new()),
            TranscriptKey {
                project_root: PROJECT_ROOT.into(),
                pilot_home: dir.join("home"),
                session_id: SESSION.into(),
                transcript_path: path.clone(),
            },
            clock(),
            Arc::new(move || Ok(entry_id(entries.fetch_add(1, Ordering::Relaxed) + 1))),
        )
        .with_operation_ids(Arc::new(move || {
            Ok(format!(
                "op-{}",
                operations.fetch_add(1, Ordering::Relaxed) + 1
            ))
        }));
        Self {
            path,
            recorder: Arc::new(recorder),
        }
    }
    fn reopen(&mut self) {
        let read = read_transcript(&self.path).unwrap();
        self.recorder.restore(&read.entries).unwrap();
    }
}

fn error_outcome(error: &RecorderError) -> JsValue {
    let (name, code) = match error {
        RecorderError::State(_) | RecorderError::Store(_) | RecorderError::NodeHostRequired(_) => {
            ("Error", None)
        }
        RecorderError::Domain { code, .. } => ("SessionDomainValidationError", Some(*code)),
        RecorderError::Envelope { code, .. } => ("SessionEventValidationError", Some(*code)),
        RecorderError::Type(_) => ("TypeError", None),
        RecorderError::Range(_) => ("RangeError", None),
    };
    object([(
        "error",
        object([
            ("name", text(name)),
            ("code", code.map_or(JsValue::Undefined, text)),
            ("message", text(&error.to_string())),
        ]),
    )])
}
fn host_error_outcome(error: &HostError) -> JsValue {
    object([(
        "error",
        object([
            ("name", text("Error")),
            (
                "message",
                match &error.module_error {
                    Some(module) => JsValue::String(module.message.clone()),
                    None => text(&error.message),
                },
            ),
        ]),
    )])
}

async fn run_step(ctx: &mut Ctx, step: &JsValue) -> JsValue {
    let op = s(field(step, "op"));
    let turn: JsString = field(step, "turnId")
        .as_string()
        .cloned()
        .unwrap_or_else(|| "turn-1".into());
    let session: JsString = SESSION.into();
    let result = match op.as_str() {
        "startTurn" => ctx
            .recorder
            .start_turn(&session, &turn)
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "nextOperationId" => ctx
            .recorder
            .next_compaction_operation_id()
            .map(|id| object([("id", JsValue::String(id))])),
        "modelRequest" => ctx
            .recorder
            .record_model_request(
                &session,
                &turn,
                object([
                    ("model", text("fixture-model")),
                    (
                        "messages",
                        JsValue::Array(vec![object([
                            ("role", text("user")),
                            (
                                "content",
                                JsValue::Array(vec![object([
                                    ("type", text("text")),
                                    ("text", text("open step")),
                                ])]),
                            ),
                        ])]),
                    ),
                ]),
            )
            .map(|step| object([("step", JsValue::Number(step as f64))])),
        "toolCalls" => ctx
            .recorder
            .record_tool_calls(&session, &turn, array(field(step, "calls")))
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "recordStarted" => ctx
            .recorder
            .record_compaction_started(&session, &turn, field(step, "draft"))
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "recordCompleted" => ctx
            .recorder
            .record_compaction_completed(&session, &turn, field(step, "draft"))
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "recordFailed" => ctx
            .recorder
            .record_compaction_failed(&session, &turn, field(step, "draft"))
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "deferCompletion" => ctx
            .recorder
            .defer_compaction_completion(&session, &turn, field(step, "draft"))
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "commitDeferred" => ctx
            .recorder
            .commit_deferred_compaction(&session, &turn)
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "failDeferred" => ctx
            .recorder
            .fail_deferred_compaction(&session, &turn, &s(field(step, "message")))
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "writeControlBoundary" => ctx
            .recorder
            .record_control_boundary(&session, &turn, field(step, "boundary"))
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "persistReplacement" => {
            // TurnRunner.ts:344-365 through the production host adapter.
            let port = turn::persist_compaction_to_recorder(
                ctx.recorder.clone(),
                session.clone(),
                turn.clone(),
                Arc::new(|_| {}),
            );
            match port(
                field(step, "boundary").as_object().unwrap().clone(),
                array(field(step, "messages")).to_vec(),
            )
            .await
            {
                Ok(()) => Ok(object([("ok", JsValue::Bool(true))])),
                Err(error) => return host_error_outcome(&error),
            }
        }
        "reopen" => {
            ctx.reopen();
            let read = read_transcript(&ctx.path).unwrap();
            Ok(object([
                ("entries", JsValue::Number(read.entries.len() as f64)),
                (
                    "sequence",
                    JsValue::Number(ctx.recorder.snapshot_state().0 as f64),
                ),
            ]))
        }
        "read" => {
            let read = read_transcript(&ctx.path).unwrap();
            let sequence = read
                .entries
                .last()
                .map_or(0.0, |e| field(e, "sequence").as_number().unwrap());
            Ok(object([
                ("entries", JsValue::Number(read.entries.len() as f64)),
                (
                    "diagnostics",
                    JsValue::Number(read.diagnostics.len() as f64),
                ),
                ("sequence", JsValue::Number(sequence)),
            ]))
        }
        "state" => {
            let (sequence, last) = ctx.recorder.snapshot_state();
            Ok(object([
                ("sequence", JsValue::Number(sequence as f64)),
                ("lastEntryId", last.map_or(JsValue::Null, JsValue::String)),
            ]))
        }
        other => panic!("unknown step {other}"),
    };
    result.unwrap_or_else(|error| error_outcome(&error))
}

fn snapshot_files(root: &Path) -> JsValue {
    fn walk(root: &Path, rel: &str, out: &mut JsObject) {
        let mut names: Vec<_> = fs::read_dir(root.join(rel))
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        names.sort_by(|a, b| {
            a.file_name()
                .to_string_lossy()
                .as_bytes()
                .cmp(b.file_name().to_string_lossy().as_bytes())
        });
        for entry in names {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if entry.file_type().unwrap().is_dir() {
                walk(root, &child, out);
            } else {
                let bytes = fs::read(root.join(&child)).unwrap();
                out.insert(
                    child.as_str().into(),
                    String::from_utf8_lossy(&bytes).as_ref().into(),
                );
            }
        }
    }
    let mut out = JsObject::new();
    walk(root, "", &mut out);
    JsValue::Object(out)
}
fn normalize(text: &str, dir: &Path) -> String {
    let uuid =
        regex::Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")
            .unwrap();
    let replaced = text.replace(&dir.display().to_string(), "<dir>");
    let mut ids: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    uuid.replace_all(&replaced, |c: &regex::Captures| {
        let next = format!("<id-{}>", ids.len() + 1);
        ids.entry(c[0].to_string()).or_insert(next).clone()
    })
    .into_owned()
}

#[tokio::test]
async fn real_ts_recorder_scenarios_match_outcomes_and_bytes() {
    let oracle = oracle();
    let JsValue::Array(scenarios) = field(&oracle, "scenarios") else {
        panic!("scenarios")
    };
    assert_eq!(scenarios.len(), 11, "oracle scenario count");
    let scratch = Scratch::new("replay");
    for scenario in scenarios {
        let name = s(field(scenario, "name"));
        let dir = scratch.0.join(&name);
        fs::create_dir_all(&dir).unwrap();
        let mut ctx = Ctx::new(&dir);
        let JsValue::Array(steps) = field(scenario, "steps") else {
            panic!("steps")
        };
        let mut replayed = Vec::new();
        for step in steps {
            let outcome = run_step(&mut ctx, step).await;
            let mut step = step.as_object().unwrap().clone();
            step.insert("outcome".into(), outcome);
            replayed.push(JsValue::Object(step));
        }
        let mut actual = JsObject::new();
        actual.insert("name".into(), field(scenario, "name").clone());
        if let Some(options) = scenario.get("options") {
            actual.insert("options".into(), options.clone());
        }
        actual.insert("steps".into(), JsValue::Array(replayed));
        actual.insert("files".into(), snapshot_files(&dir));
        let actual = normalize(
            &stringify_json(&JsValue::Object(actual)).unwrap().unwrap(),
            &dir,
        );
        let expected = stringify_json(scenario).unwrap().unwrap();
        if actual != expected {
            let a = parse_json(&actual).unwrap();
            for part in ["steps", "files"] {
                assert_eq!(
                    stringify_json(field(&a, part)).unwrap(),
                    stringify_json(field(scenario, part)).unwrap(),
                    "{name}: {part}"
                );
            }
            assert_eq!(actual, expected, "{name}");
        }
    }
}

fn turn_call(payload: &JsValue) -> JsValue {
    object([("payload", payload.clone())])
}

#[tokio::test]
async fn real_ts_dispatcher_scenarios_match_persist_compaction() {
    let oracle = oracle();
    let JsValue::Array(scenarios) = field(&oracle, "turn") else {
        panic!("turn")
    };
    assert_eq!(scenarios.len(), 18, "oracle turn scenario count");
    for scenario in scenarios {
        let name = s(field(scenario, "name"));
        let mode = s(field(scenario, "mode"));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut ports = Ports::default();
        if mode != "none" {
            let captured = calls.clone();
            let throw = mode == "throw";
            ports.persist_compaction = Some(Arc::new(move |boundary, messages| {
                captured
                    .lock()
                    .unwrap()
                    .push((boundary.clone(), messages.clone()));
                if throw {
                    Box::pin(async { Err(ModuleError::new("disk full").into()) })
                } else {
                    Box::pin(async { Ok(()) })
                }
            }));
        }
        let handler = Handler {
            ports: ports.clone(),
            authorizations: None,
        };
        assert_eq!(
            handler
                .methods()
                .is_some_and(|methods| methods.contains(&"persist_compaction")),
            mode != "none",
            "{name}: capability surface"
        );
        let outcome = match handler.call(turn_call(field(scenario, "payload"))).await {
            Ok(response) => JsValue::Object(response),
            Err(error) => host_error_outcome(&error),
        };
        assert_eq!(
            stringify_json(&outcome).unwrap(),
            stringify_json(field(scenario, "response")).unwrap(),
            "{name}: response"
        );
        let captured = calls.lock().unwrap();
        let actual = JsValue::Array(
            captured
                .iter()
                .map(|(boundary, messages)| {
                    object([
                        ("boundary", JsValue::Object(boundary.clone())),
                        ("messages", JsValue::Array(messages.clone())),
                    ])
                })
                .collect(),
        );
        assert_eq!(
            stringify_json(&actual).unwrap(),
            stringify_json(field(scenario, "calls")).unwrap(),
            "{name}: callback calls"
        );
        assert_eq!(
            captured.len(),
            array(field(scenario, "calls")).len(),
            "{name}: callback count"
        );
    }
}

fn recorder_at(dir: &Path) -> Recorder {
    let entries = Arc::new(AtomicUsize::new(0));
    let operations = Arc::new(AtomicUsize::new(0));
    Recorder::with_sources(
        Arc::new(JsonlTranscriptStore::new()),
        TranscriptKey {
            project_root: PROJECT_ROOT.into(),
            pilot_home: dir.join("home"),
            session_id: SESSION.into(),
            transcript_path: dir.join(format!("{SESSION}.jsonl")),
        },
        clock(),
        Arc::new(move || Ok(entry_id(entries.fetch_add(1, Ordering::Relaxed) + 1))),
    )
    .with_operation_ids(Arc::new(move || {
        Ok(format!(
            "op-{}",
            operations.fetch_add(1, Ordering::Relaxed) + 1
        ))
    }))
}

#[tokio::test]
async fn unsupported_boundary_closes_deferred_bracket_and_aborts() {
    // TS's JsonlTranscriptWriter always provides recordCompactionReplacement,
    // so a non-compact boundary is this adapter's equivalent of the throwing
    // write. The real TurnRunner catch is replay-tested below.
    let scratch = Scratch::new("port");
    let session: JsString = SESSION.into();
    let turn: JsString = "turn-1".into();
    let dir = scratch.0.join("unsupported");
    fs::create_dir_all(&dir).unwrap();
    let recorder = Arc::new(recorder_at(&dir));
    recorder.start_turn(&session, &turn).unwrap();
    recorder
        .record_compaction_started(
            &session,
            &turn,
            &object([
                ("operationId", text("unsupported-op")),
                ("trigger", text("auto")),
                ("messageCount", JsValue::Number(1.0)),
            ]),
        )
        .unwrap();
    recorder
        .defer_compaction_completion(
            &session,
            &turn,
            &object([
                ("operationId", text("unsupported-op")),
                ("status", text("compacted")),
                ("messageCount", JsValue::Number(1.0)),
            ]),
        )
        .unwrap();
    let aborts = Arc::new(Mutex::new(Vec::new()));
    let captured = aborts.clone();
    let port = turn::persist_compaction_to_recorder(
        recorder.clone(),
        session,
        turn,
        Arc::new(move |reason: &str| captured.lock().unwrap().push(reason.to_string())),
    );
    port(
        object([("kind", text("resume"))])
            .as_object()
            .unwrap()
            .clone(),
        Vec::new(),
    )
    .await
    .expect("TS callback answers {persisted:true} after aborting");
    assert_eq!(
        *aborts.lock().unwrap(),
        ["compaction_persistence_failed"],
        "abort reason"
    );
    let read = read_transcript(dir.join(format!("{SESSION}.jsonl"))).unwrap();
    let kinds: Vec<String> = read
        .entries
        .iter()
        .map(|entry| s(field(entry, "type")))
        .collect();
    assert_eq!(
        kinds,
        ["turn_started", "compaction_started", "compaction_failed"]
    );
}

/// P2-1: the fixture executes the real `TurnRunner` callback. TS resolves the
/// callback after `failDeferredCompaction` and `abort("compaction_persistence_failed")`,
/// so the dispatcher answers `{persisted:true}`; the abort travels beside it.
#[tokio::test]
async fn real_ts_runner_aborts_after_successful_persist_response() {
    let oracle = oracle();
    let expected = field(&oracle, "runner");
    assert_eq!(s(field(expected, "name")), "runner-persistence-failure");
    assert_eq!(field(expected, "callbackCalls").as_number(), Some(1.0));
    assert_eq!(field(expected, "callbackReturned"), &JsValue::Bool(true));
    assert!(matches!(field(expected, "callbackError"), JsValue::Null));
    assert_eq!(field(expected, "signalAborted"), &JsValue::Bool(true));
    assert_eq!(
        s(field(expected, "signalReason")),
        "compaction_persistence_failed"
    );

    let scratch = Scratch::new("runner");
    let dir = scratch.0.join("runner-persistence-failure");
    fs::create_dir_all(&dir).unwrap();
    let recorder = Arc::new(recorder_at(&dir));
    let session: JsString = SESSION.into();
    let turn: JsString = "turn-1".into();
    recorder.start_turn(&session, &turn).unwrap();
    recorder
        .record_compaction_started(
            &session,
            &turn,
            &object([
                ("operationId", text("op-r")),
                ("trigger", text("auto")),
                ("messageCount", JsValue::Number(1.0)),
            ]),
        )
        .unwrap();
    recorder
        .defer_compaction_completion(
            &session,
            &turn,
            &object([
                ("operationId", text("op-r")),
                ("status", text("compacted")),
                ("tier", text("full")),
                ("compactionId", text("compaction-runner")),
                ("messageCount", JsValue::Number(1.0)),
            ]),
        )
        .unwrap();

    let aborts = Arc::new(Mutex::new(Vec::new()));
    let captured = aborts.clone();
    let ports = Ports {
        persist_compaction: Some(turn::persist_compaction_to_recorder(
            recorder,
            session,
            turn,
            Arc::new(move |reason: &str| captured.lock().unwrap().push(reason.to_string())),
        )),
        ..Ports::default()
    };
    let handler = Handler {
        ports,
        authorizations: None,
    };
    let boundary = object([("kind", text("resume"))]);
    let response = handler
        .call(turn_call(&object([
            ("operation", text("persist_compaction")),
            ("boundary", boundary),
            ("messages", JsValue::Array(Vec::new())),
        ])))
        .await
        .expect("dispatcher responds after the callback resolves");
    assert_eq!(
        stringify_json(&JsValue::Object(response))
            .unwrap()
            .as_deref(),
        Some("{\"persisted\":true}")
    );
    assert_eq!(
        *aborts.lock().unwrap(),
        ["compaction_persistence_failed"],
        "the abort is the loop's, not the dispatcher error path"
    );

    let read = read_transcript(dir.join(format!("{SESSION}.jsonl"))).unwrap();
    let entries: Vec<JsValue> = read
        .entries
        .iter()
        .filter(|entry| s(field(entry, "type")).starts_with("compaction_"))
        .map(|entry| {
            let mut item = JsObject::new();
            item.insert("type".into(), field(entry, "type").clone());
            if let Some(operation) = entry.get("operationId") {
                item.insert("operationId".into(), operation.clone());
            }
            if let Some(error) = entry.get("error") {
                item.insert("error".into(), error.clone());
            }
            JsValue::Object(item)
        })
        .collect();
    assert_eq!(
        stringify_json(&JsValue::Array(entries)).unwrap(),
        stringify_json(field(expected, "compactionEntries")).unwrap(),
        "compaction entries"
    );
}

/// #314: with no `operation_ids` injection the recorder must use its own
/// `randomUUID` default, not the writer's entry-id source. A shared source
/// would make the next entry skip a number and make transcript bytes depend on
/// whether a compaction id was requested.
#[test]
fn default_operation_id_is_independent_of_entry_ids() {
    let scratch = Scratch::new("default-operation-id");
    let key = TranscriptKey {
        project_root: PROJECT_ROOT.into(),
        pilot_home: scratch.0.join("home"),
        session_id: SESSION.into(),
        transcript_path: scratch.0.join("s1.jsonl"),
    };
    let recorder = Recorder::with_sources(
        Arc::new(JsonlTranscriptStore::new()),
        key.clone(),
        clock(),
        Arc::new(|| Ok("entry-0001".into())),
    );
    let id = recorder.next_compaction_operation_id().unwrap();
    let id = id.to_utf8().unwrap();
    assert_ne!(id, "entry-0001", "operation id must not consume entry ids");
    let v4 =
        regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
            .unwrap();
    assert!(v4.is_match(&id), "default operation id is a v4 UUID: {id}");

    recorder
        .start_turn(&SESSION.into(), &"turn-1".into())
        .unwrap();
    let read = read_transcript(&key.transcript_path).unwrap();
    assert_eq!(read.entries.len(), 1);
    assert_eq!(s(field(&read.entries[0], "entryId")), "entry-0001");
}

/// #314 concurrency: commit and fail must serialize the "find deferred bracket
/// and write its terminal" pair. Without one lock, a loser could observe the
/// bracket after it was closed and turn a successful persistence into an
/// `invalid_compaction` error/abort.
#[test]
fn concurrent_commit_and_fail_close_exactly_one_bracket() {
    let scratch = Scratch::new("concurrent-deferred");
    let ctx = Ctx::new(&scratch.0);
    let session = JsString::from(SESSION);
    let turn = JsString::from("turn-1");
    ctx.recorder.start_turn(&session, &turn).unwrap();
    ctx.recorder
        .record_compaction_started(
            &session,
            &turn,
            &object([
                ("operationId", text("op-c")),
                ("trigger", text("auto")),
                ("messageCount", JsValue::Number(1.0)),
            ]),
        )
        .unwrap();
    ctx.recorder
        .defer_compaction_completion(
            &session,
            &turn,
            &object([
                ("operationId", text("op-c")),
                ("status", text("compacted")),
                ("messageCount", JsValue::Number(1.0)),
            ]),
        )
        .unwrap();

    let barrier = Arc::new(std::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for index in 0..8 {
        let recorder = ctx.recorder.clone();
        let barrier = barrier.clone();
        let session = session.clone();
        let turn = turn.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            if index % 2 == 0 {
                recorder.commit_deferred_compaction(&session, &turn)
            } else {
                recorder.fail_deferred_compaction(&session, &turn, "boom")
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    let read = read_transcript(&ctx.path).unwrap();
    let terminals = read
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                s(field(entry, "type")).as_str(),
                "compaction_completed" | "compaction_failed"
            )
        })
        .count();
    assert_eq!(terminals, 1, "exactly one terminal record");
}
