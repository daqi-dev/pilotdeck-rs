//! Real-TS replay for `EdgeClawMemoryService` assembly and sync facade
//! (A6.6-edgeclaw-18 / M18).
//!
//! `tests/fixtures/edgeclaw-service-facade/generate.mts` ran the fixed
//! `service.ts` on Node v22.22.0 with an isolated HOME, a frozen clock, a fixed
//! `Math.random` sequence and fully explicit LLM options (no request is made).
//! The oracle records every facade return value, the SQLite pipeline/L0 rows
//! and the workspace/global file bytes. This test replays the same declarative
//! operation sequence against the Rust assembly and compares all three.
#![allow(clippy::too_many_lines)]

#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::memory::edgeclaw::{
    file_memory::FileMemoryClock,
    messages::CaptureStrategy,
    retriever::{RandomToken, RetrieverClock},
    service::{
        CaptureTurnInput, ClearMemoryScope, EdgeClawMemoryService, ServiceDependencies,
        ServiceError, ServiceOptions,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use scratch::Scratch;
use serde_json::{Map, Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const ORACLE: &str = include_str!("fixtures/edgeclaw-service-facade/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";
const NOW_MS: i64 = 1_792_627_200_000; // Date.parse("2026-10-22T00:00:00.000Z")

// -- oracle helpers --------------------------------------------------------

fn array(value: &Value) -> &[Value] {
    value
        .as_array()
        .unwrap_or_else(|| panic!("#775 expected an array, got {value}"))
}

fn text(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("#775 expected a string, got {value}"))
}

fn text_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .unwrap_or_else(|| panic!("#775 missing field {key}"))
        .as_str()
        .unwrap_or_else(|| panic!("#775 field {key} is not a string"))
}

// -- JS value <-> JSON encoding -------------------------------------------

/// The same checked encoding as `observe.mts::encode`.
fn encode_js(value: &JsValue) -> Value {
    match value {
        JsValue::Undefined => json!({"$type": "undefined"}),
        JsValue::Null => Value::Null,
        JsValue::Bool(value) => Value::Bool(*value),
        JsValue::Number(number) => {
            if number.is_nan() {
                json!({"$type": "number", "value": "NaN"})
            } else if *number == f64::INFINITY {
                json!({"$type": "number", "value": "Infinity"})
            } else if *number == f64::NEG_INFINITY {
                json!({"$type": "number", "value": "-Infinity"})
            } else if *number == 0.0 && number.is_sign_negative() {
                json!({"$type": "number", "value": "-0"})
            } else if number.fract() == 0.0
                && *number >= i64::MIN as f64
                && *number <= i64::MAX as f64
            {
                json!(*number as i64)
            } else {
                json!(number)
            }
        }
        JsValue::String(value) => Value::String(
            value
                .to_utf8()
                .expect("#775 fixture strings are valid UTF-8"),
        ),
        JsValue::Array(items) => Value::Array(items.iter().map(encode_js).collect()),
        JsValue::Object(object) => Value::Object(
            object
                .entries()
                .iter()
                .map(|(key, value)| {
                    (
                        key.to_utf8().expect("#775 fixture keys are valid UTF-8"),
                        encode_js(value),
                    )
                })
                .collect(),
        ),
    }
}

fn decode_js(value: &Value) -> JsValue {
    match value {
        Value::Array(items) => JsValue::Array(items.iter().map(decode_js).collect()),
        Value::Object(entries) => {
            if let Some(kind) = entries.get("$type").and_then(Value::as_str) {
                match kind {
                    "undefined" => return JsValue::Undefined,
                    "number" => {
                        let number = match text_at(value, "value") {
                            "NaN" => f64::NAN,
                            "Infinity" => f64::INFINITY,
                            "-Infinity" => f64::NEG_INFINITY,
                            "-0" => -0.0,
                            other => other.parse().expect("#775 encoded number"),
                        };
                        return JsValue::Number(number);
                    }
                    other => panic!("#775 unknown encoded type {other}"),
                }
            }
            let mut object = JsObject::new();
            for (key, value) in entries {
                object.insert(key.as_str().into(), decode_js(value));
            }
            JsValue::Object(object)
        }
        other => other
            .as_str()
            .map(|value| JsValue::String(value.into()))
            .or_else(|| other.as_bool().map(JsValue::Bool))
            .or_else(|| other.as_i64().map(|value| JsValue::Number(value as f64)))
            .or_else(|| other.as_f64().map(JsValue::Number))
            .unwrap_or(JsValue::Null),
    }
}

fn clean_string(value: &str, root: &Path) -> String {
    use std::sync::OnceLock;
    static HASH: OnceLock<regex::Regex> = OnceLock::new();
    let root = root.to_string_lossy();
    let replaced = value.replace(root.as_ref(), "$ROOT");
    HASH.get_or_init(|| regex::Regex::new(r"/workspaces/[0-9a-f]{10}(/|$)").expect("#775 regex"))
        .replace_all(&replaced, "/workspaces/$$HASH$1")
        .into_owned()
}

fn clean(value: Value, root: &Path) -> Value {
    match value {
        Value::String(value) => Value::String(clean_string(&value, root)),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|item| clean(item, root)).collect())
        }
        Value::Object(entries) => Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key, clean(value, root)))
                .collect(),
        ),
        other => other,
    }
}

fn error_value(error: &ServiceError) -> Value {
    json!({
        "name": error.js_name(),
        "message": error
            .js_message()
            .to_utf8()
            .expect("#775 error messages are valid UTF-8"),
    })
}

// -- scenario driver -------------------------------------------------------

/// Resolve one scenario-relative template the way `path.resolve(scenarioRoot,
/// template)` does in `observe.mts`; `$ROOT` templates stay rooted at the
/// scratch directory.
fn scenario_path(template: &str, scenario_root: &Path, scratch_root: &Path) -> PathBuf {
    let value = template.replace("$ROOT", &scratch_root.to_string_lossy());
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        scenario_root.join(path)
    }
}

struct Run<'a> {
    root: &'a Path,
    last_export: std::cell::RefCell<Option<JsValue>>,
}

fn step(
    run: &Run<'_>,
    op: &str,
    args: &Value,
    result: std::result::Result<JsValue, ServiceError>,
) -> Value {
    let mut row = Map::new();
    row.insert("op".into(), Value::String(op.into()));
    row.insert("args".into(), clean(encode_js(&decode_js(args)), run.root));
    match result {
        Ok(value) => {
            row.insert("ok".into(), Value::Bool(true));
            row.insert("value".into(), clean(encode_js(&value), run.root));
        }
        Err(error) => {
            row.insert("ok".into(), Value::Bool(false));
            row.insert("error".into(), clean(error_value(&error), run.root));
        }
    }
    Value::Object(row)
}

// The service is created per scenario and lives in a local; keep the driver
// explicit instead of hiding it behind a trait object.
struct Scenario<'a> {
    service: EdgeClawMemoryService,
    run: Run<'a>,
}

fn run_ops(scenario: &Scenario<'_>, ops: &[Value]) -> Vec<Value> {
    let service = &scenario.service;
    let run = &scenario.run;
    let mut steps = Vec::new();
    for op in ops {
        let name = text_at(op, "op");
        let args = op
            .get("args")
            .cloned()
            .unwrap_or_else(|| json!({"$type": "undefined"}));
        let value = match name {
            "getSettings" => step(run, name, &args, service.get_settings().map(|s| s.to_js())),
            "saveSettings" => step(
                run,
                name,
                &args,
                service
                    .save_settings(&decode_js(&args["partial"]))
                    .map(|s| s.to_js()),
            ),
            "overview" => step(run, name, &args, service.overview()),
            "snapshot" => {
                let limit = args.get("limit").map(decode_js);
                step(run, name, &args, service.snapshot(limit.as_ref()))
            }
            "captureTurn" => {
                let messages = array(&args["messages"])
                    .iter()
                    .map(decode_js)
                    .collect::<Vec<_>>();
                let decoded_input = decode_js(&args["input"]);
                let input = decoded_input
                    .as_object()
                    .expect("#775 capture input object");
                let session_key = text_at(&args["input"], "sessionKey").to_owned();
                let timestamp = optional_string(input.get("timestamp"));
                let source = optional_string(input.get("source"));
                step(
                    run,
                    name,
                    &args,
                    service
                        .capture_turn(
                            &messages,
                            CaptureTurnInput {
                                session_key,
                                timestamp,
                                source,
                            },
                        )
                        .map(|result| result.to_value()),
                )
            }
            "list" => {
                let options = options_object(args.get("options"));
                step(run, name, &args, service.list(&options))
            }
            "get" => {
                let ids = decode_js(&args["ids"]);
                let max_lines = args.get("maxLines").and_then(Value::as_f64);
                step(run, name, &args, service.get(&ids, max_lines))
            }
            "getUserSummary" => step(run, name, &args, service.get_user_summary()),
            "getProjectMeta" => step(run, name, &args, service.get_project_meta()),
            "getWorkspaceMode" => step(
                run,
                name,
                &args,
                Ok(JsValue::String(service.get_workspace_mode().into())),
            ),
            "listReadableProjectCatalog" => {
                step(run, name, &args, service.list_readable_project_catalog())
            }
            "getReadableProject" => step(
                run,
                name,
                &args,
                service.get_readable_project(text_at(&args, "id")),
            ),
            "listReadableProjectEntries" => {
                let options = options_object(args.get("options"));
                step(
                    run,
                    name,
                    &args,
                    service.list_readable_project_entries(text_at(&args, "id"), &options),
                )
            }
            "updateProjectMeta" => {
                let decoded = decode_js(&args["input"]);
                let input = decoded
                    .as_object()
                    .expect("#775 update input object")
                    .clone();
                step(run, name, &args, service.update_project_meta(&input))
            }
            "getSnapshotVersion" => step(
                run,
                name,
                &args,
                service
                    .get_snapshot_version()
                    .map(|value| JsValue::String(value.into())),
            ),
            "saveCaseTrace" => {
                let decoded = decode_js(&args["record"]);
                let record = decoded
                    .as_object()
                    .expect("#775 case trace record object")
                    .clone();
                step(
                    run,
                    name,
                    &args,
                    service.save_case_trace(&record).map(|_| JsValue::Undefined),
                )
            }
            "listCaseTraces" => {
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                step(
                    run,
                    name,
                    &args,
                    service.list_case_traces(limit).map(JsValue::Array),
                )
            }
            "getCaseTrace" => step(
                run,
                name,
                &args,
                service
                    .get_case_trace(text_at(&args, "caseId"))
                    .map(|value| value.unwrap_or(JsValue::Undefined)),
            ),
            "listIndexTraces" => {
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                step(
                    run,
                    name,
                    &args,
                    service.list_index_traces(limit).map(JsValue::Array),
                )
            }
            "getIndexTrace" => step(
                run,
                name,
                &args,
                service
                    .get_index_trace(text_at(&args, "id"))
                    .map(|value| value.unwrap_or(JsValue::Undefined)),
            ),
            "listDreamTraces" => {
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                step(
                    run,
                    name,
                    &args,
                    service.list_dream_traces(limit).map(JsValue::Array),
                )
            }
            "getDreamTrace" => step(
                run,
                name,
                &args,
                service
                    .get_dream_trace(text_at(&args, "id"))
                    .map(|value| value.unwrap_or(JsValue::Undefined)),
            ),
            "export" => {
                let result = service.export_bundle();
                if let Ok(value) = &result {
                    *run.last_export.borrow_mut() = Some(value.clone());
                }
                step(run, name, &args, result)
            }
            "importLastExport" => {
                let bundle = run
                    .last_export
                    .borrow()
                    .clone()
                    .expect("#775 importLastExport needs a prior export");
                step(run, name, &args, service.import_bundle(&bundle))
            }
            "clear" => {
                let scope = match args.get("scope").and_then(Value::as_str) {
                    Some("all_memory") => ClearMemoryScope::AllMemory,
                    _ => ClearMemoryScope::CurrentProject,
                };
                step(run, name, &args, service.clear(scope))
            }
            "clearSession" => step(
                run,
                name,
                &args,
                service.clear_session(text_at(&args, "sessionKey")),
            ),
            "act" => {
                let decoded = decode_js(&args["input"]);
                let input = decoded.as_object().expect("#775 act input object").clone();
                step(run, name, &args, service.act(&input))
            }
            "close" => step(
                run,
                name,
                &args,
                service.close().map(|_| JsValue::Undefined),
            ),
            other => panic!("#775 unknown op {other}"),
        };
        steps.push(value);
    }
    steps
}

fn optional_string(value: Option<&JsValue>) -> Option<String> {
    match value {
        Some(JsValue::String(value)) => Some(
            value
                .to_utf8()
                .expect("#775 fixture optional strings are valid UTF-8"),
        ),
        _ => None,
    }
}

fn options_object(value: Option<&Value>) -> JsObject {
    match value {
        None => JsObject::new(),
        Some(Value::Null) => JsObject::new(),
        Some(Value::Object(entries)) if entries.get("$type").is_none() => {
            let decoded = decode_js(value.expect("#775 options"));
            decoded.as_object().cloned().unwrap_or_default()
        }
        Some(_) => JsObject::new(),
    }
}

fn seed(service: &EdgeClawMemoryService, item: &Value) {
    let method = text_at(item, "method");
    let input = decode_js(
        item.get("input")
            .unwrap_or_else(|| panic!("#775 seed {method} needs an input")),
    );
    match method {
        "indexTrace" => service
            .repository
            .storage
            .save_index_trace(&input, 30)
            .expect("#775 seed index trace"),
        "dreamTrace" => service
            .repository
            .storage
            .save_dream_trace(&input, 30)
            .expect("#775 seed dream trace"),
        _ => {
            let input = input.as_object().expect("#775 seed input object");
            let store = if item.get("store").and_then(Value::as_str) == Some("global") {
                service.repository.get_global_user_store()
            } else {
                service.repository.get_file_memory_store()
            };
            store
                .execute(method, input)
                .unwrap_or_else(|error| panic!("#775 seed {method}: {error}"));
        }
    }
}

fn run_scenario(
    spec: &Value,
    scratch_root: &Path,
    tokens: &[String],
    token_index: &Arc<AtomicUsize>,
) -> Value {
    let name = text_at(spec, "name");
    let root = scratch_root.join(name);
    fs::create_dir_all(&root).expect("#775 scenario root");
    fs::create_dir_all(root.join("home")).expect("#775 scenario home");
    let input_paths = spec.get("inputPaths").expect("#775 scenario input paths");
    let workspace = scenario_path(text_at(input_paths, "workspace"), &root, scratch_root);
    let mut options = ServiceOptions::new(&workspace);
    if let Some(root_dir) = input_paths.get("rootDir").and_then(Value::as_str) {
        options.root_dir = Some(scenario_path(root_dir, &root, scratch_root));
    }
    if let Some(db_path) = input_paths.get("dbPath").and_then(Value::as_str) {
        options.db_path = Some(scenario_path(db_path, &root, scratch_root));
    }
    if let Some(memory_dir) = input_paths.get("memoryDir").and_then(Value::as_str) {
        options.memory_dir = Some(scenario_path(memory_dir, &root, scratch_root));
    }
    if let Some(settings) = spec.get("options") {
        match settings.get("captureStrategy").and_then(Value::as_str) {
            Some("full_session") => options.capture_strategy = CaptureStrategy::FullSession,
            Some("last_turn") | None => options.capture_strategy = CaptureStrategy::LastTurn,
            Some(other) => panic!("#775 unknown captureStrategy {other}"),
        }
        options.include_assistant = settings.get("includeAssistant").and_then(Value::as_bool);
        options.max_message_chars = settings.get("maxMessageChars").and_then(Value::as_f64);
        options.heartbeat_batch_size = settings
            .get("heartbeatBatchSize")
            .and_then(Value::as_u64)
            .map(|value| value as usize);
        options.default_indexing_settings = settings.get("defaultIndexingSettings").map(decode_js);
        options.source = settings
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }

    let tokens_arc = tokens.to_vec();
    let random_token: RandomToken = {
        let index = token_index.clone();
        Arc::new(move || {
            let at = index.fetch_add(1, Ordering::SeqCst);
            JsString::from(
                tokens_arc
                    .get(at)
                    .unwrap_or_else(|| panic!("#775 ran out of frozen random tokens at {at}"))
                    .as_str(),
            )
        })
    };
    let clock: FileMemoryClock = Arc::new(|| NOW.to_owned());
    let now_ms: RetrieverClock = Arc::new(|| NOW_MS);
    let mut config = JsObject::new();
    config.insert("agents".into(), JsValue::Object(JsObject::new()));
    let dependencies = ServiceDependencies {
        home: Some(scratch_root.join("home")),
        clock: Some(clock),
        now_ms: Some(now_ms),
        random_token: Some(random_token),
        transport: None,
        runtime: None,
        logger: None,
        llm_config: Some(JsValue::Object(config)),
    };
    let service = EdgeClawMemoryService::with_dependencies(options, dependencies)
        .unwrap_or_else(|error| panic!("#775 construct {name}: {error}"));

    for item in array(spec.get("seeds").expect("#775 seeds")) {
        seed(&service, item);
    }
    let run = Run {
        root: scratch_root,
        last_export: std::cell::RefCell::new(None),
    };
    let scenario = Scenario { service, run };
    let steps = run_ops(&scenario, array(spec.get("ops").expect("#775 ops")));
    // Every TS scenario closes in its ops; close again defensively (and ignore
    // the expected "database is not open" if it already closed).
    let _ = scenario.service.close();

    let observed_paths = {
        let mut paths = Map::new();
        let path =
            |value: &Path| Value::String(clean_string(&value.to_string_lossy(), scratch_root));
        paths.insert("workspace".into(), path(&workspace));
        match spec["inputPaths"].get("rootDir").and_then(Value::as_str) {
            Some(root_dir) => {
                paths.insert(
                    "rootDir".into(),
                    Value::String(clean_string(
                        &scenario_path(root_dir, &root, scratch_root).to_string_lossy(),
                        scratch_root,
                    )),
                );
            }
            None => {
                paths.insert("rootDir".into(), Value::Null);
            }
        }
        paths.insert("dataDir".into(), path(&scenario.service.data_dir));
        paths.insert("dbPath".into(), path(&scenario.service.db_path));
        paths.insert("memoryDir".into(), path(&scenario.service.memory_dir));
        Value::Object(paths)
    };

    let global = match spec["inputPaths"].get("rootDir").and_then(Value::as_str) {
        Some(root_dir) => scenario_path(root_dir, &root, scratch_root).join("global"),
        None => scratch_root
            .join("home")
            .join(".edgeclaw")
            .join("memory")
            .join("global"),
    };
    let sqlite = sqlite_dump(&scenario.service.db_path);
    let files = json!({
        "memory": file_tree(&scenario.service.memory_dir),
        "global": file_tree(&global),
    });

    json!({
        "name": name,
        "options": spec.get("options").cloned().unwrap_or(Value::Null),
        "seeds": spec["seeds"].clone(),
        "ops": spec["ops"].clone(),
        "inputPaths": spec["inputPaths"].clone(),
        "paths": observed_paths,
        "steps": steps,
        "sqlite": clean(sqlite, scratch_root),
        "files": clean(files, scratch_root),
    })
}

fn sqlite_dump(path: &Path) -> Value {
    let connection = rusqlite::Connection::open(path).expect("#775 open sqlite for dump");
    let pipeline = connection
        .prepare("SELECT state_key, state_json, updated_at FROM pipeline_state ORDER BY state_key")
        .expect("#775 prepare pipeline dump")
        .query_map([], |row| {
            Ok(json!({
                "state_key": row.get::<_, String>(0)?,
                "state_json": row.get::<_, String>(1)?,
                "updated_at": row.get::<_, String>(2)?,
            }))
        })
        .expect("#775 query pipeline")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("#775 collect pipeline");
    let l0 = connection
        .prepare("SELECT l0_index_id, session_key, timestamp, messages_json, source, indexed, created_at FROM l0_sessions ORDER BY l0_index_id")
        .expect("#775 prepare l0 dump")
        .query_map([], |row| {
            Ok(json!({
                "l0_index_id": row.get::<_, String>(0)?,
                "session_key": row.get::<_, String>(1)?,
                "timestamp": row.get::<_, String>(2)?,
                "messages_json": row.get::<_, String>(3)?,
                "source": row.get::<_, String>(4)?,
                "indexed": row.get::<_, i64>(5)?,
                "created_at": row.get::<_, String>(6)?,
            }))
        })
        .expect("#775 query l0")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("#775 collect l0");
    json!({"pipeline": pipeline, "l0": l0})
}

fn file_tree(root: &Path) -> Value {
    fn walk(root: &Path, base: &Path, out: &mut Vec<Value>) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        let mut entries = entries
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("#775 read fixture tree");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if path.is_file() {
                let relative = path
                    .strip_prefix(base)
                    .expect("#775 fixture path is inside the tree")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(json!({
                    "relativePath": relative,
                    "content": fs::read_to_string(&path).expect("#775 fixture file is UTF-8"),
                }));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    Value::Array(files)
}

fn observations(oracle: &Value) -> Value {
    let scratch = Scratch::new("edgeclaw-service-facade-rust");
    let root = scratch.path();
    fs::create_dir_all(root.join("home")).expect("#775 rust home");
    let tokens = array(oracle.get("randomTokens").expect("#775 randomTokens"))
        .iter()
        .map(|value| text(value).to_owned())
        .collect::<Vec<_>>();
    let token_index = Arc::new(AtomicUsize::new(0));
    let mut scenarios = Vec::new();
    for spec in array(oracle.get("scenarios").expect("#775 scenarios")) {
        scenarios.push(run_scenario(spec, root, &tokens, &token_index));
    }
    json!({"scenarios": scenarios})
}

fn compare(oracle: &Value) {
    let actual = observations(oracle);
    let expected = array(oracle.get("scenarios").expect("#775 scenarios"));
    let actual = array(actual.get("scenarios").expect("#775 rust scenarios"));
    assert_eq!(
        actual.len(),
        expected.len(),
        "#775 scenario count differs from TS"
    );
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        let name = text_at(expected, "name");
        assert_eq!(
            actual.get("paths"),
            expected.get("paths"),
            "#775 paths for scenario {index} {name}"
        );
        assert_eq!(
            actual.get("steps"),
            expected.get("steps"),
            "#775 facade steps for scenario {index} {name}"
        );
        assert_eq!(
            actual.get("sqlite"),
            expected.get("sqlite"),
            "#775 sqlite rows for scenario {index} {name}"
        );
        assert_eq!(
            actual.get("files"),
            expected.get("files"),
            "#775 file tree for scenario {index} {name}"
        );
    }
}

#[test]
fn ts_service_facade_scenarios_match() {
    let oracle_js = baseline::oracle(ORACLE);
    assert!(
        oracle_js.get("sourceCommit").is_some(),
        "#775 oracle provenance"
    );
    let oracle: Value = serde_json::from_str(ORACLE).expect("#775 oracle is valid JSON");
    compare(&oracle);
}

/// Port of `tests/context/edgeclaw-memory-session-clear.spec.ts`: wiping one
/// session must not clear another session's pending capture.
#[test]
fn session_clear_keeps_other_pending_capture() {
    let scratch = Scratch::new("edgeclaw-session-clear");
    fs::create_dir_all(scratch.path().join("home")).expect("#775 session home");
    let mut options = ServiceOptions::new(scratch.path().join("workspace"));
    options.root_dir = Some(scratch.path().join("root"));
    let mut config = JsObject::new();
    config.insert("agents".into(), JsValue::Object(JsObject::new()));
    let service = EdgeClawMemoryService::with_dependencies(
        options,
        ServiceDependencies {
            home: Some(scratch.path().join("home")),
            clock: Some(Arc::new(|| NOW.to_owned())),
            now_ms: Some(Arc::new(|| NOW_MS)),
            random_token: Some(Arc::new(|| JsString::from("token"))),
            transport: None,
            runtime: None,
            logger: None,
            llm_config: Some(JsValue::Object(config)),
        },
    )
    .expect("#775 construct session-clear service");
    for (session, content) in [
        ("session-one", "session one"),
        ("session-two", "session two"),
    ] {
        let mut message = JsObject::new();
        message.insert("role".into(), JsValue::String("user".into()));
        message.insert("content".into(), JsValue::String(content.into()));
        let messages = vec![JsValue::Object(message)];
        service
            .capture_turn(
                &messages,
                CaptureTurnInput {
                    session_key: session.into(),
                    timestamp: Some(NOW.into()),
                    source: None,
                },
            )
            .expect("#775 capture session turn");
    }
    let result = service
        .clear_session("session-one")
        .expect("#775 clear session one");
    assert_eq!(
        result.get("scope").and_then(JsValue::as_string),
        Some(&JsString::from("session")),
        "#775 clearSession scope"
    );
    let cleared = result
        .get("cleared")
        .and_then(JsValue::as_object)
        .expect("#775 clearSession cleared object");
    assert_eq!(
        cleared.get("l0Sessions").and_then(JsValue::as_number),
        Some(1.0),
        "#775 clearSession removed exactly one L0 row"
    );
    assert!(
        service
            .repository
            .storage
            .list_unindexed_l0_by_session("session-one")
            .expect("#775 read session one")
            .is_empty(),
        "#775 session-one L0 rows must be gone"
    );
    assert_eq!(
        service
            .repository
            .storage
            .list_unindexed_l0_by_session("session-two")
            .expect("#775 read session two")
            .len(),
        1,
        "#775 session-two pending capture must survive"
    );
}

#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn real_ts_reads_rust_service_observations() {
    let oracle: Value = serde_json::from_str(ORACLE).expect("#775 oracle is valid JSON");
    let scratch = Scratch::new("edgeclaw-service-facade-verify");
    let observed = observations(&oracle);
    let path = scratch.path().join("rust-observations.json");
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&observed).expect("#775 observations serialize")
        ),
    )
    .expect("#775 write rust observations");
    let baseline = std::env::var_os("PDRS_BASELINE").expect("PDRS_BASELINE");
    let mut command = baseline::node_command();
    command
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/edgeclaw-service-facade/verify.mts"),
        )
        .arg(&path)
        .env("PDRS_BASELINE", &baseline)
        .stdin(Stdio::null());
    run_node(&mut command, scratch.path());
}

#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-service-facade");
}

fn run_node(command: &mut Command, scratch: &Path) {
    let stdout = scratch.join("node.stdout");
    let stderr = scratch.join("node.stderr");
    let mut child = command
        .stdout(Stdio::from(
            fs::File::create(&stdout).expect("#775 create node stdout"),
        ))
        .stderr(Stdio::from(
            fs::File::create(&stderr).expect("#775 create node stderr"),
        ))
        .spawn()
        .expect("#775 spawn node verify");
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("#775 poll node verify") {
            break status;
        }
        if start.elapsed() > Duration::from_secs(180) {
            let _ = child.kill();
            panic!("#775 verify.mts timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        status.success(),
        "#775 verify.mts failed\nstdout:\n{}\nstderr:\n{}",
        fs::read_to_string(stdout).unwrap_or_default(),
        fs::read_to_string(stderr).unwrap_or_default()
    );
}
