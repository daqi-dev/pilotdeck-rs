//! Durable recorder contract tests. Scenario outcomes and transcript bytes
//! come from the real TS AgentSessionEventRecorder
//! (tests/fixtures/recorder/generate.mts).
#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::storage::recorder::{Recorder, RecorderError};
use pilotdeck_gateway::storage::{AsyncTranscriptAdapter, SessionStorageInput};
use pilotdeck_gateway::storage::{JsonlTranscriptStore, TranscriptKey, read_transcript};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use regex::Regex;
use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/recorder");
const NOW: &str = "2026-10-18T00:00:00.000Z";
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
    let text = fs::read_to_string(format!("{FIXTURES}/oracle.json")).unwrap();
    assert!(
        !text.contains(".generate-"),
        "oracle.json leaks a generation path"
    );
    baseline::oracle(&text)
}
fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}
struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = clean(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../_tmp/20261018-a6-recorder/test-scratch"),
        );
        fs::create_dir_all(&root).unwrap();
        loop {
            let dir = root.join(format!(
                "{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&dir) {
                Ok(()) => return Self(dir),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create exclusive scratch: {error}"),
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn clock() -> pilotdeck_gateway::storage::recorder::RecorderClock {
    Arc::new(|| Ok(NOW.to_string()))
}
struct Ctx {
    path: PathBuf,
    recorder: Recorder,
}
impl Ctx {
    fn new(dir: &Path) -> Self {
        Self::with_options(dir, &JsValue::Undefined)
    }
    fn with_options(dir: &Path, options: &JsValue) -> Self {
        let path = dir.join(format!("{SESSION}.jsonl"));
        let recorder = if options.is_undefined() {
            let chat_dir = dir.to_path_buf();
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |_| chat_dir.clone()),
            );
            let storage = adapter.create_session_storage(SessionStorageInput {
                project_root: PROJECT_ROOT.into(),
                pilot_home: dir.join("home"),
                session_id: SESSION.into(),
                now: Some(Arc::new(|| NOW.to_owned())),
            });
            Recorder::from_writer(Arc::new(storage.transcript))
        } else {
            let ticking = field(options, "tickingClock") == &JsValue::Bool(true);
            let deterministic = field(options, "deterministicIds") == &JsValue::Bool(true);
            let uuids = match field(options, "uuids") {
                JsValue::Array(ids) => Some(ids.iter().map(s).collect::<Vec<_>>()),
                _ => None,
            };
            let ticks = AtomicUsize::new(0);
            let ids = AtomicUsize::new(0);
            Recorder::with_sources(
                Arc::new(JsonlTranscriptStore::new()),
                TranscriptKey {
                    project_root: PROJECT_ROOT.into(),
                    pilot_home: dir.join("home"),
                    session_id: SESSION.into(),
                    transcript_path: path.clone(),
                },
                Arc::new(move || {
                    Ok(if ticking {
                        format!(
                            "2026-10-18T00:00:{:02}.000Z",
                            ticks.fetch_add(1, Ordering::Relaxed)
                        )
                    } else {
                        NOW.to_owned()
                    })
                }),
                Arc::new(move || {
                    let index = ids.fetch_add(1, Ordering::Relaxed);
                    Ok(if let Some(ids) = &uuids {
                        ids[index].clone()
                    } else if deterministic {
                        format!("id-{}", index + 1)
                    } else {
                        panic!("test options must specify ids")
                    })
                }),
            )
        };
        Self { path, recorder }
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

fn run_step(ctx: &mut Ctx, step: &JsValue) -> JsValue {
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
        "modelRequest" => ctx
            .recorder
            .record_model_request(&session, &turn, field(step, "request").clone())
            .map(|step| object([("step", JsValue::Number(step as f64))])),
        "modelEvent" => ctx
            .recorder
            .record_model_event(&session, &turn, field(step, "event"))
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "toolCalls" => ctx
            .recorder
            .record_tool_calls(&session, &turn, array(field(step, "calls")))
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "toolResults" => ctx
            .recorder
            .record_tool_results(&session, &turn, array(field(step, "results")))
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "writerAcceptedInput" => ctx
            .recorder
            .writer()
            .record_accepted_input(
                &session,
                &turn,
                array(field(step, "messages")).to_vec(),
                field(step, "metadata").as_object().cloned(),
            )
            .map_err(RecorderError::from)
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "writerMetadata" => ctx
            .recorder
            .writer()
            .record_session_metadata(&session, &turn, field(step, "metadata").clone())
            .map_err(RecorderError::from)
            .map(|_| object([("ok", JsValue::Bool(true))])),
        "restore" => ctx
            .recorder
            .restore(array(field(step, "entries")))
            .map(|_| object([("ok", JsValue::Bool(true))])),
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
            .map(|e| e.unwrap())
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
        Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}").unwrap();
    let replaced = text.replace(&dir.display().to_string(), "<dir>");
    let mut ids: HashMap<String, String> = HashMap::new();
    uuid.replace_all(&replaced, |c: &regex::Captures| {
        let next = format!("<id-{}>", ids.len() + 1);
        ids.entry(c[0].to_string()).or_insert(next).clone()
    })
    .into_owned()
}

#[test]
fn real_ts_scenarios_match_outcomes_and_bytes() {
    let oracle = oracle();
    let JsValue::Array(scenarios) = field(&oracle, "scenarios") else {
        panic!("scenarios")
    };
    assert!(
        scenarios.len() >= 25,
        "oracle has {} scenarios",
        scenarios.len()
    );
    let scratch = Scratch::new("replay");
    for scenario in scenarios {
        let name = s(field(scenario, "name"));
        let dir = scratch.0.join(&name);
        fs::create_dir_all(&dir).unwrap();
        let mut ctx = Ctx::with_options(&dir, field(scenario, "options"));
        let JsValue::Array(steps) = field(scenario, "steps") else {
            panic!("steps")
        };
        let mut replayed = Vec::new();
        for step in steps {
            let outcome = run_step(&mut ctx, step);
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

fn run_generator(mode: &str, dir: &Path) {
    let baseline = std::env::var("PDRS_BASELINE").expect("set fixed baseline path");
    let status = Command::new("node")
        .arg("--import")
        .arg(format!("{baseline}/node_modules/tsx/dist/loader.mjs"))
        .arg(format!("{FIXTURES}/generate.mts"))
        .arg(dir)
        .arg(mode)
        .env("PDRS_BASELINE", &baseline)
        .status()
        .unwrap();
    assert!(status.success(), "generator {mode} failed");
}

fn chain(path: &Path) -> Vec<JsValue> {
    let read = read_transcript(path).unwrap();
    assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
    let null = JsValue::Null;
    for (i, entry) in read.entries.iter().enumerate() {
        assert_eq!(field(entry, "sequence").as_number(), Some((i + 1) as f64));
        assert_eq!(
            field(entry, "parentEntryId"),
            if i == 0 {
                &null
            } else {
                field(&read.entries[i - 1], "entryId")
            },
            "parent chain at {}",
            i + 1
        );
    }
    read.entries
}

fn rust_continue(path: &Path, turn: &str) {
    let key = TranscriptKey {
        project_root: PROJECT_ROOT.into(),
        pilot_home: path.parent().unwrap().join("home"),
        session_id: SESSION.into(),
        transcript_path: path.to_path_buf(),
    };
    let active = Recorder::with_clock(Arc::new(JsonlTranscriptStore::new()), key, clock());
    let read = read_transcript(path).unwrap();
    active.restore(&read.entries).unwrap();
    let session: JsString = SESSION.into();
    let turn: JsString = turn.into();
    active.start_turn(&session, &turn).unwrap();
    active
        .record_model_request(
            &session,
            &turn,
            object([("model", text("fixture-model")), ("from", text("rust"))]),
        )
        .unwrap();
    active
        .record_model_event(
            &session,
            &turn,
            &object([
                ("type", text("text_delta")),
                ("raw", object([("dropped", JsValue::Bool(true))])),
            ]),
        )
        .unwrap();
    active
        .record_tool_calls(
            &session,
            &turn,
            &[object([
                ("id", text("rust-call")),
                ("name", text("read_file")),
                ("raw", object([("dropped", JsValue::Bool(true))])),
            ])],
        )
        .unwrap();
    active
        .record_tool_results(
            &session,
            &turn,
            &[object([
                ("type", text("success")),
                ("toolCallId", text("rust-call")),
                ("toolName", text("read_file")),
                ("raw", object([("dropped", JsValue::Bool(true))])),
            ])],
        )
        .unwrap();
}

#[test]
#[ignore = "requires fixed TS baseline and Node/tsx; run with PDRS_BASELINE"]
fn real_ts_and_rust_continue_each_others_transcripts() {
    // TS writes, Rust resumes and appends, TS verifies/resumes again.
    let ts_first = Scratch::new("ts-first");
    run_generator("--cross-ts-write", &ts_first.0);
    let path = ts_first.0.join(format!("{SESSION}.jsonl"));
    let entries = chain(&path);
    assert_eq!(entries.len(), 14);
    rust_continue(&path, "turn-rust-1");
    let entries = chain(&path);
    assert_eq!(entries.len(), 21, "seven Rust entries");
    assert_eq!(
        field(&entries[14], "type")
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap(),
        "turn_started"
    );
    assert_eq!(
        field(&entries[20], "type")
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap(),
        "step_completed"
    );
    assert!(
        cross_event_matches(&entries[17], "text_delta"),
        "Rust event payload must match exactly"
    );
    assert_eq!(s(field(&entries[18], "type")), "tool_call");
    assert_eq!(
        field(&entries[18], "call"),
        &object([("id", text("rust-call")), ("name", text("read_file"))])
    );
    assert_eq!(s(field(&entries[19], "type")), "tool_result");
    assert_eq!(
        field(&entries[19], "result"),
        &object([
            ("type", text("success")),
            ("toolCallId", text("rust-call")),
            ("toolName", text("read_file"))
        ])
    );
    run_generator("--cross-ts-verify", &ts_first.0);
    let entries = chain(&path);
    assert_eq!(entries.len(), 28, "TS continues past the Rust turn");
    assert!(cross_event_matches(&entries[24], "done"));
    assert_eq!(
        field(&entries[27], "type")
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap(),
        "step_completed"
    );

    // Rust writes, TS verifies and appends, Rust resumes the TS continuation.
    let rust_first = Scratch::new("rust-first");
    let rust_path = rust_first.0.join(format!("{SESSION}.jsonl"));
    {
        let ctx = Ctx::new(&rust_first.0);
        let session: JsString = SESSION.into();
        for turn in ["turn-rust-a", "turn-rust-b"] {
            let turn: JsString = turn.into();
            ctx.recorder.start_turn(&session, &turn).unwrap();
            ctx.recorder
                .record_model_request(&session, &turn, object([("model", text("m"))]))
                .unwrap();
            ctx.recorder
                .record_model_event(&session, &turn, &object([("type", text("done"))]))
                .unwrap();
            ctx.recorder
                .record_tool_calls(
                    &session,
                    &turn,
                    &[object([("id", text("rust-x")), ("name", text("f"))])],
                )
                .unwrap();
            ctx.recorder
                .record_tool_results(&session, &turn, &[object([("toolCallId", text("rust-x"))])])
                .unwrap();
        }
        assert_eq!(chain(&rust_path).len(), 14);
    }
    run_generator("--cross-ts-verify", &rust_first.0);
    let entries = chain(&rust_path);
    assert_eq!(entries.len(), 21);
    assert!(cross_event_matches(&entries[17], "done"));
    rust_continue(&rust_path, "turn-rust-after-ts");
    let entries = chain(&rust_path);
    assert_eq!(entries.len(), 28);
    assert!(cross_event_matches(&entries[24], "text_delta"));
}

/// The cross-test predicate must reject missing/wrong entries as well as raw leaks.
fn cross_event_matches(entry: &JsValue, kind: &str) -> bool {
    field(entry, "type") == &text("model_stream_event")
        && field(entry, "event") == &object([("type", text(kind))])
}
#[test]
fn cross_event_check_rejects_wrong_entry_missing_payload_and_raw_leaks() {
    for entry in [
        object([("type", text("model_request"))]),
        object([("type", text("model_stream_event"))]),
        object([
            ("type", text("model_stream_event")),
            (
                "event",
                object([("type", text("text_delta")), ("raw", JsValue::Bool(true))]),
            ),
        ]),
    ] {
        assert!(!cross_event_matches(&entry, "text_delta"));
    }
    assert!(cross_event_matches(
        &object([
            ("type", text("model_stream_event")),
            ("event", object([("type", text("text_delta"))]))
        ]),
        "text_delta"
    ));
}

#[test]
fn shared_writer_publishes_domain_and_entry_ids_only_after_store_success() {
    use pilotdeck_gateway::storage::{StoreResult, TranscriptRead, TranscriptStore};
    use std::sync::atomic::AtomicBool;
    struct RejectOnce {
        reject: AtomicBool,
        disk: JsonlTranscriptStore,
    }
    impl TranscriptStore for RejectOnce {
        fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
            if self.reject.swap(false, Ordering::SeqCst) {
                return Err(std::io::Error::other("injected append failure").into());
            }
            self.disk.append(key, entry)
        }
        fn read(&self, key: &TranscriptKey, limit: Option<u64>) -> StoreResult<TranscriptRead> {
            self.disk.read(key, limit)
        }
    }
    let scratch = Scratch::new("store-reject");
    let path = scratch.0.join("s1.jsonl");
    let store = Arc::new(RejectOnce {
        reject: AtomicBool::new(true),
        disk: JsonlTranscriptStore::new(),
    });
    let ids = AtomicUsize::new(0);
    let recorder = Recorder::with_sources(
        store.clone(),
        TranscriptKey {
            project_root: PROJECT_ROOT.into(),
            pilot_home: scratch.0.join("home"),
            session_id: SESSION.into(),
            transcript_path: path.clone(),
        },
        clock(),
        Arc::new(move || {
            // Reusing the rejected id must work: neither history ids nor domain
            // state may be published when the store returns an error.
            Ok(format!(
                "id-{}",
                ids.fetch_add(1, Ordering::SeqCst).saturating_sub(1)
            ))
        }),
    );
    let session = SESSION.into();
    let turn = "turn".into();
    assert!(matches!(
        recorder.start_turn(&session, &turn),
        Err(RecorderError::Store(_))
    ));
    assert_eq!(recorder.snapshot_state(), (0, None));
    recorder.start_turn(&session, &turn).unwrap();
    let other = Recorder::from_writer(recorder.writer().clone());
    assert!(matches!(
        other.start_turn(&session, &turn),
        Err(RecorderError::Domain {
            code: "duplicate_turn_start",
            ..
        })
    ));
    recorder
        .record_model_request(&session, &turn, object([]))
        .unwrap();
    store.reject.store(true, Ordering::SeqCst);
    let calls = [object([("id", text("retry"))])];
    assert!(matches!(
        recorder.record_tool_calls(&session, &turn, &calls),
        Err(RecorderError::Store(_))
    ));
    assert_eq!(recorder.writer().snapshot_state().sequence, 3);
    recorder.record_tool_calls(&session, &turn, &calls).unwrap();
    recorder
        .record_tool_results(&session, &turn, &[object([("toolCallId", text("retry"))])])
        .unwrap();
    assert_eq!(chain(&path).len(), 6);
    assert_eq!(other.snapshot_state(), recorder.snapshot_state());
}

#[test]
fn recorder_and_writer_concurrent_appends_share_one_chain() {
    let scratch = Scratch::new("concurrent");
    let ctx = Ctx::new(&scratch.0);
    let recorder = Arc::new(ctx.recorder);
    let session: JsString = SESSION.into();
    let turn: JsString = "turn".into();
    recorder.start_turn(&session, &turn).unwrap();
    recorder
        .record_model_request(&session, &turn, object([]))
        .unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..16 {
                recorder
                    .writer()
                    .record_session_metadata(&session, &turn, object([]))
                    .unwrap();
            }
        });
        scope.spawn(|| {
            for _ in 0..16 {
                recorder
                    .record_model_event(&session, &turn, &object([("type", text("done"))]))
                    .unwrap();
            }
        });
    });
    recorder.record_tool_results(&session, &turn, &[]).unwrap();
    let entries = chain(&ctx.path);
    assert_eq!(entries.len(), 36);
    let ids: std::collections::HashSet<_> = entries
        .iter()
        .map(|entry| s(field(entry, "entryId")))
        .collect();
    assert_eq!(ids.len(), entries.len());
}

#[test]
fn shared_writer_and_restore_fail_closed_for_unported_active_turn_lifecycles() {
    use pilotdeck_gateway::storage::AdapterError;
    let scratch = Scratch::new("unported-lifecycle");
    let ctx = Ctx::new(&scratch.0);
    let session = SESSION.into();
    let turn = "turn".into();
    ctx.recorder.start_turn(&session, &turn).unwrap();
    let before = ctx.recorder.snapshot_state();
    for kind in [
        "durable_message",
        "assistant_message",
        "tool_result_message",
        "turn_result",
    ] {
        let draft = object([
            ("type", text(kind)),
            (
                "message",
                object([("metadata", object([("queueItemId", text("q"))]))]),
            ),
        ]);
        assert!(matches!(
            ctx.recorder.writer().record_session_event(
                &session,
                &turn,
                draft.as_object().unwrap().clone()
            ),
            Err(AdapterError::NodeHostRequired(_))
        ));
        let mut entry = draft.as_object().unwrap().clone();
        for (key, value) in [
            ("sessionId", text(SESSION)),
            ("turnId", text("turn")),
            ("sequence", JsValue::Number(2.0)),
            ("entryId", text("tail")),
        ] {
            entry.insert(key.into(), value);
        }
        let mut entries = read_transcript(&ctx.path).unwrap().entries;
        entries.push(JsValue::Object(entry));
        assert!(matches!(
            ctx.recorder.restore(&entries),
            Err(RecorderError::NodeHostRequired(_))
        ));
        assert_eq!(ctx.recorder.snapshot_state(), before);
        assert_eq!(chain(&ctx.path).len(), 1);
    }
    ctx.recorder
        .record_model_request(&session, &turn, object([]))
        .unwrap();
}
