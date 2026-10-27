//! Real-TS replay for the stateful `HeartbeatIndexer` (A6.6-edgeclaw-13).
//!
//! `tests/fixtures/edgeclaw-heartbeat-indexer/generate.mts` ran the fixed
//! `service.ts` / `heartbeat.ts` on Node v22.22.0 with a scripted loopback LLM,
//! a frozen clock and an isolated HOME. Each case records the captured L0 rows,
//! `runHeartbeat` stats (or the rejection), pipeline state, index trace, every
//! memory/global file byte and the request sequence. The concurrency cases use
//! real awaited classify/create/select calls and deterministic hooks; the Rust
//! side replays them through the same synchronous hook points.

#[path = "common/baseline.rs"]
mod baseline;
#[path = "support/llm_stub.rs"]
#[allow(dead_code)]
mod llm_stub;

use llm_stub::LlmStub;
use pilotdeck_gateway::runtime::{
    memory::edgeclaw::{
        file_memory::EdgeClawFileStores,
        heartbeat::{
            helpers::messages_from_value,
            indexer::{
                CaptureL0SessionInput, HeartbeatHook, HeartbeatIndexer, HeartbeatLogger,
                HeartbeatOptions, HeartbeatRunOptions, HeartbeatStage,
            },
        },
        llm::transport::LlmMemoryExtractor,
        messages::{MemoryMessage, MemoryRole},
        storage::{IndexingSettings, L0SessionRecord},
    },
    model::http::HyperModelTransport,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier, Mutex, atomic::AtomicU64},
    thread,
};

const ORACLE: &str = include_str!("fixtures/edgeclaw-heartbeat-indexer/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";

// -- oracle access ---------------------------------------------------------

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value
        .get(key)
        .unwrap_or_else(|| panic!("#770 oracle is missing field {key}"))
}

fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("#770 expected an array, got {other:?}"),
    }
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| panic!("#770 expected a string, got {value:?}"))
        .to_utf8()
        .expect("#770 fixture strings are UTF-8")
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert((*key).into(), value.clone());
    }
    JsValue::Object(object)
}

fn strings(values: &[String]) -> JsValue {
    JsValue::Array(
        values
            .iter()
            .map(|value| JsValue::String(value.as_str().into()))
            .collect(),
    )
}

fn messages(value: &JsValue) -> Vec<MemoryMessage> {
    messages_from_value(value)
}

fn message_values(values: &[MemoryMessage]) -> JsValue {
    JsValue::Array(values.iter().map(MemoryMessage::to_value).collect())
}

// -- canonical JSON comparison --------------------------------------------

fn canonical(value: &JsValue) -> String {
    fn sorted(value: &JsValue) -> JsValue {
        match value {
            JsValue::Object(object) => {
                let mut entries: Vec<_> = object.entries().to_vec();
                entries.sort_by(|left, right| left.0.units().cmp(right.0.units()));
                let mut next = JsObject::new();
                for (key, value) in entries {
                    next.insert(key, sorted(&value));
                }
                JsValue::Object(next)
            }
            JsValue::Array(items) => JsValue::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    stringify_json(&sorted(value))
        .expect("#770 observation serializes")
        .expect("#770 observation is not undefined")
}

// -- temp dir --------------------------------------------------------------

struct TempDir(PathBuf);
impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pdrs-770-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).expect("#770 create temp dir");
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// -- observation helpers ---------------------------------------------------

/// Replace `from` with `to` over UTF-16 units. The observation may contain
/// lone surrogates (a candidate name cut at 80 units), so path scrubbing must
/// never round-trip the string through UTF-8.
fn replace_units(value: &JsString, from: &[u16], to: &str) -> JsString {
    if from.is_empty() {
        return value.clone();
    }
    let units = value.units();
    let mut out = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if units[index..].starts_with(from) {
            out.extend(to.encode_utf16());
            index += from.len();
        } else {
            out.push(units[index]);
            index += 1;
        }
    }
    JsString::from_utf16(out)
}

fn clean_paths(value: &JsValue, root: &Path, home: &Path) -> JsValue {
    let root: Vec<u16> = root.to_string_lossy().encode_utf16().collect();
    let home: Vec<u16> = home.to_string_lossy().encode_utf16().collect();
    fn walk(value: &JsValue, root: &[u16], home: &[u16]) -> JsValue {
        match value {
            JsValue::String(text) => {
                let next = replace_units(text, home, "$HOME");
                JsValue::String(replace_units(&next, root, "$ROOT"))
            }
            JsValue::Array(items) => {
                JsValue::Array(items.iter().map(|item| walk(item, root, home)).collect())
            }
            JsValue::Object(object) => {
                let mut next = JsObject::new();
                for (key, value) in object.entries() {
                    next.insert(key.clone(), walk(value, root, home));
                }
                JsValue::Object(next)
            }
            other => other.clone(),
        }
    }
    walk(value, &root, &home)
}

fn tree(root: &Path) -> JsValue {
    fn walk(root: &Path, dir: &Path) -> Vec<JsValue> {
        let path = if dir.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(dir)
        };
        let Ok(entries) = fs::read_dir(&path) else {
            return Vec::new();
        };
        let mut entries: Vec<_> = entries
            .map(|entry| entry.expect("#770 read tree entry"))
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        let mut out = Vec::new();
        for entry in entries {
            let relative = if dir.as_os_str().is_empty() {
                PathBuf::from(entry.file_name())
            } else {
                dir.join(entry.file_name())
            };
            let file_type = entry.file_type().expect("#770 tree file type");
            if file_type.is_dir() {
                out.extend(walk(root, &relative));
            } else {
                let content = fs::read(root.join(&relative)).expect("#770 read memory file");
                out.push(object(&[
                    (
                        "relativePath",
                        JsValue::String(relative.to_string_lossy().as_ref().into()),
                    ),
                    (
                        "content",
                        JsValue::String(String::from_utf8_lossy(&content).as_ref().into()),
                    ),
                ]));
            }
        }
        out
    }
    JsValue::Array(walk(root, Path::new("")))
}

fn l0_value(record: &L0SessionRecord) -> JsValue {
    object(&[
        (
            "l0IndexId",
            JsValue::String(record.l0_index_id.as_str().into()),
        ),
        (
            "sessionKey",
            JsValue::String(record.session_key.as_str().into()),
        ),
        (
            "timestamp",
            JsValue::String(record.timestamp.as_str().into()),
        ),
        ("messages", record.messages.clone()),
        ("source", JsValue::String(record.source.as_str().into())),
        ("indexed", JsValue::Bool(record.indexed)),
        (
            "createdAt",
            JsValue::String(record.created_at.as_str().into()),
        ),
    ])
}

#[derive(Default)]
struct RecordingLogger(Mutex<Vec<(String, String)>>);
impl HeartbeatLogger for RecordingLogger {
    fn info(&self, message: &JsString) {
        self.0
            .lock()
            .expect("#770 logger lock")
            .push(("info".into(), text(&JsValue::String(message.clone()))));
    }
    fn warn(&self, message: &JsString) {
        self.0
            .lock()
            .expect("#770 logger lock")
            .push(("warn".into(), text(&JsValue::String(message.clone()))));
    }
}

// -- event hooks -----------------------------------------------------------

#[derive(Clone)]
enum Action {
    Capture {
        session_key: String,
        timestamp: String,
        messages: Vec<MemoryMessage>,
    },
    ClearSession(String),
    ClearProject,
    Close,
}

struct Event {
    stage: HeartbeatStage,
    action: Action,
    done: bool,
    capturer: HeartbeatIndexer,
    stores: Arc<EdgeClawFileStores>,
}

fn stage_for(timing: &str) -> HeartbeatStage {
    match timing {
        "afterList" => HeartbeatStage::AfterListUnindexed,
        "classify" => HeartbeatStage::BeforeClassify,
        "createNote" => HeartbeatStage::BeforeCreateNote,
        "select" => HeartbeatStage::BeforeSelectIndexProject,
        "beforeMark" => HeartbeatStage::BeforeMarkL0Indexed,
        other => panic!("#770 unknown timing {other}"),
    }
}

fn hook(event: Arc<Mutex<Event>>) -> HeartbeatHook {
    Arc::new(move |stage: HeartbeatStage, session_key: &str| {
        let mut event = event.lock().expect("#770 event lock");
        if event.done || stage != event.stage {
            return;
        }
        event.done = true;
        match event.action.clone() {
            Action::Capture {
                session_key,
                timestamp,
                messages,
            } => {
                event
                    .capturer
                    .capture_l0_session(CaptureL0SessionInput {
                        session_key,
                        timestamp: Some(timestamp),
                        messages,
                        source: None,
                    })
                    .expect("#770 hook capture");
            }
            Action::ClearSession(session) => {
                event
                    .stores
                    .clear_session_memory_data(&session)
                    .expect("#770 hook clearSession");
            }
            Action::ClearProject => {
                event
                    .stores
                    .clear_current_workspace_memory_data()
                    .expect("#770 hook clearProject");
            }
            Action::Close => {
                let _ = event.stores.close();
            }
        }
        let _ = session_key;
    })
}

// -- case runner -----------------------------------------------------------

fn config(origin: &str) -> JsValue {
    parse_json(&format!(
        r#"{{"agents":{{"defaults":{{"model":{{"primary":"p/m"}}}}}},"models":{{"providers":{{"p":{{"api":"openai-completions","baseUrl":"{origin}/v1","apiKey":"fixture-key","models":[{{"id":"m"}}]}}}}}}}}"#
    ))
    .expect("#770 config json")
}

async fn run_case(case: &JsValue) -> JsValue {
    let name = text(field(case, "name"));
    let mode = text(field(case, "mode"));
    let root = TempDir::new(&name);
    let root_path = root.path();
    let home = root_path.join("home");
    fs::create_dir_all(&home).expect("#770 create home");
    let workspace = if mode == "general" {
        home.join(".pilotdeck")
    } else {
        root_path.join("workspace")
    };
    fs::create_dir_all(&workspace).expect("#770 create workspace");
    let clock = Arc::new(|| NOW.to_string());
    let stores = Arc::new(
        EdgeClawFileStores::with_workspace_clock(
            root_path.join("control.sqlite"),
            root_path.join("memory"),
            root_path.join("root").join("global"),
            &workspace,
            home.join(".pilotdeck"),
            clock.clone(),
        )
        .expect("#770 open repository"),
    );
    stores
        .storage
        .set_pipeline_state(
            "workspaceDir",
            &JsValue::String(workspace.to_string_lossy().as_ref().into()),
        )
        .expect("#770 set workspaceDir");

    let logger = Arc::new(RecordingLogger::default());
    let steps: Vec<Value> = array(field(case, "llmSteps"))
        .iter()
        .map(|step| serde_json::from_str(&canonical(step)).expect("#770 llm step"))
        .collect();
    let server = LlmStub::start(steps, Arc::new(AtomicU64::new(0))).await;
    let extractor = Arc::new(LlmMemoryExtractor::new(
        config(&server.origin),
        Arc::new(HyperModelTransport::new()),
        Arc::new(()),
    ));
    let options = || HeartbeatOptions {
        batch_size: None,
        source: Some("edgeclaw".into()),
        settings: IndexingSettings {
            reasoning_mode: "answer_first".into(),
            auto_index_interval_minutes: 30,
            auto_dream_interval_minutes: 60,
        },
        logger: Some(logger.clone()),
        clock: Some(clock.clone()),
    };
    let capturer = HeartbeatIndexer::new(stores.clone(), extractor.clone(), options());

    // Seed project metadata exactly through the same store call.
    let mut seeds = Vec::new();
    for seed in array(field(case, "seeds")) {
        let mut input = JsObject::new();
        input.insert(
            "projectName".into(),
            JsValue::String(
                field(seed, "projectName")
                    .as_string()
                    .cloned()
                    .unwrap_or_default(),
            ),
        );
        input.insert(
            "description".into(),
            JsValue::String(
                field(seed, "description")
                    .as_string()
                    .cloned()
                    .unwrap_or_default(),
            ),
        );
        input.insert(
            "status".into(),
            JsValue::String(
                field(seed, "status")
                    .as_string()
                    .cloned()
                    .unwrap_or_default(),
            ),
        );
        let meta = stores
            .get_file_memory_store()
            .upsert_project_meta(&input)
            .expect("#770 seed project");
        seeds.push(meta);
    }

    let mut captures = Vec::new();
    for capture in array(field(case, "captures")) {
        let input = field(capture, "input");
        let session_key = text(field(input, "sessionKey"));
        let timestamp = text(field(input, "timestamp"));
        let input_messages = messages(field(input, "messages"));
        let source = input
            .get("source")
            .and_then(JsValue::as_string)
            .map(|value| text(&JsValue::String(value.clone())));
        let record = capturer
            .capture_l0_session(CaptureL0SessionInput {
                session_key: session_key.clone(),
                timestamp: Some(timestamp.clone()),
                messages: input_messages.clone(),
                source,
            })
            .expect("#770 capture l0");
        let result = object(&[
            ("captured", JsValue::Bool(record.is_some())),
            ("normalizedMessages", message_values(&input_messages)),
            ("sessionKey", JsValue::String(session_key.as_str().into())),
        ]);
        captures.push(object(&[("input", input.clone()), ("result", result)]));
    }
    let mut direct_captures = Vec::new();
    for capture in array(field(case, "directCaptures")) {
        let input = field(capture, "input");
        let record = capturer
            .capture_l0_session(CaptureL0SessionInput {
                session_key: text(field(input, "sessionKey")),
                timestamp: Some(text(field(input, "timestamp"))),
                messages: messages(field(input, "messages")),
                source: input
                    .get("source")
                    .and_then(JsValue::as_string)
                    .map(|value| text(&JsValue::String(value.clone()))),
            })
            .expect("#770 direct capture l0");
        direct_captures.push(object(&[
            ("input", input.clone()),
            (
                "record",
                record.as_ref().map(l0_value).unwrap_or(JsValue::Null),
            ),
        ]));
    }

    let timing = case
        .get("timing")
        .and_then(JsValue::as_string)
        .map(|value| text(&JsValue::String(value.clone())));
    let action = case
        .get("action")
        .and_then(JsValue::as_string)
        .map(|value| text(&JsValue::String(value.clone())));
    let mut indexer = capturer.clone();
    if let (Some(timing), Some(action)) = (timing.as_deref(), action.as_deref()) {
        let action = match action {
            "capture" => {
                let event = field(case, "eventCapture");
                Action::Capture {
                    session_key: text(field(event, "sessionKey")),
                    timestamp: text(field(event, "timestamp")),
                    messages: messages(field(event, "messages")),
                }
            }
            "clearSession" => Action::ClearSession(text(
                field(&array(field(case, "captures"))[0], "input")
                    .get("sessionKey")
                    .expect("#770 session key"),
            )),
            "clearProject" => Action::ClearProject,
            "close" => Action::Close,
            other => panic!("#770 unknown action {other}"),
        };
        indexer = indexer.with_hook(hook(Arc::new(Mutex::new(Event {
            stage: stage_for(timing),
            action,
            done: false,
            capturer: capturer.clone(),
            stores: stores.clone(),
        }))));
    }

    let result = if array(field(case, "directCaptures")).is_empty() {
        match indexer
            .run_heartbeat(HeartbeatRunOptions {
                reason: Some("manual".into()),
                ..HeartbeatRunOptions::default()
            })
            .await
        {
            Ok(stats) => object(&[("stats", stats.to_value())]),
            Err(error) => object(&[(
                "error",
                object(&[
                    ("name", JsValue::String("Error".into())),
                    (
                        "message",
                        JsValue::String(error.to_string().as_str().into()),
                    ),
                ]),
            )]),
        }
    } else {
        JsValue::Null
    };

    let db = match stores.storage.list_all_l0() {
        Ok(l0) => {
            let traces = stores
                .storage
                .list_recent_index_traces(20)
                .expect("#770 list traces");
            let mut keys = vec!["lastIndexedAt".to_string()];
            for capture in array(field(case, "captures")) {
                keys.push(format!(
                    "lastIndexedCursor:{}",
                    text(field(field(capture, "input"), "sessionKey"))
                ));
            }
            keys.push("workspaceDir".to_string());
            let mut pipeline = JsObject::new();
            for key in keys {
                let value = stores
                    .storage
                    .get_pipeline_state(&key)
                    .expect("#770 pipeline state")
                    .unwrap_or(JsValue::Null);
                pipeline.insert(key.as_str().into(), value);
            }
            object(&[
                ("l0", JsValue::Array(l0.iter().map(l0_value).collect())),
                ("traces", JsValue::Array(traces)),
                ("pipeline", JsValue::Object(pipeline)),
            ])
        }
        Err(error) => object(&[(
            "error",
            object(&[
                ("name", JsValue::String("Error".into())),
                (
                    "message",
                    JsValue::String(error.to_string().as_str().into()),
                ),
            ]),
        )]),
    };
    let files = object(&[
        (
            "memory",
            tree(stores.get_file_memory_store().get_root_dir()),
        ),
        (
            "global",
            tree(stores.get_global_user_store().get_root_dir()),
        ),
    ]);
    let requests = JsValue::Array(
        server
            .requests
            .lock()
            .expect("#770 stub requests")
            .iter()
            .map(|request| {
                let headers = parse_json(
                    &request
                        .get("headers")
                        .expect("#770 stub request headers")
                        .to_string(),
                )
                .expect("#770 stub request headers parse");
                object(&[
                    (
                        "method",
                        JsValue::String(request["method"].as_str().unwrap_or_default().into()),
                    ),
                    (
                        "url",
                        JsValue::String(request["url"].as_str().unwrap_or_default().into()),
                    ),
                    ("headers", headers),
                    (
                        "body",
                        JsValue::String(
                            request["body"]
                                .as_str()
                                .expect("#770 stub request body")
                                .into(),
                        ),
                    ),
                ])
            })
            .collect(),
    );
    let logs = JsValue::Array(
        logger
            .0
            .lock()
            .expect("#770 logs")
            .iter()
            .map(|(level, message)| {
                object(&[
                    ("level", JsValue::String(level.as_str().into())),
                    ("message", strings(std::slice::from_ref(message))),
                ])
            })
            .collect(),
    );
    server.close().await;

    let observed = object(&[
        ("name", JsValue::String(name.as_str().into())),
        ("mode", JsValue::String(mode.as_str().into())),
        (
            "timing",
            case.get("timing")
                .filter(|value| !value.is_undefined())
                .cloned()
                .unwrap_or(JsValue::Null),
        ),
        (
            "action",
            case.get("action")
                .filter(|value| !value.is_undefined())
                .cloned()
                .unwrap_or(JsValue::Null),
        ),
        (
            "eventCapture",
            case.get("eventCapture")
                .filter(|value| !value.is_undefined())
                .cloned()
                .unwrap_or(JsValue::Null),
        ),
        ("seeds", JsValue::Array(seeds)),
        ("captures", JsValue::Array(captures)),
        ("directCaptures", JsValue::Array(direct_captures)),
        ("llmSteps", field(case, "llmSteps").clone()),
        ("result", result),
        ("db", db),
        ("files", files),
        ("requests", requests),
        ("logs", logs),
    ]);
    clean_paths(&observed, root_path, &home)
}

fn prompt_debug_values(observation: &JsValue) -> JsValue {
    let mut debug = Vec::new();
    if let Some(JsValue::Array(traces)) = observation.get("db").and_then(|db| db.get("traces")) {
        for trace in traces {
            if let Some(JsValue::Array(steps)) = trace.get("steps") {
                for step in steps {
                    if let Some(value) = step.get("promptDebug") {
                        debug.push(value.clone());
                    }
                }
            }
        }
    }
    JsValue::Array(debug)
}

fn stored_candidate_fields(observation: &JsValue) -> JsValue {
    let mut fields = Vec::new();
    if let Some(JsValue::Array(traces)) = observation.get("db").and_then(|db| db.get("traces")) {
        for trace in traces {
            if let Some(JsValue::Array(results)) = trace.get("storedResults") {
                for result in results {
                    fields.push(JsValue::Array(vec![
                        result
                            .get("candidateType")
                            .cloned()
                            .unwrap_or(JsValue::Null),
                        result
                            .get("candidateName")
                            .cloned()
                            .unwrap_or(JsValue::Null),
                        result.get("scope").cloned().unwrap_or(JsValue::Null),
                    ]));
                }
            }
        }
    }
    JsValue::Array(fields)
}

async fn assert_case(name: &str) {
    let oracle = baseline::oracle(ORACLE);
    let parsed = parse_json(ORACLE).expect("#770 oracle json");
    let case = array(field(&parsed, "cases"))
        .iter()
        .find(|case| text(field(case, "name")) == name)
        .unwrap_or_else(|| panic!("#770 oracle case {name}"));
    let observed = run_case(case).await;
    // Targeted anchors first: a mutation that changes a request body or the
    // candidate source must fail with a stable, test-specific message instead
    // of the generic full-observation diff.
    assert_eq!(
        canonical(field(&observed, "requests")),
        canonical(field(case, "requests")),
        "#770 case {name} request method/url/headers/body differs"
    );
    assert_eq!(
        canonical(&prompt_debug_values(&observed)),
        canonical(&prompt_debug_values(case)),
        "#770 case {name} promptDebug differs"
    );
    assert_eq!(
        canonical(&stored_candidate_fields(&observed)),
        canonical(&stored_candidate_fields(case)),
        "#770 case {name} storedResults candidate source differs"
    );
    if canonical(&observed) != canonical(case) {
        panic!(
            "#770 case {name} differs\n  actual:   {}\n  expected: {}",
            canonical(&observed),
            canonical(case)
        );
    }
    assert_eq!(
        oracle
            .get("nodeVersion")
            .and_then(JsValue::as_string)
            .map(|value| value.to_utf8().unwrap()),
        Some("v22.22.0".into()),
        "#770 oracle node version"
    );
}

// The repository/file-store pair keeps process-wide operation state; run the
// replay cases one at a time so a sibling test cannot observe a half-written
// manifest from another temp root.
static HEARTBEAT_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

macro_rules! case_test {
    ($function:ident, $name:literal) => {
        #[tokio::test]
        async fn $function() {
            let _guard = HEARTBEAT_TEST_LOCK.lock().await;
            assert_case($name).await;
        }
    };
}

case_test!(single_project_cycle_matches_ts, "single-project");
case_test!(single_all_kinds_matches_ts, "single-all-kinds");
case_test!(general_create_matches_ts, "general-create");
case_test!(general_attach_matches_ts, "general-attach");
case_test!(single_long_emoji_name_matches_ts, "single-long-emoji-name");
case_test!(
    general_attach_long_emoji_name_matches_ts,
    "general-attach-long-emoji-name"
);
case_test!(single_non_bmp_name_matches_ts, "single-non-bmp-name");
case_test!(
    capture_duplicate_suppression_matches_ts,
    "capture-duplicate-suppression"
);
case_test!(
    matrix_after_list_clear_session_matches_ts,
    "matrix-single-afterList-clearSession"
);
case_test!(
    matrix_classify_capture_matches_ts,
    "matrix-single-classify-capture"
);
case_test!(
    matrix_classify_clear_session_matches_ts,
    "matrix-single-classify-clearSession"
);
case_test!(
    matrix_create_note_clear_session_matches_ts,
    "matrix-single-createNote-clearSession"
);
case_test!(
    matrix_before_mark_clear_session_matches_ts,
    "matrix-single-beforeMark-clearSession"
);
case_test!(
    matrix_general_classify_clear_session_matches_ts,
    "matrix-general-classify-clearSession"
);
case_test!(
    matrix_general_create_note_clear_session_matches_ts,
    "matrix-general-createNote-clearSession"
);
case_test!(
    matrix_general_select_clear_project_matches_ts,
    "matrix-general-select-clearProject"
);
case_test!(
    matrix_classify_close_matches_ts,
    "matrix-single-classify-close"
);
case_test!(
    matrix_before_mark_close_matches_ts,
    "matrix-single-beforeMark-close"
);
case_test!(
    matrix_general_select_close_matches_ts,
    "matrix-general-select-close"
);

// P1-1: `captureL0Session` reads the newest L0 row and then inserts in a
// second storage call. TS is safe because the function is synchronous and has
// no await; the Rust indexer is `Clone + Send + Sync`, so two threads can both
// read the same `recent` row before either insert. This test starts eight
// threads on one barrier and requires exactly one inserted row and exactly one
// `Some` return, which is the same outcome the real TS produces for any
// number of concurrent callers.
#[test]
fn capture_l0_session_two_threads_is_atomic() {
    let root = TempDir::new("capture-race");
    let home = root.path().join("home");
    fs::create_dir_all(&home).expect("#770-P1-1 create home");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).expect("#770-P1-1 create workspace");
    let clock = Arc::new(|| NOW.to_string());
    let stores = Arc::new(
        EdgeClawFileStores::with_workspace_clock(
            root.path().join("control.sqlite"),
            root.path().join("memory"),
            root.path().join("root").join("global"),
            &workspace,
            home.join(".pilotdeck"),
            clock.clone(),
        )
        .expect("#770-P1-1 open repository"),
    );
    let extractor = Arc::new(LlmMemoryExtractor::new(
        config("http://127.0.0.1:1"),
        Arc::new(HyperModelTransport::new()),
        Arc::new(()),
    ));
    let indexer = HeartbeatIndexer::new(
        stores.clone(),
        extractor,
        HeartbeatOptions {
            batch_size: None,
            source: Some("edgeclaw".into()),
            settings: IndexingSettings {
                reasoning_mode: "answer_first".into(),
                auto_index_interval_minutes: 30,
                auto_dream_interval_minutes: 60,
            },
            logger: None,
            clock: Some(clock),
        },
    );

    const THREADS: usize = 8;
    let barrier = Arc::new(Barrier::new(THREADS));
    let mut handles = Vec::new();
    for index in 0..THREADS {
        let indexer = indexer.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            indexer
                .capture_l0_session(CaptureL0SessionInput {
                    session_key: "s1".into(),
                    timestamp: Some(format!("2026-10-22T00:00:0{index}.000Z")),
                    messages: vec![MemoryMessage {
                        msg_id: None,
                        role: MemoryRole::User,
                        content: "same user".into(),
                    }],
                    source: None,
                })
                .expect("#770-P1-1 capture_l0_session must not fail")
                .is_some()
        }));
    }
    let captured = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("#770-P1-1 worker thread must not panic")
        })
        .filter(|captured| *captured)
        .count();
    assert_eq!(
        captured, 1,
        "#770-P1-1 exactly one concurrent capture may insert, the rest must be suppressed as duplicates"
    );
    let rows = stores
        .storage
        .list_all_l0()
        .expect("#770-P1-1 list l0 rows");
    assert_eq!(
        rows.len(),
        1,
        "#770-P1-1 concurrent capture of the same session/messages must leave exactly one L0 row"
    );
    // The same lock must still insert when the predicate is false: another
    // session, and a same-session transcript with new content.
    let other_session = indexer
        .capture_l0_session(CaptureL0SessionInput {
            session_key: "s2".into(),
            timestamp: Some("2026-10-22T00:00:09.000Z".into()),
            messages: vec![MemoryMessage {
                msg_id: None,
                role: MemoryRole::User,
                content: "same user".into(),
            }],
            source: None,
        })
        .expect("#770-P1-1 other-session capture must not fail");
    assert!(
        other_session.is_some(),
        "#770-P1-1 a different session must still insert"
    );
    let new_content = indexer
        .capture_l0_session(CaptureL0SessionInput {
            session_key: "s2".into(),
            timestamp: Some("2026-10-22T00:00:10.000Z".into()),
            messages: vec![MemoryMessage {
                msg_id: None,
                role: MemoryRole::User,
                content: "new user".into(),
            }],
            source: None,
        })
        .expect("#770-P1-1 new-content capture must not fail");
    assert!(
        new_content.is_some(),
        "#770-P1-1 a same-session transcript with new content must still insert"
    );
    assert_eq!(
        stores
            .storage
            .list_all_l0()
            .expect("#770-P1-1 list l0 rows after predicate-false captures")
            .len(),
        3,
        "#770-P1-1 predicate-false captures must add rows"
    );
}

#[ignore]
#[test]
fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-heartbeat-indexer");
}
