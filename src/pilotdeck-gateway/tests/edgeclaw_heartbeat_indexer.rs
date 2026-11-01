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
#[path = "../src/mutants.rs"]
mod mutants;

use llm_stub::LlmStub;
use pilotdeck_gateway::runtime::{
    memory::edgeclaw::{
        file_memory::{ClearSessionStage, EdgeClawFileStores, FileMemoryError},
        heartbeat::{
            helpers::messages_from_value,
            indexer::{
                CaptureL0SessionInput, HeartbeatError, HeartbeatHook, HeartbeatIndexer,
                HeartbeatLogger, HeartbeatOptions, HeartbeatRunOptions, HeartbeatStage,
                HeartbeatStats,
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

// #917: the six M13 clearSession cells whose expectation flips from the TS
// baseline (which resurrects the cleared session's file after the clear) to
// the registered divergences
// `A6.6-EDGECLAW-HEARTBEAT-CLEAR-RESURRECT-001` / `-002`
// (human decision 2026-10-27, #914 option A). `oracle.json` itself stays
// byte-identical to TS, so the regeneration guard keeps proving the TS side.
const FIX_ORACLE: &str = include_str!("fixtures/edgeclaw-heartbeat-indexer/fix-917.json");
// fix-r1 adds the sixth cell: the TS baseline never recorded a
// `matrix-general-select-clearSession` oracle.json case, so its TS evidence
// rides along inside the fix-917.json cell under `ts` instead.
const DIVERGED_CASES: [&str; 6] = [
    "matrix-single-afterList-clearSession",
    "matrix-single-classify-clearSession",
    "matrix-single-createNote-clearSession",
    "matrix-general-classify-clearSession",
    "matrix-general-createNote-clearSession",
    "matrix-general-select-clearSession",
];

// -- oracle access ---------------------------------------------------------

// #1263 skip lines must reach default `cargo test` output: libtest captures
// the eprintln! macro for a passing test, so the line is written through the
// stderr handle directly (the #448 unsafe_boundary precedent, probed there on
// rustc 1.99.1: handle writes bypass the capture; the macro's output does not).
fn stderr_skip_line(text: &str) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(text.as_bytes());
    let _ = std::io::stderr().flush();
}

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

struct TempDir {
    owner: PathBuf,
    repository: PathBuf,
}
impl TempDir {
    fn new(name: &str) -> Self {
        Self::new_in(&std::env::temp_dir(), name)
    }
    fn new_in(parent: &Path, name: &str) -> Self {
        let owner = parent.join(format!(
            "pdrs-770-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        // General-mode discovery scans memory.parent().parent(). A UUID at
        // the repository level alone leaves that root at the shared temp dir,
        // where sibling tests' half-open SQLite files become discoverable.
        let repository = owner.join("case");
        fs::create_dir_all(&repository).expect("#1098 create isolated repository root");
        Self { owner, repository }
    }
    fn path(&self) -> &Path {
        if crate::mutants::on("heartbeat-shared-discovery-root") {
            return &self.owner;
        }
        &self.repository
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.owner);
    }
}

#[test]
fn heartbeat_roots_do_not_discover_sibling_repository() {
    // Both siblings live under a private lab, even under the old-layout
    // negative control. No probe scans the real system temporary directory.
    let lab = TempDir::new("isolation-lab");
    let general_root = TempDir::new_in(lab.path(), "general");
    let peer_root = TempDir::new_in(lab.path(), "peer");
    let open = |root: &Path, general: bool| {
        let home = root.join("home/.pilotdeck");
        let workspace = if general {
            home.clone()
        } else {
            root.join("workspace")
        };
        fs::create_dir_all(&workspace).expect("#1098 create discovery workspace");
        let repository = EdgeClawFileStores::with_workspace_clock(
            root.join("control.sqlite"),
            root.join("memory"),
            root.join("root/global"),
            &workspace,
            &home,
            Arc::new(|| NOW.into()),
        )
        .expect("#1098 open discovery repository");
        repository
            .storage
            .set_pipeline_state(
                "workspaceDir",
                &JsValue::String(workspace.to_string_lossy().as_ref().into()),
            )
            .expect("#1098 persist discovery workspace");
        repository
    };
    let general = open(general_root.path(), true);
    let peer = open(peer_root.path(), false);
    let mut seed = JsObject::new();
    seed.insert("projectName".into(), "#1098 peer".into());
    seed.insert("description".into(), "".into());
    seed.insert("status".into(), "active".into());
    peer.get_file_memory_store()
        .upsert_project_meta(&seed)
        .expect("#1098 seed peer project");
    let names = |repository: &EdgeClawFileStores| {
        let catalog = repository
            .list_readable_project_catalog()
            .expect("#1098 read real project catalog");
        let JsValue::Array(entries) = &catalog else {
            panic!("#1098 catalog must be an array")
        };
        entries
            .iter()
            .map(|entry| {
                entry
                    .get("projectName")
                    .and_then(JsValue::as_string)
                    .expect("#1098 catalog project name")
                    .to_utf8()
                    .expect("#1098 catalog project name is UTF-8")
            })
            .collect::<Vec<_>>()
    };
    let peer_names = names(&peer);
    assert_eq!(
        peer_names,
        ["#1098 peer"],
        "#1098 peer catalog must prove discovery seed is live"
    );
    let oracle: Value = serde_json::from_str(include_str!(
        "fixtures/edgeclaw-heartbeat-isolation/oracle.json"
    ))
    .expect("#1098 isolation TS oracle");
    assert_eq!(
        serde_json::json!({"peer":peer_names,"general":names(&general)}),
        oracle["layouts"]["isolated"],
        "#1098 heartbeat discovery must not open sibling repositories"
    );
}

#[test]
#[ignore = "requires Node v22.22.0 and the fixed TS checkout"]
fn isolation_regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-heartbeat-isolation");
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
            auto_index_interval_minutes: 30.0,
            auto_dream_interval_minutes: 60.0,
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

// #917: the same replay shape, but the expectation is the registered
// divergence in `fix-917.json` instead of the TS cell.
macro_rules! diverged_case_test {
    ($function:ident, $name:literal) => {
        #[tokio::test]
        async fn $function() {
            let _guard = HEARTBEAT_TEST_LOCK.lock().await;
            assert_diverged_case($name).await;
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
    matrix_classify_capture_matches_ts,
    "matrix-single-classify-capture"
);
// #917: the six clearSession resurrection cells replay the same TS inputs
// but expect the registered divergence (`fix-917.json`), not the TS outcome.
diverged_case_test!(
    matrix_after_list_clear_session_no_resurrection,
    "matrix-single-afterList-clearSession"
);
diverged_case_test!(
    matrix_classify_clear_session_no_resurrection,
    "matrix-single-classify-clearSession"
);
diverged_case_test!(
    matrix_create_note_clear_session_no_resurrection,
    "matrix-single-createNote-clearSession"
);
case_test!(
    matrix_before_mark_clear_session_matches_ts,
    "matrix-single-beforeMark-clearSession"
);
diverged_case_test!(
    matrix_general_classify_clear_session_no_resurrection,
    "matrix-general-classify-clearSession"
);
diverged_case_test!(
    matrix_general_create_note_clear_session_no_resurrection,
    "matrix-general-createNote-clearSession"
);
// #917 fix-r1: the select-window cell. The TS baseline never recorded it in
// oracle.json, so the replay input comes from the fix cell itself and the TS
// evidence is the gated probe observation under its `ts` field.
diverged_case_test!(
    matrix_general_select_clear_session_no_resurrection,
    "matrix-general-select-clearSession"
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
                auto_index_interval_minutes: 30.0,
                auto_dream_interval_minutes: 60.0,
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

// -- #917 registered divergence ---------------------------------------------

fn fix_case<'a>(fix: &'a JsValue, name: &str) -> &'a JsValue {
    fix.get("cases")
        .and_then(|cases| cases.get(name))
        .unwrap_or_else(|| panic!("#917 fix-917.json is missing case {name}"))
}

/// Same replay as [`assert_case`], but the expectation is the #917 registered
/// divergence in `fix-917.json` instead of the TS cell: the session was
/// cleared while the classify/createNote/select awaits were in flight, so the
/// turn's candidates must be discarded instead of writing the file the TS
/// baseline resurrects.
async fn assert_diverged_case(name: &str) {
    let parsed = parse_json(ORACLE).expect("#917 oracle json");
    let fix = parse_json(FIX_ORACLE).expect("#917 fix-917.json");
    let expected = fix_case(&fix, name);
    // fix-r1: the sixth cell has no oracle.json twin (the TS baseline never
    // recorded `matrix-general-select-clearSession`), so its input is the fix
    // cell itself; the TS probe evidence under `ts` is not a replay input.
    let case = array(field(&parsed, "cases"))
        .iter()
        .find(|case| text(field(case, "name")) == name)
        .unwrap_or(expected);
    let observed = run_case(case).await;
    assert_eq!(
        canonical(field(&observed, "requests")),
        canonical(field(expected, "requests")),
        "#917 case {name} request method/url/headers/body differs"
    );
    assert_eq!(
        canonical(&prompt_debug_values(&observed)),
        canonical(&prompt_debug_values(expected)),
        "#917 case {name} promptDebug differs"
    );
    assert_eq!(
        canonical(&stored_candidate_fields(&observed)),
        canonical(&stored_candidate_fields(expected)),
        "#917 case {name} storedResults candidate source differs"
    );
    if canonical(&observed) != canonical(&without_ts(expected)) {
        panic!(
            "#917 case {name} differs\n  actual:   {}\n  expected: {}",
            canonical(&observed),
            canonical(&without_ts(expected))
        );
    }
}

/// The fix-r1 sixth cell carries its TS probe observation under `ts`, which is
/// evidence for the divergence registration, not part of the replay
/// expectation; strip it before the whole-observation comparison.
fn without_ts(value: &JsValue) -> JsValue {
    match value {
        JsValue::Object(object) => {
            let mut next = JsObject::new();
            for (key, item) in object.entries().to_vec() {
                if key.to_utf8().map(|key| key == "ts").unwrap_or(false) {
                    continue;
                }
                next.insert(key, item);
            }
            JsValue::Object(next)
        }
        other => panic!("#917 fix-917.json case must be an object, got {other:?}"),
    }
}

/// `fix-917.json` must override exactly the registered resurrection cells,
/// and every override must really remove what the TS cell still records: the
/// TS cell writes at least one `Project/` / `GeneralProjects/` file, reports
/// `writtenFiles > 0` and keeps its L0 rows deleted; the divergence writes no
/// such file, reports `writtenFiles = 0`, keeps the L0 rows deleted and keeps
/// the TS request sequence and promptDebug bytes untouched. The fix-r1 sixth
/// cell has no oracle.json twin, so its TS side is the gated probe observation
/// under `ts`: same input, same request sequence, but the routing step's
/// promptDebug drops with the discarded persist (strict prefix, see below).
#[test]
fn registered_divergence_covers_exactly_the_resurrect_cells() {
    let parsed = parse_json(ORACLE).expect("#917 oracle json");
    let fix = parse_json(FIX_ORACLE).expect("#917 fix-917.json");
    let registered = match field(&fix, "cases") {
        JsValue::Object(cases) => cases
            .entries()
            .iter()
            .map(|(key, _)| {
                key.to_utf8()
                    .expect("#917 fix-917.json case keys must be UTF-8")
            })
            .collect::<Vec<_>>(),
        other => panic!("#917 fix-917.json cases must be an object, got {other:?}"),
    };
    assert_eq!(
        registered,
        DIVERGED_CASES.to_vec(),
        "#917 fix-917.json must override exactly the registered M13 resurrection cells"
    );
    // fix-r2 (review opus P2): the first five cells are registered by `-001`,
    // the sixth by `-002`; the ledger pointers must name both so no cell
    // points at an entry that does not cover it.
    let divergences = array(field(&fix, "registeredDivergence"))
        .iter()
        .map(text)
        .collect::<Vec<_>>();
    assert_eq!(
        divergences,
        vec![
            "A6.6-EDGECLAW-HEARTBEAT-CLEAR-RESURRECT-001",
            "A6.6-EDGECLAW-HEARTBEAT-CLEAR-RESURRECT-002",
        ],
        "#917 fix-917.json must point at both diff ledger entries (-001: the first five cells, -002: the sixth)"
    );
    let resurrected = |case: &JsValue| {
        // fix-r1: the general-select cell seeds `GeneralProjects/` metadata
        // before the heartbeat, so seeded paths are not resurrection output.
        let seeded = array(field(case, "seeds"))
            .iter()
            .map(|seed| text(field(seed, "relativePath")))
            .collect::<Vec<_>>();
        array(field(field(case, "files"), "memory"))
            .iter()
            .filter(|file| {
                let path = text(field(file, "relativePath"));
                path.starts_with("Project/")
                    || (path.starts_with("GeneralProjects/") && !seeded.contains(&path))
            })
            .count()
    };
    let written_files = |case: &JsValue| {
        field(case, "result")
            .get("stats")
            .and_then(|stats| stats.get("writtenFiles"))
            .and_then(JsValue::as_number)
            .unwrap_or_default()
    };
    for name in DIVERGED_CASES {
        let expected = fix_case(&fix, name);
        // fix-r1: the sixth cell has no oracle.json twin, so its TS baseline
        // is the gated probe observation riding along under `ts`.
        let oracle = array(field(&parsed, "cases"))
            .iter()
            .find(|case| text(field(case, "name")) == name);
        let ts = oracle.unwrap_or_else(|| field(expected, "ts"));
        assert!(
            resurrected(ts) > 0,
            "#917 TS cell {name} must still record the resurrected file in its TS baseline"
        );
        assert_eq!(
            resurrected(expected),
            0,
            "#917 divergence cell {name} must not write Project/GeneralProjects files"
        );
        assert!(
            written_files(ts) > 0.0,
            "#917 TS cell {name} must record writtenFiles > 0"
        );
        assert_eq!(
            written_files(expected),
            0.0,
            "#917 divergence cell {name} must record writtenFiles = 0"
        );
        assert!(
            array(field(field(ts, "db"), "l0")).is_empty(),
            "#917 TS cell {name} must record the cleared L0 rows"
        );
        assert!(
            array(field(field(expected, "db"), "l0")).is_empty(),
            "#917 divergence cell {name} must keep the L0 rows deleted"
        );
        assert_eq!(
            canonical(field(ts, "requests")),
            canonical(field(expected, "requests")),
            "#917 divergence cell {name} must keep the TS request sequence"
        );
        if oracle.is_some() {
            assert_eq!(
                canonical(&prompt_debug_values(ts)),
                canonical(&prompt_debug_values(expected)),
                "#917 divergence cell {name} must keep the TS promptDebug"
            );
        } else {
            // fix-r1: the sixth cell's routing step carries the select
            // promptDebug in TS; the divergence drops it together with the
            // discarded persist, so the fix must keep the probe's earlier
            // classify/create promptDebug entries one for one.
            let (probe_debug, kept_debug) =
                (prompt_debug_values(ts), prompt_debug_values(expected));
            match (&probe_debug, &kept_debug) {
                (JsValue::Array(probe), JsValue::Array(kept)) => {
                    assert!(
                        !kept.is_empty()
                            && kept.len() < probe.len()
                            && kept
                                .iter()
                                .zip(probe.iter())
                                .all(|(kept, probe)| canonical(kept) == canonical(probe)),
                        "#917 fix-r1 cell {name} must keep the probe classify/create promptDebug"
                    );
                }
                _ => panic!("#917 fix-r1 cell {name} promptDebug must be arrays"),
            }
        }
        if let Some(probe) = expected.get("ts") {
            // fix-r1: the probe observation must be the same experiment as the
            // divergence cell — identical inputs, identical request sequence —
            // so the persist flip is attributable to the fix alone.
            for key in [
                "name", "mode", "timing", "action", "seeds", "captures", "llmSteps",
            ] {
                assert_eq!(
                    canonical(field(probe, key)),
                    canonical(field(expected, key)),
                    "#917 fix-r1 cell {name} ts probe {key} must match the divergence input"
                );
            }
            assert_eq!(
                canonical(field(probe, "requests")),
                canonical(field(expected, "requests")),
                "#917 fix-r1 cell {name} ts probe requests must match the divergence replay"
            );
        }
    }
}

// #917: the deterministic hook injects `clearSession` at the point where the
// real-TS probe released the gated createNote request and asserts it hit the
// target session. The persist after the await must then discard the turn — no
// file is written and no storedResult is recorded — instead of resurrecting
// the cleared session's memory the way the TS baseline does.
#[tokio::test]
async fn clear_during_create_note_await_discards_persist() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let root = TempDir::new("clear-create-note");
    let root_path = root.path();
    let home = root_path.join("home");
    fs::create_dir_all(&home).expect("#917 create home");
    let workspace = root_path.join("workspace");
    fs::create_dir_all(&workspace).expect("#917 create workspace");
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
        .expect("#917 open repository"),
    );
    stores
        .storage
        .set_pipeline_state(
            "workspaceDir",
            &JsValue::String(workspace.to_string_lossy().as_ref().into()),
        )
        .expect("#917 set workspaceDir");

    let response = |value: serde_json::Value| {
        serde_json::json!({
            "status": 200,
            "body": serde_json::to_string(&serde_json::json!({
                "choices": [{
                    "message": {
                        "content": serde_json::to_string(&value)
                            .expect("#917 scripted response content")
                    }
                }]
            })).expect("#917 scripted response envelope"),
        })
    };
    let steps = vec![
        response(serde_json::json!({
            "should_store": true,
            "labels": [{
                "type": "project",
                "reason": "durable project fact",
                "evidence": "project evidence"
            }]
        })),
        response(serde_json::json!({
            "skip": false,
            "reason": "",
            "name": "Alpha",
            "description": "Alpha description",
            "markdown": "## Summary\nAlpha body"
        })),
    ];
    let server = LlmStub::start(steps, Arc::new(AtomicU64::new(0))).await;
    let extractor = Arc::new(LlmMemoryExtractor::new(
        config(&server.origin),
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
                auto_index_interval_minutes: 30.0,
                auto_dream_interval_minutes: 60.0,
            },
            logger: None,
            clock: Some(clock),
        },
    );
    indexer
        .capture_l0_session(CaptureL0SessionInput {
            session_key: "s1".into(),
            timestamp: Some("2026-10-22T00:00:01.000Z".into()),
            messages: vec![MemoryMessage {
                msg_id: None,
                role: MemoryRole::User,
                content: "cleared during create".into(),
            }],
            source: None,
        })
        .expect("#917 capture l0 must not fail")
        .expect("#917 capture l0 must insert the first row");

    let injected: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let hook_injected = injected.clone();
    let hook_stores = stores.clone();
    let hooked = indexer.with_hook(Arc::new(move |stage, session_key| {
        assert_eq!(
            session_key, "s1",
            "#917 the clear hook must fire on the target session"
        );
        if stage == HeartbeatStage::BeforeCreateNote {
            let mut injected = hook_injected.lock().expect("#917 hook lock");
            assert_eq!(
                *injected, 0,
                "#917 the createNote clear must be injected exactly once"
            );
            *injected += 1;
            hook_stores
                .clear_session_memory_data("s1")
                .expect("#917 hook clearSession");
        }
    }));

    let stats = hooked
        .run_heartbeat(HeartbeatRunOptions {
            reason: Some("manual".into()),
            ..HeartbeatRunOptions::default()
        })
        .await
        .expect("#917 runHeartbeat must not reject when the session is cleared mid-await");
    assert_eq!(
        *injected.lock().expect("#917 hook lock"),
        1,
        "#917 the clear hook must have fired at the createNote await"
    );
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 0, 0),
        "#917 a cleared session must not count any written file"
    );
    let l0 = stores.storage.list_all_l0().expect("#917 list l0 rows");
    assert!(
        l0.is_empty(),
        "#917 the cleared session's L0 rows must stay deleted"
    );
    let memory_files = array(&tree(stores.get_file_memory_store().get_root_dir()))
        .iter()
        .map(|file| text(field(file, "relativePath")))
        .collect::<Vec<_>>();
    assert!(
        memory_files
            .iter()
            .all(|path| !path.starts_with("Project/")),
        "#917 no project memory file may be written for the cleared session, got {memory_files:?}"
    );
    let global_tree = tree(stores.get_global_user_store().get_root_dir());
    let global_files = array(&global_tree);
    assert!(
        global_files.is_empty(),
        "#917 no global user file may be written for the cleared session"
    );
    let traces = stores
        .storage
        .list_recent_index_traces(20)
        .expect("#917 list traces");
    let trace = traces
        .first()
        .expect("#917 the heartbeat must record its index trace");
    assert_eq!(
        trace.get("isNoOp"),
        Some(&JsValue::Bool(true)),
        "#917 the discarded turn must leave a no-op trace"
    );
    let requests = server.requests.lock().expect("#917 stub requests").clone();
    server.close().await;
    assert_eq!(
        requests.len(),
        2,
        "#917 classify and createNote must still run; only the persist is discarded"
    );
}

// -- #917 fix-r1: gated in-flight injection -----------------------------------
//
// Codex r1 P2: the hook above injects `clearSession` synchronously before the
// LLM request is issued, so it never proves the clear lands while the request
// is actually in flight. The direct tests below use a `"kind":"gate"` LlmStub
// step — the request is recorded first, then its response is parked until
// `release(step_index)` — wait until the stub has recorded the request, run
// the clear from the test thread, and only then release the response.

fn llm_response(value: serde_json::Value) -> Value {
    serde_json::json!({
        "status": 200,
        "body": serde_json::to_string(&serde_json::json!({
            "choices": [{
                "message": {
                    "content": serde_json::to_string(&value)
                        .expect("#917 scripted response content")
                }
            }]
        })).expect("#917 scripted response envelope"),
    })
}

fn classify_response(labels: &[(&str, &str)]) -> Value {
    llm_response(serde_json::json!({
        "should_store": !labels.is_empty(),
        "labels": labels
            .iter()
            .map(|(kind, reason)| serde_json::json!({
                "type": kind,
                "reason": reason,
                "evidence": format!("{kind} evidence"),
            }))
            .collect::<Vec<_>>(),
    }))
}

fn note_response(name: &str, description: &str, markdown: &str) -> Value {
    llm_response(serde_json::json!({
        "skip": false,
        "reason": "",
        "name": name,
        "description": description,
        "markdown": markdown,
    }))
}

fn select_response(decision: &str, project_id: &str, reason: &str) -> Value {
    llm_response(serde_json::json!({
        "decision": decision,
        "selected_project_id": project_id,
        "reason": reason,
    }))
}

/// Park this step's response until `LlmStub::release(index)`.
fn gated(step: Value) -> Value {
    let mut step = step;
    step["kind"] = serde_json::json!("gate");
    step
}

struct GatedLab {
    stores: Arc<EdgeClawFileStores>,
    db_path: PathBuf,
    server: LlmStub,
    indexer: HeartbeatIndexer,
    logger: Arc<RecordingLogger>,
    // Kept last so the temp root outlives the stores it hosts.
    _root: TempDir,
}

async fn gated_lab(
    name: &str,
    general: bool,
    seeds: &[(&str, &str, &str)],
    steps: impl FnOnce(&[String]) -> Vec<Value>,
) -> GatedLab {
    let root = TempDir::new(name);
    let root_path = root.path().to_path_buf();
    let home = root_path.join("home");
    fs::create_dir_all(&home).expect("#917 create home");
    let workspace = if general {
        home.join(".pilotdeck")
    } else {
        root_path.join("workspace")
    };
    fs::create_dir_all(&workspace).expect("#917 create workspace");
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
        .expect("#917 open repository"),
    );
    stores
        .storage
        .set_pipeline_state(
            "workspaceDir",
            &JsValue::String(workspace.to_string_lossy().as_ref().into()),
        )
        .expect("#917 set workspaceDir");
    let mut project_ids = Vec::new();
    for (project_name, description, status) in seeds {
        let mut input = JsObject::new();
        input.insert(
            "projectName".into(),
            JsValue::String((*project_name).into()),
        );
        input.insert("description".into(), JsValue::String((*description).into()));
        input.insert("status".into(), JsValue::String((*status).into()));
        let meta = stores
            .get_file_memory_store()
            .upsert_project_meta(&input)
            .expect("#917 seed project");
        project_ids.push(
            meta.get("projectId")
                .and_then(JsValue::as_string)
                .map(|value| value.to_utf8().expect("#917 project id is UTF-8"))
                .unwrap_or_default(),
        );
    }
    let server = LlmStub::start(steps(&project_ids), Arc::new(AtomicU64::new(0))).await;
    let extractor = Arc::new(LlmMemoryExtractor::new(
        config(&server.origin),
        Arc::new(HyperModelTransport::new()),
        Arc::new(()),
    ));
    let logger = Arc::new(RecordingLogger::default());
    let indexer = HeartbeatIndexer::new(
        stores.clone(),
        extractor,
        HeartbeatOptions {
            batch_size: None,
            source: Some("edgeclaw".into()),
            settings: IndexingSettings {
                reasoning_mode: "answer_first".into(),
                auto_index_interval_minutes: 30.0,
                auto_dream_interval_minutes: 60.0,
            },
            logger: Some(logger.clone()),
            clock: Some(clock),
        },
    );
    GatedLab {
        stores,
        db_path: root_path.join("control.sqlite"),
        server,
        indexer,
        logger,
        _root: root,
    }
}

fn capture_user_turn(indexer: &HeartbeatIndexer, session: &str, timestamp: &str, content: &str) {
    indexer
        .capture_l0_session(CaptureL0SessionInput {
            session_key: session.into(),
            timestamp: Some(timestamp.into()),
            messages: vec![MemoryMessage {
                msg_id: None,
                role: MemoryRole::User,
                content: content.into(),
            }],
            source: None,
        })
        .expect("#917 capture l0 must not fail")
        .expect("#917 capture l0 must insert the row");
}

async fn wait_for_requests(server: &LlmStub, count: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while server
        .requests
        .lock()
        .expect("#917 stub requests lock")
        .len()
        < count
    {
        assert!(
            std::time::Instant::now() < deadline,
            "#917 the stub must record {count} in-flight requests within 10s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

fn spawn_heartbeat(
    indexer: &HeartbeatIndexer,
) -> tokio::task::JoinHandle<Result<HeartbeatStats, HeartbeatError>> {
    let runner = indexer.clone();
    tokio::spawn(async move {
        runner
            .run_heartbeat(HeartbeatRunOptions {
                reason: Some("manual".into()),
                ..HeartbeatRunOptions::default()
            })
            .await
    })
}

fn memory_paths(stores: &EdgeClawFileStores) -> Vec<String> {
    array(&tree(stores.get_file_memory_store().get_root_dir()))
        .iter()
        .map(|file| text(field(file, "relativePath")))
        .collect()
}

fn run_options() -> HeartbeatRunOptions {
    HeartbeatRunOptions {
        reason: Some("manual".into()),
        ..HeartbeatRunOptions::default()
    }
}

// T1: a `clearSession` that lands while the single-turn createNote request is
// genuinely in flight (the stub recorded it, its response is parked) must
// still discard the turn's persist.
#[tokio::test]
async fn clear_while_create_note_request_in_flight_discards_persist() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("clear-create-note-gate", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            gated(note_response(
                "Alpha",
                "Alpha description",
                "## Summary\nAlpha body",
            )),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "cleared during in-flight create",
    );
    let heartbeat = spawn_heartbeat(&lab.indexer);
    wait_for_requests(&lab.server, 2).await;
    lab.stores
        .clear_session_memory_data("s1")
        .expect("#917 the in-flight clear must succeed");
    lab.server.release(1);
    let stats = heartbeat
        .await
        .expect("#917 the heartbeat task must not panic")
        .expect("#917 runHeartbeat must not reject when the clear lands mid-request");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 0, 0),
        "#917 a clear while the createNote request is in flight must discard the turn's persist"
    );
    assert!(
        lab.stores
            .storage
            .list_all_l0()
            .expect("#917 list l0 rows")
            .is_empty(),
        "#917 the cleared session's L0 rows must stay deleted"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().all(|path| !path.starts_with("Project/")),
        "#917 no project memory file may be written for the in-flight-cleared session, got {paths:?}"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#917 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        2,
        "#917 classify and createNote must still run; only the persist is discarded"
    );
}

// T2: the second turn's createNote request is the parked one; the clear lands
// while it is in flight. The first turn's already-committed file is deleted by
// the clear, the second turn's candidate is discarded, and no file survives.
#[tokio::test]
async fn second_turn_create_note_request_in_flight_clear_discards_persist() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("second-turn-clear-gate", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
            classify_response(&[("project", "second turn fact")]),
            gated(note_response(
                "Beta",
                "Beta description",
                "## Summary\nBeta body",
            )),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "first turn user",
    );
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:02.000Z",
        "second turn user",
    );
    let heartbeat = spawn_heartbeat(&lab.indexer);
    wait_for_requests(&lab.server, 4).await;
    lab.stores
        .clear_session_memory_data("s1")
        .expect("#917 the in-flight clear must succeed");
    lab.server.release(3);
    let stats = heartbeat
        .await
        .expect("#917 the heartbeat task must not panic")
        .expect("#917 runHeartbeat must not reject when the clear lands mid-request");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (2, 1, 0),
        "#917 the first turn may write before the clear; the second turn's in-flight clear must discard its persist"
    );
    assert!(
        lab.stores
            .storage
            .list_all_l0()
            .expect("#917 list l0 rows")
            .is_empty(),
        "#917 the cleared session's L0 rows must stay deleted"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().all(|path| !path.starts_with("Project/")),
        "#917 the clear must delete the first turn's file and discard the second turn's candidate, got {paths:?}"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#917 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        4,
        "#917 both turns' classify and createNote requests must run; only the second persist is discarded"
    );
}

// T-select: general mode, seeded project, attach route — the clear lands
// while the selectIndexProject request is in flight. The routed candidate
// must be discarded; the seeded meta has no source session and must survive.
#[tokio::test]
async fn clear_during_select_request_in_flight_discards_persist() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab(
        "select-clear-gate",
        true,
        &[("Existing Project", "existing description", "in_progress")],
        |project_ids| {
            vec![
                classify_response(&[("project", "durable project fact")]),
                note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
                gated(select_response(
                    "attach_existing",
                    &project_ids[0],
                    "attach existing",
                )),
            ]
        },
    )
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "cleared during in-flight select",
    );
    let heartbeat = spawn_heartbeat(&lab.indexer);
    wait_for_requests(&lab.server, 3).await;
    lab.stores
        .clear_session_memory_data("s1")
        .expect("#917 the in-flight clear must succeed");
    lab.server.release(2);
    let stats = heartbeat
        .await
        .expect("#917 the heartbeat task must not panic")
        .expect("#917 runHeartbeat must not reject when the clear lands mid-select");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 0, 0),
        "#917 a clear while the select request is in flight must discard the routed candidate"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().all(|path| !path.starts_with("Project/")),
        "#917 no note may be attached after an in-flight clearSession, got {paths:?}"
    );
    assert!(
        paths
            .iter()
            .any(|path| path.starts_with("GeneralProjects/")),
        "#917 the seeded project meta must survive clearSession, got {paths:?}"
    );
    assert!(
        lab.stores
            .storage
            .list_all_l0()
            .expect("#917 list l0 rows")
            .is_empty(),
        "#917 the cleared session's L0 rows must stay deleted"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#917 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        3,
        "#917 classify, createNote and select must still run; only the persist is discarded"
    );
}

// T-second-label: one turn, two labels (the extractor keeps one candidate per
// kind, so the second label must be a different kind). The first candidate
// commits before the clear; the clear lands while the second candidate's
// select request is in flight, so the per-candidate recheck (not just the
// first round's) is what must discard the second label's candidate.
#[tokio::test]
async fn second_label_select_request_in_flight_clear_discards_persist() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("second-label-clear-gate", true, &[], |_| {
        vec![
            classify_response(&[("project", "first fact"), ("feedback", "second fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
            note_response("Beta", "Beta description", "## Rule\nBeta body"),
            gated(select_response("create_new", "", "new project")),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "two labels in one turn",
    );
    let heartbeat = spawn_heartbeat(&lab.indexer);
    wait_for_requests(&lab.server, 4).await;
    lab.stores
        .clear_session_memory_data("s1")
        .expect("#917 the in-flight clear must succeed");
    lab.server.release(3);
    let stats = heartbeat
        .await
        .expect("#917 the heartbeat task must not panic")
        .expect("#917 runHeartbeat must not reject when the clear lands mid-select");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 2, 0),
        "#917 the first label may write before the clear; the second label's in-flight clear must discard its candidate"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths
            .iter()
            .all(|path| !path.to_lowercase().contains("beta")),
        "#917 the second label's candidate must be discarded after the in-flight clear, got {paths:?}"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#917 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        4,
        "#917 classify, both createNote requests and the second label's select must still run"
    );
}

// T-lock: the commit segment (in-scope recheck + meta write + upsertCandidate)
// holds the same snapshot operation locks `clearSession` takes. A cross-thread
// clear that starts while the segment is parked at BeforePersistCommit must
// block until the segment's writes complete, then delete the committed file —
// no net resurrection. Dropping the scope (the negative-control mutant) lets
// the clear run first and the write resurrect the file afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_segment_blocks_concurrent_clear_until_writes_complete() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("commit-lock", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "cleared during commit segment",
    );
    let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let parked_tx = Arc::new(Mutex::new(parked_tx));
    let release_rx = Arc::new(Mutex::new(release_rx));
    let fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook = {
        let parked_tx = parked_tx.clone();
        let release_rx = release_rx.clone();
        let fired = fired.clone();
        Arc::new(move |stage: HeartbeatStage, session_key: &str| {
            assert_eq!(
                session_key, "s1",
                "#917 the commit hook must fire on the target session"
            );
            if stage == HeartbeatStage::BeforePersistCommit {
                fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                parked_tx
                    .lock()
                    .expect("#917 commit hook channel lock")
                    .send(())
                    .expect("#917 the commit hook must signal the parked segment");
                release_rx
                    .lock()
                    .expect("#917 commit hook channel lock")
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .expect("#917 the commit hook must be released");
            }
        })
    };
    let hooked = lab.indexer.with_hook(hook);
    let handle = tokio::runtime::Handle::current();
    let heartbeat = thread::spawn(move || handle.block_on(hooked.run_heartbeat(run_options())));
    parked_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("#917 the heartbeat must park inside the commit segment");
    // Deterministic blocked/completed discrimination (the #1207 fix-r2 review
    // P2-1 shape): the workspace store's operation-lock wait observer reports
    // the clear actually parked on the commit segment's snapshot locks — not a
    // wall-clock window host load could stretch past the dropped-scope
    // mutant's clear. While the segment stays parked the clear can only
    // block, so the first probe event must be the blocking one; the
    // dropped-scope mutant lets the clear run to completion inside the
    // parked window and is killed by receiving that completion here.
    #[derive(Debug)]
    enum ClearProbe {
        BlockedOnSnapshotLocks,
        Done,
    }
    let (clear_probe_tx, clear_probe_rx) = std::sync::mpsc::channel::<ClearProbe>();
    lab.stores.workspace_memory.set_operation_wait_observer({
        let clear_probe_tx = clear_probe_tx.clone();
        Arc::new(move || {
            // Re-acquisition contention after the release is expected
            // noise; the discrimination only needs the first event.
            let _ = clear_probe_tx.send(ClearProbe::BlockedOnSnapshotLocks);
        })
    });
    let clear_stores = lab.stores.clone();
    let clear = thread::spawn(move || {
        clear_stores
            .clear_session_memory_data("s1")
            .expect("#917 the concurrent clear must succeed after the commit segment");
        clear_probe_tx
            .send(ClearProbe::Done)
            .expect("#917 the concurrent clear must signal its completion");
    });
    let first = clear_probe_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("#917 the clear probe must report blocking or completion within 10s");
    assert!(
        matches!(first, ClearProbe::BlockedOnSnapshotLocks),
        "#917 a concurrent clearSession must stay blocked on the commit segment's snapshot locks until its writes complete, got {first:?}"
    );
    release_tx
        .send(())
        .expect("#917 the release signal must reach the commit hook");
    // Post-release contention on the re-acquired locks can queue more
    // BlockedOnSnapshotLocks probes ahead of the completion; only the
    // completion itself is required here.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match clear_probe_rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        {
            Ok(ClearProbe::Done) => break,
            Ok(ClearProbe::BlockedOnSnapshotLocks) => continue,
            Err(timeout) => panic!(
                "#917 the blocked clear must complete once the commit segment released the snapshot locks, got {timeout:?}"
            ),
        }
    }
    let stats = heartbeat
        .join()
        .expect("#917 the heartbeat thread must not panic")
        .expect(
            "#917 runHeartbeat must not reject when a concurrent clear waits on the commit segment",
        );
    clear.join().expect("#917 the clear thread must not panic");
    assert_eq!(
        fired.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "#917 the commit hook must fire exactly once"
    );
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 1, 0),
        "#917 the parked commit segment must still write its candidate exactly once"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().all(|path| !path.starts_with("Project/")),
        "#917 the waiting clear must delete the committed file after the segment releases, got {paths:?}"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#917 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        2,
        "#917 classify and createNote must still run before the parked commit segment"
    );
}

// T-clear-close: the composite Codex 1 cell. `clearSession` then `close()`
// land before the createNote response arrives. The clear generation answers
// without SQLite, so the turn is still discarded (TS would resurrect it —
// registered divergence -002) and the run ends in the same closed-database
// rejection the TS close cells record.
#[tokio::test]
async fn clear_then_close_discards_candidates_and_fails_closed() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("clear-then-close", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "cleared then closed",
    );
    let hook_stores = lab.stores.clone();
    let hooked =
        lab.indexer
            .with_hook(Arc::new(move |stage: HeartbeatStage, session_key: &str| {
                assert_eq!(
                    session_key, "s1",
                    "#917 the clear-then-close hook must fire on the target session"
                );
                if stage == HeartbeatStage::BeforeCreateNote {
                    hook_stores
                        .clear_session_memory_data("s1")
                        .expect("#917 the composite clear must succeed");
                    let _ = hook_stores.close();
                }
            }));
    let error = hooked.run_heartbeat(run_options()).await.expect_err(
        "#917 clear-then-close must end in the closed-database rejection like the TS close cell",
    );
    assert!(
        error.to_string().contains("database is not open"),
        "#917 clear-then-close must reject with the honest closed-storage error, got {error}"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().all(|path| !path.starts_with("Project/")),
        "#917 the clear generation must discard the turn even though SQLite is closed, got {paths:?}"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#917 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        2,
        "#917 classify and createNote must still run before the closed-database rejection"
    );
}

// T-drop-table: the composite Codex 1 cell. A second SQLite connection drops
// `l0_sessions` before the createNote response arrives. The recheck's L0
// lookup fails with a non-Closed storage error and must fail closed: the
// session fails with the honest error and nothing is written.
#[tokio::test]
async fn non_closed_l0_read_error_fails_closed() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("drop-table", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "table dropped mid-turn",
    );
    let db_path = lab.db_path.clone();
    let hooked =
        lab.indexer
            .with_hook(Arc::new(move |stage: HeartbeatStage, session_key: &str| {
                assert_eq!(
                    session_key, "s1",
                    "#917 the drop-table hook must fire on the target session"
                );
                if stage == HeartbeatStage::BeforeCreateNote {
                    let connection = rusqlite::Connection::open(&db_path)
                        .expect("#917 the probe connection must open the repository database");
                    connection
                        .execute_batch("DROP TABLE l0_sessions")
                        .expect("#917 the probe connection must drop the L0 table");
                }
            }));
    let stats = hooked
        .run_heartbeat(run_options())
        .await
        .expect("#917 a non-Closed L0 read error must fail the session, not reject the run");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (0, 0, 1),
        "#917 the session must fail closed with the honest storage error"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().all(|path| !path.starts_with("Project/")),
        "#917 a fail-closed read error must not resurrect the turn, got {paths:?}"
    );
    let warns = lab
        .logger
        .0
        .lock()
        .expect("#917 logger lock")
        .iter()
        .filter(|(level, _)| level == "warn")
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    assert!(
        warns
            .iter()
            .any(|message| message.contains("no such table")),
        "#917 the warn must carry the honest SQLite error, got {warns:?}"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#917 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        2,
        "#917 classify and createNote must still run before the fail-closed recheck"
    );
}

// T-fail (fix-r2, review P1-1 data-loss half): `clearSession` fails midway —
// the clear hook aborts it at the pre-fix-r2 bump position, before any data
// is removed — while the turn's createNote request is in flight. A failed
// clear must not advance the generation, so the heartbeat must neither
// discard the turn nor mark its still-present L0 row indexed without
// persisting: the turn's file is written and the run succeeds. The
// bump-before-delete order silently dropped the turn here (candidates
// discarded, row marked indexed, never retried).
#[tokio::test]
async fn failed_clear_midway_keeps_the_turn_persistable() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("clear-fails-midway", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            gated(note_response(
                "Alpha",
                "Alpha description",
                "## Summary\nAlpha body",
            )),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "cleared by a failing clear",
    );
    lab.stores
        .set_clear_session_hook(Arc::new(|_stage, session| {
            assert_eq!(
                session, "s1",
                "#917 the failed-clear hook must fire on the target session"
            );
            Err(FileMemoryError::Invalid(
                "#917 injected mid-clear storage failure".into(),
            ))
        }));
    let heartbeat = spawn_heartbeat(&lab.indexer);
    wait_for_requests(&lab.server, 2).await;
    let error = lab
        .stores
        .clear_session_memory_data("s1")
        .expect_err("#917 the mid-way clear must fail with the injected storage error");
    assert!(
        error
            .to_string()
            .contains("#917 injected mid-clear storage failure"),
        "#917 the clear must fail with the injected mid-clear error, got {error}"
    );
    lab.server.release(1);
    let stats = heartbeat
        .await
        .expect("#917 the heartbeat task must not panic")
        .expect("#917 a clearSession that failed midway must not fail the heartbeat run");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 1, 0),
        "#917 a clearSession that failed midway must leave the turn persistable, not silently drop it"
    );
    let rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#917 list l0 rows after the failed clear");
    assert_eq!(
        rows.len(),
        1,
        "#917 the failed clear must leave the session's L0 row in place"
    );
    assert!(
        rows[0].indexed,
        "#917 the L0 row may be marked indexed only alongside the persist that really wrote the file"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().any(|path| path.starts_with("Project/")),
        "#917 the failed clear must not lose the turn's persist, got {paths:?}"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#917 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        2,
        "#917 classify and createNote must still run; the persist follows the failed clear"
    );
}

// T-park (fix-r2, review P1-1 race half / opus r2 baseline-race cell):
// `clearSession` parks inside its critical section at the pre-fix-r2 bump
// position (nothing removed yet) while a heartbeat run starts inside that
// window: the AfterListUnindexed signal proves the run read its clear
// baseline and listed the still-present rows while the clear holds the
// snapshot scope (it then blocks on that scope at getProjectMeta). Releasing
// the clear lets it finish (delete + bump); the heartbeat — whose baseline
// predates the completed clear — must discard the turn. With the bump before
// the deletion the same run could observe the new generation while the rows
// were still present; after the move "generation advanced" implies "rows
// gone" by construction, so this interleaving cannot resurrect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clear_parked_mid_scope_does_not_resurrect_turn_started_inside_window() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("clear-parked-mid-scope", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "cleared while parked mid-scope",
    );
    let (clear_parked_tx, clear_parked_rx) = std::sync::mpsc::channel::<()>();
    let (release_clear_tx, release_clear_rx) = std::sync::mpsc::channel::<()>();
    let (listed_tx, listed_rx) = std::sync::mpsc::channel::<()>();
    let release_clear_rx = Arc::new(Mutex::new(release_clear_rx));
    lab.stores
        .set_clear_session_hook(Arc::new(move |stage, session| {
            assert_eq!(
                session, "s1",
                "#917 the parked-clear hook must fire on the target session"
            );
            if stage != ClearSessionStage::BeforeRemoveData {
                return Ok(());
            }
            clear_parked_tx
                .send(())
                .expect("#917 the parked clear must signal its critical section");
            release_clear_rx
                .lock()
                .expect("#917 parked-clear channel lock")
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("#917 the parked clear must be released");
            Ok(())
        }));
    let clear_stores = lab.stores.clone();
    let clear = thread::spawn(move || {
        clear_stores
            .clear_session_memory_data("s1")
            .expect("#917 the parked clear must complete after its release");
    });
    clear_parked_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("#917 the clear must park inside its critical section");
    let listed_tx = Arc::new(Mutex::new(listed_tx));
    let hooked =
        lab.indexer
            .with_hook(Arc::new(move |stage: HeartbeatStage, session_key: &str| {
                assert_eq!(
                    session_key, "s1",
                    "#917 the parked-clear heartbeat hook must fire on the target session"
                );
                if stage == HeartbeatStage::AfterListUnindexed {
                    listed_tx
                        .lock()
                        .expect("#917 heartbeat hook channel lock")
                        .send(())
                        .expect(
                            "#917 the heartbeat must signal it listed the rows inside the window",
                        );
                }
            }));
    let handle = tokio::runtime::Handle::current();
    let heartbeat = thread::spawn(move || handle.block_on(hooked.run_heartbeat(run_options())));
    listed_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("#917 the heartbeat must list the rows while the clear is parked mid-scope");
    // fix-r3 (review P1-2): the parked clear must not have advanced the
    // generation while it sits inside its critical section — with the bump
    // moved back in front of the seam (the pure fix-r2 rollback) this reads 1
    // and the regression is caught here, before any release can mask it.
    assert_eq!(
        lab.stores.session_clear_generation("s1"),
        0,
        "#917 the parked clear must not advance the generation before its L0 deletion completes"
    );
    release_clear_tx
        .send(())
        .expect("#917 the release signal must reach the parked clear");
    let stats = heartbeat
        .join()
        .expect("#917 the heartbeat thread must not panic")
        .expect("#917 runHeartbeat must not reject when a parked clear completes mid-run");
    clear
        .join()
        .expect("#917 the parked-clear thread must not panic");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 0, 0),
        "#917 the turn whose baseline was read inside the parked clear's window must be discarded, not resurrected"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().all(|path| !path.starts_with("Project/")),
        "#917 no project memory file may be written for the session cleared inside the parked window, got {paths:?}"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#917 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        2,
        "#917 classify and createNote must still run before the discard"
    );
}

// T-race (fix-r3, review P0 / DeepSeek P1-1): the run parks between its
// clear-baseline read and the unindexed list (`BeforeListUnindexed`, the
// Rust-only seam — TS runs this segment synchronously, heartbeat.ts:587, so
// the interleaving cannot happen there). While parked, a `clearSession`
// completes (deleting the old turn's row and bumping the generation) and a
// brand-new turn is captured for the same session. The session-level
// generation comparison used to treat that new row as cleared — candidates
// discarded, row still marked indexed, the turn lost forever. The per-row
// recheck must keep it: it is new data captured after the clear finished,
// exactly the turn the TS baseline persists on the next run (probe
// `probe-out/post-clear-capture-ts.json`, Node v22.22.1: writtenFiles=1,
// indexed=true).
#[tokio::test]
async fn turn_captured_after_completed_clear_is_not_silently_dropped() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("post-clear-capture", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "old turn the clear removes",
    );
    let stores = lab.stores.clone();
    let capturing = lab.indexer.clone();
    let hooked =
        lab.indexer
            .with_hook(Arc::new(move |stage: HeartbeatStage, session_key: &str| {
                assert_eq!(
                    session_key, "s1",
                    "#917 the post-clear-capture hook must fire on the target session"
                );
                if stage == HeartbeatStage::BeforeListUnindexed {
                    // The baseline above this point read 0. Complete the clear
                    // (row deleted, generation bumped to 1, removed set = the old
                    // row), then capture a brand-new turn: the review P0 shape.
                    stores
                        .clear_session_memory_data("s1")
                        .expect("#917 the in-hook clearSession must complete");
                    capture_user_turn(
                        &capturing,
                        "s1",
                        "2026-10-22T00:00:02.000Z",
                        "new turn captured after the clear",
                    );
                }
            }));
    let stats = hooked
        .run_heartbeat(run_options())
        .await
        .expect("#917 the heartbeat run over the post-clear turn must succeed");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 1, 0),
        "#917 the turn captured after the completed clear must be indexed, not silently dropped"
    );
    let rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#917 list l0 rows after the post-clear run");
    assert_eq!(
        rows.len(),
        1,
        "#917 only the turn captured after the clear may keep its L0 row, got {rows:?}"
    );
    assert_eq!(
        rows[0].timestamp, "2026-10-22T00:00:02.000Z",
        "#917 the surviving L0 row must be the post-clear turn"
    );
    assert!(
        rows[0].indexed,
        "#917 the post-clear turn's row may be marked indexed only because its file was really written"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().any(|path| path.starts_with("Project/")),
        "#917 the post-clear turn's file must be written, got {paths:?}"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#917 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        2,
        "#917 classify and createNote must run for the post-clear turn"
    );
}

// T-partial (fix-r3, review P2): the clear parks at `BeforeDeleteL0` — the
// session's memory files are already removed, its L0 rows are not — then
// aborts with the injected error (the "bump after `delete_memory_entries`,
// before the L0 DELETE" half-fix position). At the park the generation must
// still be the baseline and the row still present; after the abort the clear
// has failed with the files already gone, so the generation must still be
// untouched and the turn must stay persistable — the following heartbeat
// writes its file. With the bump anywhere before the L0 DELETE the generation
// advances while the rows survive, and an in-flight heartbeat silently drops
// the turn instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clear_failing_before_l0_delete_keeps_generation_and_turn() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("clear-fails-before-l0-delete", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "cleared by a clear failing right before the L0 delete",
    );
    let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    lab.stores
        .set_clear_session_hook(Arc::new(move |stage, session| {
            assert_eq!(
                session, "s1",
                "#917 the partial-clear hook must fire on the target session"
            );
            if stage == ClearSessionStage::BeforeRemoveData {
                return Ok(());
            }
            assert_eq!(
                stage,
                ClearSessionStage::BeforeDeleteL0,
                "#917 the partial-clear hook must see the two stages in order"
            );
            parked_tx
                .send(())
                .expect("#917 the partial clear must signal it reached the L0 delete");
            release_rx
                .lock()
                .expect("#917 partial-clear channel lock")
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("#917 the partial clear must be released");
            Err(FileMemoryError::Invalid(
                "#917 injected failure before the L0 delete".into(),
            ))
        }));
    let clear_stores = lab.stores.clone();
    let clear = thread::spawn(move || {
        let error = clear_stores
            .clear_session_memory_data("s1")
            .expect_err("#917 the partial clear must fail with the injected error");
        assert!(
            error
                .to_string()
                .contains("#917 injected failure before the L0 delete"),
            "#917 the partial clear must fail with the injected error, got {error}"
        );
    });
    parked_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("#917 the clear must park before the L0 delete");
    assert_eq!(
        lab.stores.session_clear_generation("s1"),
        0,
        "#917 the clear parked before its L0 delete must not have advanced the generation"
    );
    let parked_rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#917 list l0 rows while the clear is parked before the L0 delete");
    assert_eq!(
        parked_rows.len(),
        1,
        "#917 the clear parked before its L0 delete must keep the L0 row in place"
    );
    release_tx
        .send(())
        .expect("#917 the release signal must reach the parked partial clear");
    clear
        .join()
        .expect("#917 the partial-clear thread must not panic");
    assert_eq!(
        lab.stores.session_clear_generation("s1"),
        0,
        "#917 a clear that failed right before its L0 delete must not advance the generation"
    );
    let stats = lab
        .indexer
        .run_heartbeat(run_options())
        .await
        .expect("#917 the heartbeat after the failed partial clear must succeed");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 1, 0),
        "#917 a clear that failed right before its L0 delete must leave the turn persistable, not silently drop it"
    );
    let rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#917 list l0 rows after the failed partial clear");
    assert_eq!(
        rows.len(),
        1,
        "#917 the failed partial clear must leave the session's L0 row in place"
    );
    assert!(
        rows[0].indexed,
        "#917 the L0 row may be marked indexed only alongside the persist that really wrote the file"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().any(|path| path.starts_with("Project/")),
        "#917 the failed partial clear must not lose the turn's persist, got {paths:?}"
    );
    lab.server.close().await;
}

// #1207 fix-r1 (review P1): a clear that aborts at the `L0IdsSelected` seam
// has already removed the session's memory files; for an *indexed* row the
// pre-fix state was silent loss — the row survives with indexed=1, so
// `list_unindexed_l0_by_session` never re-lists it and the file is never
// re-derived. The abort must re-arm the session's still-present rows
// (indexed back to 0), which is exactly the recovery an unindexed row already
// gets: the following heartbeat re-lists, re-classifies and regenerates the
// file.
#[tokio::test]
async fn clear_failing_after_l0_ids_selected_rearms_indexed_rows() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("clear-fails-at-l0-ids-selected", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "indexed before a clear that fails at the L0 id selection",
    );
    let stats = lab
        .indexer
        .run_heartbeat(run_options())
        .await
        .expect("#1207 the first heartbeat must succeed");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 1, 0),
        "#1207 the first heartbeat must index the turn and write its file"
    );
    let rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#1207 list l0 rows after the first heartbeat");
    assert!(
        rows[0].indexed,
        "#1207 the row must be indexed before the failing clear starts"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().any(|path| path.starts_with("Project/")),
        "#1207 the turn's file must exist before the failing clear starts, got {paths:?}"
    );
    lab.stores
        .set_clear_session_hook(Arc::new(|stage, session| {
            assert_eq!(
                session, "s1",
                "#1207 the failing-clear hook must fire on the target session"
            );
            if stage != ClearSessionStage::L0IdsSelected {
                return Ok(());
            }
            Err(FileMemoryError::Invalid(
                "#1207 injected failure after the L0 id selection".into(),
            ))
        }));
    let error = lab
        .stores
        .clear_session_memory_data("s1")
        .expect_err("#1207 the clear must fail with the injected error");
    assert!(
        error
            .to_string()
            .contains("#1207 injected failure after the L0 id selection"),
        "#1207 the clear must fail with the injected error, got {error}"
    );
    assert_eq!(
        lab.stores.session_clear_generation("s1"),
        0,
        "#1207 the failed clear must not advance the generation"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        !paths.iter().any(|path| path.starts_with("Project/")),
        "#1207 the failed clear removed the session's files; only the re-arm can bring them back, got {paths:?}"
    );
    let rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#1207 list l0 rows after the failed clear");
    assert_eq!(
        rows.len(),
        1,
        "#1207 the failed clear must leave the session's L0 row in place"
    );
    assert!(
        !rows[0].indexed,
        "#1207 the clear that failed with the files already removed must re-arm its still-present indexed row"
    );
    let pending = lab
        .stores
        .storage
        .list_unindexed_l0_by_session("s1")
        .expect("#1207 list unindexed rows after the failed clear");
    assert_eq!(
        pending.len(),
        1,
        "#1207 the re-armed row must be re-listable so the next heartbeat regenerates its file"
    );
    let stats = lab
        .indexer
        .run_heartbeat(run_options())
        .await
        .expect("#1207 the heartbeat after the re-armed clear failure must succeed");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 1, 0),
        "#1207 the heartbeat after the re-armed clear failure must re-index the row and regenerate its file"
    );
    let rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#1207 list l0 rows after the second heartbeat");
    assert!(
        rows[0].indexed,
        "#1207 the re-armed row must be indexed again alongside the regenerated file"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().any(|path| path.starts_with("Project/")),
        "#1207 the heartbeat after the re-armed clear failure must regenerate the session's file, got {paths:?}"
    );
    lab.server.close().await;
}

// #1207 fix-r2 (review P0): a clear that fails *inside* delete_memory_entries
// — here the manifest repair rewriting the read-only MEMORY.md, after the
// per-file unlinks already removed the session's memory file — has the same
// shape as the tail failure above. The pre-fix state was the permanent loss
// the fix-r1 recovery was built for: the L0 row survives indexed=1, nothing
// ever re-lists it, and the file cannot be re-derived. The abort must re-arm
// the session's still-present rows exactly like the tail's error arm.
#[cfg(unix)]
#[tokio::test]
async fn clear_failing_inside_delete_entries_rearms_indexed_rows() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("clear-fails-inside-delete-entries", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
            classify_response(&[("project", "durable project fact")]),
            note_response("Alpha", "Alpha description", "## Summary\nAlpha body"),
        ]
    })
    .await;
    capture_user_turn(
        &lab.indexer,
        "s1",
        "2026-10-22T00:00:01.000Z",
        "indexed before a clear that fails inside the entry deletion",
    );
    let stats = lab
        .indexer
        .run_heartbeat(run_options())
        .await
        .expect("#1207 the first heartbeat must succeed");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 1, 0),
        "#1207 the first heartbeat must index the turn and write its file"
    );
    let rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#1207 list l0 rows after the first heartbeat");
    assert!(
        rows[0].indexed,
        "#1207 the row must be indexed before the failing clear starts"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().any(|path| path.starts_with("Project/")),
        "#1207 the turn's file must exist before the failing clear starts, got {paths:?}"
    );
    // The real mid-deletion IO failure face: the workspace store's manifest
    // rewrite (deleteEntries unlinks each file, then repairs MEMORY.md) hits
    // a read-only manifest with the files already removed.
    let manifest = lab
        .stores
        .get_file_memory_store()
        .get_root_dir()
        .join("MEMORY.md");
    assert!(
        manifest.is_file(),
        "#1207 the workspace manifest must exist before it is made read-only, got {manifest:?}"
    );
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o444))
        .expect("#1207 make the workspace manifest read-only");
    // #1263: 0444 only stops an unprivileged writer. Under a root container
    // the mode is bypassed (CAP_DAC_OVERRIDE), the `expect_err` below would
    // fail red for environment reasons, and the re-arm contract would be
    // reported as broken when it was never exercised — so probe natively
    // first and skip with a recognizable line when the mode does not bite.
    if fs::OpenOptions::new().write(true).open(&manifest).is_ok() {
        let _ = fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644));
        stderr_skip_line(
            "#1263 skip: 0444 does not block this caller (root or CAP_DAC_OVERRIDE); \
             the mid-deletion clear-failure contract needs an unprivileged host\n",
        );
        return;
    }
    let error = lab
        .stores
        .clear_session_memory_data("s1")
        .expect_err("#1207 the clear must fail inside the entry deletion");
    assert!(
        matches!(
            &error,
            FileMemoryError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied
        ),
        "#1207 the clear must fail with the read-only manifest's permission error, got {error}"
    );
    assert_eq!(
        lab.stores.session_clear_generation("s1"),
        0,
        "#1207 the failed clear must not advance the generation"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        !paths.iter().any(|path| path.starts_with("Project/")),
        "#1207 the failed clear already removed the session's files; only the re-arm can bring them back, got {paths:?}"
    );
    let rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#1207 list l0 rows after the failed clear");
    assert_eq!(
        rows.len(),
        1,
        "#1207 the failed clear must leave the session's L0 row in place"
    );
    assert!(
        !rows[0].indexed,
        "#1207 the clear that failed inside its entry deletion must re-arm its still-present indexed row"
    );
    let pending = lab
        .stores
        .storage
        .list_unindexed_l0_by_session("s1")
        .expect("#1207 list unindexed rows after the failed clear");
    assert_eq!(
        pending.len(),
        1,
        "#1207 the re-armed row must be re-listable so the next heartbeat regenerates its file"
    );
    // The next heartbeat's own manifest repair needs the manifest writable
    // again; the failure face under test is only the clear's mid-deletion one.
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644))
        .expect("#1207 make the workspace manifest writable again");
    let stats = lab
        .indexer
        .run_heartbeat(run_options())
        .await
        .expect("#1207 the heartbeat after the re-armed clear failure must succeed");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 1, 0),
        "#1207 the heartbeat after the re-armed clear failure must re-index the row and regenerate its file"
    );
    let rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#1207 list l0 rows after the second heartbeat");
    assert!(
        rows[0].indexed,
        "#1207 the re-armed row must be indexed again alongside the regenerated file"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().any(|path| path.starts_with("Project/")),
        "#1207 the heartbeat after the re-armed clear failure must regenerate the session's file, got {paths:?}"
    );
    lab.server.close().await;
}

// #1207 fix-1: the L0 id selection and the DELETE inside `clearSession` must
// share one connection guard. A capture that starts while the clear is parked
// at the `L0IdsSelected` seam must not land inside the guarded segment: with
// the pre-fix two-guard shape the insert completed while the clear was
// parked, the DELETE then removed it without recording it, and a heartbeat
// that had already listed it would write it back (the resurrection the
// review flagged). Under one guard the insert lands only after the DELETE,
// as new data — the row survives and never reads as a recorded removal.
#[test]
fn concurrent_capture_cannot_land_between_l0_id_selection_and_delete() {
    let root = TempDir::new("clear-gap-insert");
    let home = root.path().join("home");
    fs::create_dir_all(&home).expect("#1207 create home");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).expect("#1207 create workspace");
    let stores = Arc::new(
        EdgeClawFileStores::with_workspace_clock(
            root.path().join("control.sqlite"),
            root.path().join("memory"),
            root.path().join("root").join("global"),
            &workspace,
            home.join(".pilotdeck"),
            Arc::new(|| NOW.to_string()),
        )
        .expect("#1207 open repository"),
    );
    let row = |l0_index_id: &str, timestamp: &str| L0SessionRecord {
        l0_index_id: l0_index_id.into(),
        session_key: "s1".into(),
        timestamp: timestamp.into(),
        messages: JsValue::Array(vec![]),
        source: String::new(),
        indexed: false,
        created_at: String::new(),
    };
    stores
        .storage
        .insert_l0_session(&row("s1_a_raw", "2026-10-22T00:00:01.000Z"))
        .expect("#1207 the pre-clear row must insert");
    let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    stores.set_clear_session_hook(Arc::new(move |stage, session| {
        assert_eq!(
            session, "s1",
            "#1207 the guarded-segment hook must fire on the target session"
        );
        if stage != ClearSessionStage::L0IdsSelected {
            return Ok(());
        }
        parked_tx
            .send(())
            .expect("#1207 the parked clear must signal the guarded segment");
        release_rx
            .lock()
            .expect("#1207 parked-clear channel lock")
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("#1207 the parked clear must be released");
        Ok(())
    }));
    let clear_stores = stores.clone();
    let clear = thread::spawn(move || {
        clear_stores
            .clear_session_memory_data("s1")
            .expect("#1207 the parked clear must complete after its release");
    });
    parked_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("#1207 the clear must park between the id selection and the DELETE");
    // Deterministic blocked/completed discrimination (fix-r2 review P2-1): the
    // storage's process-lock wait observer reports the capture actually
    // parked on the connection guard — not a wall-clock window that host load
    // could stretch past the mutant's insert. While the clear stays parked
    // inside the guarded segment the capture can only block, so the first
    // probe event must be the blocking one; the two-guard mutant lets the
    // insert run to completion inside the parked window and is killed by
    // receiving that completion here.
    #[derive(Debug)]
    enum CaptureProbe {
        BlockedOnStorage,
        Inserted,
    }
    let (probe_tx, probe_rx) = std::sync::mpsc::channel::<CaptureProbe>();
    stores.storage.set_process_wait_observer({
        let probe_tx = probe_tx.clone();
        Arc::new(move || {
            // Re-acquisition contention after the release is expected noise;
            // the discrimination only needs the first event.
            let _ = probe_tx.send(CaptureProbe::BlockedOnStorage);
        })
    });
    let insert_stores = stores.clone();
    let insert = thread::spawn(move || {
        insert_stores
            .storage
            .insert_l0_session(&row("s1_b_raw", "2026-10-22T00:00:02.000Z"))
            .expect("#1207 the guarded-segment insert must not fail");
        probe_tx
            .send(CaptureProbe::Inserted)
            .expect("#1207 the guarded-segment insert must signal its completion");
    });
    let first = probe_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("#1207 the capture probe must report blocking or completion within 10s");
    assert!(
        matches!(first, CaptureProbe::BlockedOnStorage),
        "#1207 the capture started while the clear is parked inside the guarded segment must stay blocked on the connection guard until the clear releases it, got {first:?}"
    );
    release_tx
        .send(())
        .expect("#1207 the release signal must reach the parked clear");
    // Post-release contention on the re-acquired guard can queue more
    // BlockedOnStorage probes ahead of the completion; only the completion
    // itself is required here.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match probe_rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(CaptureProbe::Inserted) => break,
            Ok(CaptureProbe::BlockedOnStorage) => continue,
            Err(timeout) => panic!(
                "#1207 the blocked capture must complete once the clear released the storage guard, got {timeout:?}"
            ),
        }
    }
    insert
        .join()
        .expect("#1207 the guarded-segment insert thread must not panic");
    clear
        .join()
        .expect("#1207 the parked clear thread must not panic");
    assert_eq!(
        stores.session_clear_generation("s1"),
        1,
        "#1207 the released clear must have completed exactly one bump"
    );
    assert!(
        stores.session_clear_removed_since("s1", 0, "s1_a_raw"),
        "#1207 the row that predates the clear must be recorded as removed"
    );
    assert!(
        !stores.session_clear_removed_since("s1", 0, "s1_b_raw"),
        "#1207 the capture that landed inside the guarded segment is new data and must never read as a recorded removal"
    );
    let rows = stores
        .storage
        .list_all_l0()
        .expect("#1207 list l0 rows after the guarded clear");
    assert_eq!(
        rows.len(),
        1,
        "#1207 the guarded segment must keep the row inserted inside it; only the pre-clear row may be deleted"
    );
    assert_eq!(
        rows[0].l0_index_id, "s1_b_raw",
        "#1207 the surviving row must be the one captured inside the guarded segment"
    );
}

// #1207 fix-2: `removed_since` must answer "a clear that completed after the
// baseline deleted this exact row". A row whose deleting clear predates the
// baseline must read false even while later unrelated clears keep the
// session's generation moving — the pre-fix accumulating set answered true
// there (the row was re-captured after its clear, then a non-bumping
// deletion removed the row so the later clearSession could not re-record
// it), and the heartbeat silently dropped the turn and marked its row
// indexed.
#[test]
fn stale_removed_entry_after_later_clear_keeps_since_semantics() {
    let root = TempDir::new("stale-removed-entry");
    let home = root.path().join("home");
    fs::create_dir_all(&home).expect("#1207 create home");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).expect("#1207 create workspace");
    let stores = Arc::new(
        EdgeClawFileStores::with_workspace_clock(
            root.path().join("control.sqlite"),
            root.path().join("memory"),
            root.path().join("root").join("global"),
            &workspace,
            home.join(".pilotdeck"),
            Arc::new(|| NOW.to_string()),
        )
        .expect("#1207 open repository"),
    );
    let row = |l0_index_id: &str, timestamp: &str| L0SessionRecord {
        l0_index_id: l0_index_id.into(),
        session_key: "s1".into(),
        timestamp: timestamp.into(),
        messages: JsValue::Array(vec![]),
        source: String::new(),
        indexed: false,
        created_at: String::new(),
    };
    stores
        .storage
        .insert_l0_session(&row("s1_x_raw", "2026-10-22T00:00:01.000Z"))
        .expect("#1207 the re-captured row must insert first");
    stores
        .storage
        .insert_l0_session(&row("s1_y_raw", "2026-10-22T00:00:02.000Z"))
        .expect("#1207 the sibling row must insert");
    stores
        .clear_session_memory_data("s1")
        .expect("#1207 the first clear must succeed");
    assert_eq!(
        stores.session_clear_generation("s1"),
        1,
        "#1207 the first clear must reach generation 1"
    );
    stores
        .storage
        .insert_l0_session(&row("s1_x_raw", "2026-10-22T00:00:01.000Z"))
        .expect("#1207 the re-capture of the same id must insert");
    let baseline = stores.session_clear_generation("s1");
    assert_eq!(
        baseline, 1,
        "#1207 the baseline is the generation the first clear reached"
    );
    // The non-bumping deletion path: repair removes the re-captured row
    // without touching the clear generation or the removed record.
    let repair = stores
        .storage
        .repair_l0_sessions(|_| vec![])
        .expect("#1207 the repair must succeed");
    assert_eq!(
        repair.removed, 1,
        "#1207 the repair must remove the re-captured row"
    );
    assert_eq!(
        stores.session_clear_generation("s1"),
        baseline,
        "#1207 the non-bumping repair must leave the generation at the baseline"
    );
    // The later clearSession bumps the generation but cannot re-record the
    // row: repair already took it, so its id only lives in the first clear's
    // record, which predates the baseline.
    stores
        .clear_session_memory_data("s1")
        .expect("#1207 the second clear must succeed");
    assert_eq!(
        stores.session_clear_generation("s1"),
        baseline + 1,
        "#1207 the second clear must advance the generation without re-recording the repaired row"
    );
    assert!(
        !stores.session_clear_removed_since("s1", baseline, "s1_x_raw"),
        "#1207 a row whose deleting clear predates the baseline must not read as cleared-since it"
    );
    // Positive control: a row the later clears really delete still reads
    // true — the false above comes from the baseline comparison, not from a
    // disabled check.
    stores
        .storage
        .insert_l0_session(&row("s1_y2_raw", "2026-10-22T00:00:03.000Z"))
        .expect("#1207 the control row must insert");
    stores
        .clear_session_memory_data("s1")
        .expect("#1207 the third clear must succeed");
    assert_eq!(
        stores.session_clear_generation("s1"),
        baseline + 2,
        "#1207 the third clear must advance the generation again"
    );
    assert!(
        stores.session_clear_removed_since("s1", baseline, "s1_y2_raw"),
        "#1207 a row deleted by a clear after the baseline must still read as cleared-since it"
    );
}

// #1207 fix-2 end to end: a re-captured turn whose L0 row is removed by the
// non-bumping repair path while the heartbeat's createNote request is parked,
// followed by an unrelated clearSession (generation moves, the removed record
// cannot mention the row) and a third capture of the same id, must still be
// persisted. The pre-fix accumulated set made the mismatch arm read the stale
// id as cleared-since the baseline: the candidate was discarded and the
// re-captured row was marked indexed — the turn was silently lost.
#[tokio::test]
async fn recaptured_turn_survives_unrelated_later_clear() {
    let _guard = HEARTBEAT_TEST_LOCK.lock().await;
    let lab = gated_lab("recapture-later-clear", false, &[], |_| {
        vec![
            classify_response(&[("project", "durable project fact")]),
            gated(note_response(
                "Alpha",
                "Alpha description",
                "## Summary\nAlpha body",
            )),
        ]
    })
    .await;
    let recapture = (
        "s1",
        "2026-10-22T00:00:01.000Z",
        "recaptured around a stale removed entry",
    );
    capture_user_turn(&lab.indexer, recapture.0, recapture.1, recapture.2);
    lab.stores
        .clear_session_memory_data("s1")
        .expect("#1207 the first clear must succeed");
    capture_user_turn(&lab.indexer, recapture.0, recapture.1, recapture.2);
    let heartbeat = spawn_heartbeat(&lab.indexer);
    wait_for_requests(&lab.server, 2).await;
    // Mid-flight (the createNote response is parked): remove the re-captured
    // row through the non-bumping repair path, run an unrelated clearSession
    // (its own selection finds nothing, so the stale id is not re-recorded),
    // then capture the same turn a third time.
    let repair = lab
        .stores
        .storage
        .repair_l0_sessions(|_| vec![])
        .expect("#1207 the mid-flight repair must succeed");
    assert_eq!(
        repair.removed, 1,
        "#1207 the mid-flight repair must remove the re-captured row"
    );
    lab.stores
        .clear_session_memory_data("s1")
        .expect("#1207 the mid-flight clear must succeed");
    capture_user_turn(&lab.indexer, recapture.0, recapture.1, recapture.2);
    lab.server.release(1);
    let stats = heartbeat
        .await
        .expect("#1207 the heartbeat task must not panic")
        .expect("#1207 runHeartbeat must not reject when the unrelated clear lands mid-flight");
    assert_eq!(
        (
            stats.captured_sessions,
            stats.written_files,
            stats.failed_sessions
        ),
        (1, 1, 0),
        "#1207 the re-captured turn must be persisted, not silently dropped behind the stale removed entry"
    );
    let paths = memory_paths(&lab.stores);
    assert!(
        paths.iter().any(|path| path.starts_with("Project/")),
        "#1207 a project memory file must be written for the re-captured turn, got {paths:?}"
    );
    let rows = lab
        .stores
        .storage
        .list_all_l0()
        .expect("#1207 list l0 rows after the run");
    assert_eq!(
        rows.len(),
        1,
        "#1207 exactly the third capture's row may remain"
    );
    assert_eq!(
        rows[0].session_key, "s1",
        "#1207 the surviving row belongs to the target session"
    );
    assert!(
        rows[0].indexed,
        "#1207 the re-captured row may be marked indexed only alongside the persist that really wrote the file"
    );
    let requests = lab
        .server
        .requests
        .lock()
        .expect("#1207 stub requests")
        .clone();
    lab.server.close().await;
    assert_eq!(
        requests.len(),
        2,
        "#1207 classify and createNote must still run; only the persist decision differs"
    );
}
