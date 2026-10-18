//! Durable recorder contract tests. Scenario outcomes and transcript bytes
//! come from the real TS AgentSessionEventRecorder
//! (tests/fixtures/recorder/generate.mts).
use pilotdeck_gateway::storage::recorder::{Recorder, RecorderError};
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
    parse_json(&text).unwrap()
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
        let dir = clean(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../_tmp/20261018-a6-recorder/test-scratch")
                .join(format!(
                    "{name}-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )),
        );
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
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
        let path = dir.join(format!("{SESSION}.jsonl"));
        let key = TranscriptKey {
            project_root: PROJECT_ROOT.into(),
            pilot_home: dir.join("home"),
            session_id: SESSION.into(),
            transcript_path: path.clone(),
        };
        Self {
            path,
            recorder: Recorder::with_clock(Arc::new(JsonlTranscriptStore::new()), key, clock()),
        }
    }
    fn reopen(&mut self) {
        let read = read_transcript(&self.path).unwrap();
        self.recorder.restore(&read.entries).unwrap();
    }
}

fn error_outcome(error: &RecorderError) -> JsValue {
    let (name, code) = match error {
        RecorderError::State(_) | RecorderError::Store(_) => ("Error", None),
        RecorderError::Domain { code, .. } => ("SessionDomainValidationError", Some(*code)),
        RecorderError::Envelope { code, .. } => ("SessionEventValidationError", Some(*code)),
        RecorderError::Type(_) => ("TypeError", None),
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
        .unwrap_or_default();
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
        scenarios.len() >= 5,
        "oracle has {} scenarios",
        scenarios.len()
    );
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
            let outcome = run_step(&mut ctx, step);
            let mut step = step.as_object().unwrap().clone();
            step.insert("outcome".into(), outcome);
            replayed.push(JsValue::Object(step));
        }
        let mut actual = JsObject::new();
        actual.insert("name".into(), field(scenario, "name").clone());
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
        field(&entries[16], "event").get("raw").is_none(),
        "Rust strips event raw"
    );
    assert!(
        field(&entries[18], "call").get("raw").is_none(),
        "Rust strips call raw"
    );
    run_generator("--cross-ts-verify", &ts_first.0);
    let entries = chain(&path);
    assert_eq!(entries.len(), 28, "TS continues past the Rust turn");
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
    assert_eq!(chain(&rust_path).len(), 21);
    rust_continue(&rust_path, "turn-rust-after-ts");
    assert_eq!(chain(&rust_path).len(), 28);
}
