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
    heartbeat::indexer::HeartbeatError,
    llm::index::IndexError,
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

/// Process cwd is global; the space-path test changes it, and the two Node
/// verifiers spawn children that inherit it. Serialize those three tests.
static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn cwd_guard() -> std::sync::MutexGuard<'static, ()> {
    CWD_LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

/// Enter a scratch cwd and restore it (before the scratch directory drops)
/// even if the test panics.
struct CwdGuard {
    previous: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl CwdGuard {
    fn enter(path: &Path) -> Self {
        let lock = cwd_guard();
        let previous = std::env::current_dir().expect("#775 previous cwd");
        std::env::set_current_dir(path).expect("#775 enter scratch cwd");
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
    }
}

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

// The driver reads operation args/seeds from `pilotdeck_protocol::parse_json`
// rather than serde_json: `JsObject` keeps oracle key order, so a verbatim
// `retrieval` object reaches the service with the same order TS observed.
fn js_text(value: &JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| panic!("#775 expected a string, got {value:?}"))
        .to_utf8()
        .expect("#775 oracle strings are valid UTF-8")
}

fn js_text_at(value: &JsValue, key: &str) -> String {
    js_text(
        value
            .get(key)
            .unwrap_or_else(|| panic!("#775 missing field {key}")),
    )
}

fn js_array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("#775 expected an array, got {other:?}"),
    }
}

fn js_u64(value: &JsValue) -> Option<u64> {
    match value {
        JsValue::Number(number)
            if *number >= 0.0 && number.fract() == 0.0 && *number <= u64::MAX as f64 =>
        {
            Some(*number as u64)
        }
        _ => None,
    }
}

fn js_limit(args: &JsValue) -> Option<usize> {
    args.get("limit")
        .and_then(js_u64)
        .map(|value| value as usize)
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

fn decode_js(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(decode_js).collect()),
        JsValue::Object(entries) => {
            if let Some(kind) = entries.get("$type").and_then(JsValue::as_string) {
                let kind = kind.to_utf8().expect("#775 encoded $type is UTF-8");
                match kind.as_str() {
                    "undefined" => return JsValue::Undefined,
                    "number" => {
                        let encoded = entries
                            .get("value")
                            .and_then(JsValue::as_string)
                            .unwrap_or_else(|| panic!("#775 encoded number without a value"))
                            .to_utf8()
                            .expect("#775 encoded number is UTF-8");
                        let number = match encoded.as_str() {
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
            // `JsObject::entries` is insertion order from `parse_json`; never
            // rebuild through a sorted map here (that was the fix-r4 P1).
            for (key, value) in entries.entries() {
                object.insert(key.clone(), decode_js(value));
            }
            JsValue::Object(object)
        }
        other => other.clone(),
    }
}

fn clean_string(value: &str, root: &Path, expected_workspace_hash: &str) -> String {
    use std::sync::OnceLock;
    static HASH: OnceLock<regex::Regex> = OnceLock::new();
    let root = root.to_string_lossy();
    let replaced = value.replace(root.as_ref(), "$ROOT");
    let pattern = HASH
        .get_or_init(|| regex::Regex::new(r"/workspaces/([0-9a-f]{10})(/|$)").expect("#775 regex"));
    let mut cleaned = String::new();
    let mut last = 0;
    for captures in pattern.captures_iter(&replaced) {
        let whole = captures.get(0).expect("#775 whole workspace hash match");
        let hash = captures
            .get(1)
            .expect("#775 workspace hash capture")
            .as_str();
        let slash = captures
            .get(2)
            .expect("#775 workspace hash suffix")
            .as_str();
        // The old cleaner replaced any 10-hex slug, so a service that seeded
        // the workspace data dir from the wrong input still compared equal.
        // Bind the observed slug to sha1(resolve(workspaceDir))[:10].
        assert_eq!(
            hash, expected_workspace_hash,
            "#775 workspace data-dir slug {hash} is not sha1(resolve(workspaceDir))={expected_workspace_hash}"
        );
        cleaned.push_str(&replaced[last..whole.start()]);
        cleaned.push_str("/workspaces/$HASH");
        cleaned.push_str(slash);
        last = whole.end();
    }
    cleaned.push_str(&replaced[last..]);
    cleaned
}

fn clean(value: Value, root: &Path, expected_workspace_hash: &str) -> Value {
    match value {
        Value::String(value) => Value::String(clean_string(&value, root, expected_workspace_hash)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| clean(item, root, expected_workspace_hash))
                .collect(),
        ),
        Value::Object(entries) => Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key, clean(value, root, expected_workspace_hash)))
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
    workspace_hash: &'a str,
    last_export: std::cell::RefCell<Option<JsValue>>,
}

fn step(
    run: &Run<'_>,
    op: &str,
    args: &JsValue,
    result: std::result::Result<JsValue, ServiceError>,
) -> Value {
    let mut row = Map::new();
    row.insert("op".into(), Value::String(op.into()));
    row.insert(
        "args".into(),
        clean(encode_js(&decode_js(args)), run.root, run.workspace_hash),
    );
    match result {
        Ok(value) => {
            row.insert("ok".into(), Value::Bool(true));
            row.insert(
                "value".into(),
                clean(encode_js(&value), run.root, run.workspace_hash),
            );
        }
        Err(error) => {
            row.insert("ok".into(), Value::Bool(false));
            row.insert(
                "error".into(),
                clean(error_value(&error), run.root, run.workspace_hash),
            );
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

fn run_ops(scenario: &Scenario<'_>, ops: &[JsValue]) -> Vec<Value> {
    let service = &scenario.service;
    let run = &scenario.run;
    let mut steps = Vec::new();
    for op in ops {
        let name = js_text_at(op, "op");
        let args = op.get("args").cloned().unwrap_or(JsValue::Undefined);
        let value = match name.as_str() {
            "getSettings" => step(run, &name, &args, service.get_settings().map(|s| s.to_js())),
            "saveSettings" => step(
                run,
                &name,
                &args,
                service
                    .save_settings(&decode_js(
                        args.get("partial").expect("#775 saveSettings partial"),
                    ))
                    .map(|s| s.to_js()),
            ),
            "overview" => step(run, &name, &args, service.overview()),
            "snapshot" => {
                let limit = args.get("limit").map(decode_js);
                step(run, &name, &args, service.snapshot(limit.as_ref()))
            }
            "captureTurn" => {
                let messages = js_array(args.get("messages").expect("#775 capture messages"))
                    .iter()
                    .map(decode_js)
                    .collect::<Vec<_>>();
                let decoded_input = decode_js(args.get("input").expect("#775 capture input"));
                let input = decoded_input
                    .as_object()
                    .expect("#775 capture input object");
                let session_key =
                    js_text_at(args.get("input").expect("#775 capture input"), "sessionKey");
                let timestamp = optional_string(input.get("timestamp"));
                let source = optional_string(input.get("source"));
                step(
                    run,
                    &name,
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
                step(run, &name, &args, service.list(&options))
            }
            "get" => {
                let ids = decode_js(args.get("ids").expect("#775 get ids"));
                // Keep `null`, `undefined` and numbers distinct: TS only
                // defaults an omitted/undefined second argument. `maxLines`
                // may arrive as a `$type` envelope (`-0`), so decode it first.
                let max_lines = args.get("maxLines").map(decode_js);
                step(run, &name, &args, service.get(&ids, max_lines.as_ref()))
            }
            "getUserSummary" => step(run, &name, &args, service.get_user_summary()),
            "getProjectMeta" => step(run, &name, &args, service.get_project_meta()),
            "getWorkspaceMode" => step(
                run,
                &name,
                &args,
                Ok(JsValue::String(service.get_workspace_mode().into())),
            ),
            "listReadableProjectCatalog" => {
                step(run, &name, &args, service.list_readable_project_catalog())
            }
            "getReadableProject" => {
                let id = js_text_at(&args, "id");
                step(run, &name, &args, service.get_readable_project(&id))
            }
            "listReadableProjectEntries" => {
                let options = options_object(args.get("options"));
                let id = js_text_at(&args, "id");
                step(
                    run,
                    &name,
                    &args,
                    service.list_readable_project_entries(&id, &options),
                )
            }
            "updateProjectMeta" => {
                let decoded = decode_js(args.get("input").expect("#775 update input"));
                let input = decoded
                    .as_object()
                    .expect("#775 update input object")
                    .clone();
                step(run, &name, &args, service.update_project_meta(&input))
            }
            "getSnapshotVersion" => step(
                run,
                &name,
                &args,
                service
                    .get_snapshot_version()
                    .map(|value| JsValue::String(value.into())),
            ),
            "saveCaseTrace" => {
                let decoded = decode_js(args.get("record").expect("#775 case trace record"));
                let record = decoded
                    .as_object()
                    .expect("#775 case trace record object")
                    .clone();
                step(
                    run,
                    &name,
                    &args,
                    service.save_case_trace(&record).map(|_| JsValue::Undefined),
                )
            }
            "listCaseTraces" => {
                let limit = js_limit(&args);
                step(
                    run,
                    &name,
                    &args,
                    service.list_case_traces(limit).map(JsValue::Array),
                )
            }
            "getCaseTrace" => {
                let case_id = js_text_at(&args, "caseId");
                step(
                    run,
                    &name,
                    &args,
                    service
                        .get_case_trace(&case_id)
                        .map(|value| value.unwrap_or(JsValue::Undefined)),
                )
            }
            "listIndexTraces" => {
                let limit = js_limit(&args);
                step(
                    run,
                    &name,
                    &args,
                    service.list_index_traces(limit).map(JsValue::Array),
                )
            }
            "getIndexTrace" => {
                let id = js_text_at(&args, "id");
                step(
                    run,
                    &name,
                    &args,
                    service
                        .get_index_trace(&id)
                        .map(|value| value.unwrap_or(JsValue::Undefined)),
                )
            }
            "listDreamTraces" => {
                let limit = js_limit(&args);
                step(
                    run,
                    &name,
                    &args,
                    service.list_dream_traces(limit).map(JsValue::Array),
                )
            }
            "getDreamTrace" => {
                let id = js_text_at(&args, "id");
                step(
                    run,
                    &name,
                    &args,
                    service
                        .get_dream_trace(&id)
                        .map(|value| value.unwrap_or(JsValue::Undefined)),
                )
            }
            "export" => {
                let result = service.export_bundle();
                if let Ok(value) = &result {
                    *run.last_export.borrow_mut() = Some(value.clone());
                }
                step(run, &name, &args, result)
            }
            "importLastExport" => {
                let bundle = run
                    .last_export
                    .borrow()
                    .clone()
                    .expect("#775 importLastExport needs a prior export");
                step(run, &name, &args, service.import_bundle(&bundle))
            }
            "clear" => {
                let scope = match args.get("scope").and_then(JsValue::as_string) {
                    Some(scope) if scope.equals_str("all_memory") => ClearMemoryScope::AllMemory,
                    _ => ClearMemoryScope::CurrentProject,
                };
                step(run, &name, &args, service.clear(scope))
            }
            "clearSession" => {
                let session_key = js_text_at(&args, "sessionKey");
                step(run, &name, &args, service.clear_session(&session_key))
            }
            "act" => {
                let decoded = decode_js(args.get("input").expect("#775 act input"));
                let input = decoded.as_object().expect("#775 act input object").clone();
                step(run, &name, &args, service.act(&input))
            }
            "close" => step(
                run,
                &name,
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

fn options_object(value: Option<&JsValue>) -> JsObject {
    match value {
        None | Some(JsValue::Null) | Some(JsValue::Undefined) => JsObject::new(),
        Some(value @ JsValue::Object(entries)) if entries.get("$type").is_none() => {
            decode_js(value).as_object().cloned().unwrap_or_default()
        }
        Some(_) => JsObject::new(),
    }
}

fn seed(service: &EdgeClawMemoryService, item: &JsValue) {
    let method = js_text_at(item, "method");
    let input = decode_js(
        item.get("input")
            .unwrap_or_else(|| panic!("#775 seed {method} needs an input")),
    );
    match method.as_str() {
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
            let store = if item
                .get("store")
                .and_then(JsValue::as_string)
                .is_some_and(|store| store.equals_str("global"))
            {
                service.repository.get_global_user_store()
            } else {
                service.repository.get_file_memory_store()
            };
            store
                .execute(&method, input)
                .unwrap_or_else(|error| panic!("#775 seed {method}: {error}"));
        }
    }
}

fn run_scenario(
    spec: &Value,
    spec_js: &JsValue,
    scratch_root: &Path,
    tokens: &[String],
    token_index: &Arc<AtomicUsize>,
) -> Value {
    let name = text_at(spec, "name");
    let root = scratch_root.join(name);
    fs::create_dir_all(&root).expect("#775 scenario root");
    fs::create_dir_all(root.join("home")).expect("#775 scenario home");
    let input_paths = spec_js
        .get("inputPaths")
        .expect("#775 scenario input paths");
    let workspace = scenario_path(&js_text_at(input_paths, "workspace"), &root, scratch_root);
    let mut options = ServiceOptions::new(&workspace);
    if let Some(root_dir) = input_paths.get("rootDir") {
        options.root_dir = Some(scenario_path(&js_text(root_dir), &root, scratch_root));
    }
    if let Some(db_path) = input_paths.get("dbPath") {
        options.db_path = Some(scenario_path(&js_text(db_path), &root, scratch_root));
    }
    if let Some(memory_dir) = input_paths.get("memoryDir") {
        options.memory_dir = Some(scenario_path(&js_text(memory_dir), &root, scratch_root));
    }
    if let Some(settings) = spec_js
        .get("options")
        .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
    {
        match settings.get("captureStrategy").and_then(JsValue::as_string) {
            Some(value) if value.equals_str("full_session") => {
                options.capture_strategy = CaptureStrategy::FullSession;
            }
            Some(value) if value.equals_str("last_turn") => {
                options.capture_strategy = CaptureStrategy::LastTurn;
            }
            None => options.capture_strategy = CaptureStrategy::LastTurn,
            Some(other) => panic!(
                "#775 unknown captureStrategy {}",
                other.to_utf8().unwrap_or_default()
            ),
        }
        options.include_assistant = settings.get("includeAssistant").and_then(JsValue::as_bool);
        options.max_message_chars = settings.get("maxMessageChars").and_then(JsValue::as_number);
        options.heartbeat_batch_size = settings
            .get("heartbeatBatchSize")
            .and_then(js_u64)
            .map(|value| value as usize);
        options.default_indexing_settings = settings.get("defaultIndexingSettings").map(decode_js);
        options.source = settings
            .get("source")
            .and_then(JsValue::as_string)
            .map(|value| value.to_utf8().expect("#775 source is UTF-8"));
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

    for item in js_array(spec_js.get("seeds").expect("#775 seeds")) {
        seed(&service, item);
    }
    let workspace_hash = pilotdeck_gateway::runtime::memory::edgeclaw::text::hash_text(
        &JsString::from(service.workspace_dir.to_string_lossy().into_owned()),
    );
    let run = Run {
        root: scratch_root,
        workspace_hash: &workspace_hash,
        last_export: std::cell::RefCell::new(None),
    };
    let scenario = Scenario { service, run };
    let steps = run_ops(&scenario, js_array(spec_js.get("ops").expect("#775 ops")));
    // Every TS scenario closes in its ops; close again defensively (and ignore
    // the expected "database is not open" if it already closed).
    let _ = scenario.service.close();

    let observed_paths = {
        let mut paths = Map::new();
        let path = |value: &Path| {
            Value::String(clean_string(
                &value.to_string_lossy(),
                scratch_root,
                scenario.run.workspace_hash,
            ))
        };
        paths.insert("workspace".into(), path(&workspace));
        match spec["inputPaths"].get("rootDir").and_then(Value::as_str) {
            Some(root_dir) => {
                paths.insert(
                    "rootDir".into(),
                    Value::String(clean_string(
                        &scenario_path(root_dir, &root, scratch_root).to_string_lossy(),
                        scratch_root,
                        scenario.run.workspace_hash,
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
        "sqlite": clean(sqlite, scratch_root, scenario.run.workspace_hash),
        "files": clean(files, scratch_root, scenario.run.workspace_hash),
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

fn observations(oracle: &Value, driver: &JsValue) -> Value {
    let scratch = Scratch::new("edgeclaw-service-facade-rust");
    let root = scratch.path();
    fs::create_dir_all(root.join("home")).expect("#775 rust home");
    let tokens = array(oracle.get("randomTokens").expect("#775 randomTokens"))
        .iter()
        .map(|value| text(value).to_owned())
        .collect::<Vec<_>>();
    let token_index = Arc::new(AtomicUsize::new(0));
    let mut scenarios = Vec::new();
    let specs = array(oracle.get("scenarios").expect("#775 scenarios"));
    let driver_specs = js_array(driver.get("scenarios").expect("#775 driver scenarios"));
    assert_eq!(
        specs.len(),
        driver_specs.len(),
        "#775 oracle and order-preserving driver scenario counts differ"
    );
    for (spec, spec_js) in specs.iter().zip(driver_specs) {
        scenarios.push(run_scenario(spec, spec_js, root, &tokens, &token_index));
    }
    json!({"scenarios": scenarios})
}

fn compare(oracle: &Value, driver: &JsValue) {
    let actual = observations(oracle, driver);
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

// -- fix-r1 regressions: P0 re-entrancy, P1 paths/settings/caseId, P2 reopen --

/// A logger that holds the service `Weak` and calls `save_settings` from the
/// duplicate-capture log callback. On the unfixed HEAD the callback re-entered
/// the indexer mutex and deadlocked; the `try_lock` probe below turns a
/// regression into a named assertion instead of a timeout.
struct ReentrantLogger {
    service: std::sync::Mutex<Option<std::sync::Weak<EdgeClawMemoryService>>>,
    calls: AtomicUsize,
    saved: std::sync::Mutex<Option<pilotdeck_gateway::runtime::memory::edgeclaw::IndexingSettings>>,
}
impl pilotdeck_gateway::runtime::memory::edgeclaw::service::ServiceLogger for ReentrantLogger {
    fn info(&self, _message: &JsString) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let service = self
            .service
            .lock()
            .expect("#775 reentrant logger weak slot")
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .expect("#775 reentrant logger service alive");
        assert!(
            service.indexer_lock_is_available(),
            "#775 logger called while the indexer mutex is held"
        );
        let mut partial = JsObject::new();
        partial.insert("autoIndexIntervalMinutes".into(), JsValue::Number(7.0));
        let saved = service
            .save_settings(&JsValue::Object(partial))
            .expect("#775 reentrant logger saveSettings");
        *self.saved.lock().expect("#775 saved settings slot") = Some(saved);
    }
}

fn service_for(
    options: ServiceOptions,
    home: &Path,
    logger: Option<Arc<dyn pilotdeck_gateway::runtime::memory::edgeclaw::service::ServiceLogger>>,
) -> EdgeClawMemoryService {
    service_for_token(options, home, logger, "token")
}

fn service_for_token(
    options: ServiceOptions,
    home: &Path,
    logger: Option<Arc<dyn pilotdeck_gateway::runtime::memory::edgeclaw::service::ServiceLogger>>,
    token: &str,
) -> EdgeClawMemoryService {
    let token = token.to_owned();
    let mut config = JsObject::new();
    config.insert("agents".into(), JsValue::Object(JsObject::new()));
    EdgeClawMemoryService::with_dependencies(
        options,
        ServiceDependencies {
            home: Some(home.to_path_buf()),
            clock: Some(Arc::new(|| NOW.to_owned())),
            now_ms: Some(Arc::new(|| NOW_MS)),
            random_token: Some(Arc::new(move || JsString::from(token.as_str()))),
            transport: None,
            runtime: None,
            logger,
            llm_config: Some(JsValue::Object(config)),
        },
    )
    .expect("#775 construct test service")
}

#[test]
fn duplicate_capture_logger_can_save_settings() {
    let scratch = Scratch::new("edgeclaw-reentrant");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).expect("#775 reentrant home");
    let logger = Arc::new(ReentrantLogger {
        service: std::sync::Mutex::new(None),
        calls: AtomicUsize::new(0),
        saved: std::sync::Mutex::new(None),
    });
    let mut options = ServiceOptions::new(scratch.path().join("workspace"));
    options.root_dir = Some(scratch.path().join("root"));
    let service = Arc::new(service_for(options, &home, Some(logger.clone())));
    *logger.service.lock().expect("#775 reentrant logger slot") = Some(Arc::downgrade(&service));
    let mut message = JsObject::new();
    message.insert("role".into(), JsValue::String("user".into()));
    message.insert("content".into(), JsValue::String("same turn".into()));
    let messages = vec![JsValue::Object(message)];
    let run = {
        let service = service.clone();
        std::thread::spawn(move || {
            for _ in 0..2 {
                service
                    .capture_turn(
                        &messages,
                        CaptureTurnInput {
                            session_key: "s1".into(),
                            timestamp: Some(NOW.into()),
                            source: None,
                        },
                    )
                    .expect("#775 reentrant capture_turn");
            }
        })
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(run.join());
    });
    match receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(_)) => panic!("#775 reentrant capture thread panicked"),
        Err(error) => panic!("#775 reentrant capture deadlocked: {error}"),
    }
    assert_eq!(
        logger.calls.load(Ordering::SeqCst),
        1,
        "#775 duplicate capture logs exactly once"
    );
    let saved = logger
        .saved
        .lock()
        .expect("#775 saved settings slot")
        .clone()
        .expect("#775 logger saved settings");
    assert_eq!(
        saved.auto_index_interval_minutes, 7.0,
        "#775 logger saveSettings value"
    );
    assert_eq!(
        service
            .get_settings()
            .expect("#775 read settings")
            .auto_index_interval_minutes,
        7.0,
        "#775 database settings after logger saveSettings"
    );
}

#[test]
fn empty_path_options_use_ts_default_rules() {
    use pilotdeck_gateway::runtime::memory::edgeclaw::text::hash_text;

    let scratch = Scratch::new("edgeclaw-empty-paths");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).expect("#775 empty-path home");
    let workspace = scratch.path().join("workspace");
    fs::create_dir_all(&workspace).expect("#775 empty-path workspace");
    let cwd = scratch.path().join("cwd");
    fs::create_dir_all(&cwd).expect("#775 empty-path cwd");
    let _cwd = CwdGuard::enter(&cwd);

    // TS `resolveDefaultRootDir`: `rootDir ? rootDir : HOME/.edgeclaw/memory`.
    let default_root = home.join(".edgeclaw").join("memory");
    let default_data = default_root
        .join("workspaces")
        .join(hash_text(&JsString::from(
            workspace.to_string_lossy().into_owned(),
        )));
    fs::create_dir_all(&default_data).expect("#775 default data dir");
    let mut options = ServiceOptions::new(&workspace);
    options.root_dir = Some(PathBuf::from(""));
    let service = service_for(options, &home, None);
    assert_eq!(
        service.data_dir, default_data,
        "#775 rootDir=\"\" must fall back to HOME/.edgeclaw/memory"
    );
    assert_eq!(
        service.db_path,
        default_data.join("control.sqlite"),
        "#775 rootDir=\"\" dbPath"
    );
    assert_eq!(
        service.memory_dir,
        default_data.join("memory"),
        "#775 rootDir=\"\" memoryDir"
    );
    service.close().expect("#775 close empty-root service");

    // TS `resolveConfiguredDataDir`: falsy `memoryDir` does not pick dataDir;
    // the final `memoryDir ?? …` still resolves the explicit empty string (cwd).
    let mut options = ServiceOptions::new(&workspace);
    options.root_dir = Some(scratch.path().join("root"));
    options.memory_dir = Some(PathBuf::from(""));
    let explicit_root = scratch.path().join("root");
    let expected_data = explicit_root
        .join("workspaces")
        .join(hash_text(&JsString::from(
            workspace.to_string_lossy().into_owned(),
        )));
    fs::create_dir_all(&expected_data).expect("#775 memory-empty data dir");
    let service = service_for(options, &home, None);
    assert_eq!(
        service.data_dir, expected_data,
        "#775 memoryDir=\"\" must keep the workspace dataDir"
    );
    assert_eq!(
        service.memory_dir,
        std::env::current_dir().expect("#775 cwd"),
        "#775 memoryDir=\"\" must resolve to cwd, not fall back"
    );
    service.close().expect("#775 close empty-memory service");
}

#[test]
fn space_path_option_is_not_trimmed() {
    use pilotdeck_gateway::runtime::memory::edgeclaw::text::hash_text;

    let scratch = Scratch::new("edgeclaw-space-path");
    let home = scratch.path().join("home");
    let cwd = scratch.path().join("cwd");
    fs::create_dir_all(&home).expect("#775 space home");
    fs::create_dir_all(&cwd).expect("#775 space cwd");
    let workspace = scratch.path().join("workspace");
    fs::create_dir_all(&workspace).expect("#775 space workspace");
    let _cwd = CwdGuard::enter(&cwd);
    // `current_dir()` canonicalizes /var → /private/var on macOS; build the
    // expected path from the same value the service's `absolute()` sees.
    let cwd = std::env::current_dir().expect("#775 canonical space cwd");
    let expected_root = cwd.join(" ");
    let expected_data = expected_root
        .join("workspaces")
        .join(hash_text(&JsString::from(
            workspace.to_string_lossy().into_owned(),
        )));
    fs::create_dir_all(&expected_data).expect("#775 space data dir");
    let mut options = ServiceOptions::new(&workspace);
    options.root_dir = Some(PathBuf::from(" "));
    let service = service_for(options, &home, None);
    let result = (service.data_dir.clone(), service.memory_dir.clone());
    drop(service);
    assert_eq!(
        result.0, expected_data,
        "#775 rootDir=\" \" must stay a real path, not fall back to the default root"
    );
    assert_eq!(
        result.1,
        expected_data.join("memory"),
        "#775 rootDir=\" \" memoryDir"
    );
}

#[test]
fn case_id_seed_keeps_lone_surrogate_started_at() {
    let scratch = Scratch::new("edgeclaw-case-id");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).expect("#775 case-id home");
    let mut options = ServiceOptions::new(scratch.path().join("workspace"));
    options.root_dir = Some(scratch.path().join("root"));
    // `(0.25).toString(36).slice(2, 10)` in the Node probe is `"9"`.
    let service = service_for_token(options, &home, None, "9");
    let mut record = JsObject::new();
    record.insert("sessionKey".into(), JsValue::String("s1".into()));
    record.insert("query".into(), JsValue::String("q1".into()));
    record.insert(
        "startedAt".into(),
        JsValue::String(JsString::from_utf16(vec![0xd800])),
    );
    record.insert("status".into(), JsValue::String("ok".into()));
    service
        .save_case_trace(&record)
        .expect("#775 save lone-surrogate case trace");
    let traces = service
        .list_case_traces(None)
        .expect("#775 list lone-surrogate case trace");
    assert_eq!(traces.len(), 1, "#775 one case trace");
    assert_eq!(
        traces[0].get("caseId").and_then(JsValue::as_string),
        Some(&JsString::from("case_trace_6297f5dafe")),
        "#775 automatic caseId seed keeps the lone-surrogate startedAt (Node v22.22.0 probe)"
    );
    service.close().expect("#775 close case-id service");
}

#[test]
fn workspace_hash_tracks_workspace_dir() {
    use pilotdeck_gateway::runtime::memory::edgeclaw::text::hash_text;

    let scratch = Scratch::new("edgeclaw-workspace-hash");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).expect("#775 hash home");
    let root = scratch.path().join("root");
    let mut services = Vec::new();
    for name in ["workspace-a", "workspace-b"] {
        let workspace = scratch.path().join(name);
        fs::create_dir_all(&workspace).expect("#775 hash workspace");
        let expected = root.join("workspaces").join(hash_text(&JsString::from(
            workspace.to_string_lossy().into_owned(),
        )));
        fs::create_dir_all(&expected).expect("#775 hash data dir");
        let mut options = ServiceOptions::new(&workspace);
        options.root_dir = Some(root.clone());
        services.push((expected, service_for(options, &home, None)));
    }
    assert_ne!(
        services[0].1.data_dir, services[1].1.data_dir,
        "#775 different workspaces under one root must get different data dirs"
    );
    for (expected, service) in &services {
        assert_eq!(
            &service.data_dir, expected,
            "#775 data dir slug must be sha1(resolve(workspaceDir))"
        );
    }
}

#[test]
fn nonfinite_settings_keep_js_number_semantics() {
    let scratch = Scratch::new("edgeclaw-nonfinite-settings");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).expect("#775 nonfinite home");
    let mut defaults = JsObject::new();
    defaults.insert("autoIndexIntervalMinutes".into(), JsValue::Number(f64::NAN));
    defaults.insert(
        "autoDreamIntervalMinutes".into(),
        JsValue::Number(f64::INFINITY),
    );
    let mut options = ServiceOptions::new(scratch.path().join("workspace"));
    options.root_dir = Some(scratch.path().join("root"));
    options.default_indexing_settings = Some(JsValue::Object(defaults));
    let service = service_for(options, &home, None);
    let before = service.get_settings().expect("#775 nonfinite getSettings");
    assert!(
        before.auto_index_interval_minutes.is_nan(),
        "#775 NaN default must survive mergeIndexingSettings"
    );
    assert_eq!(
        before.auto_dream_interval_minutes,
        f64::INFINITY,
        "#775 Infinity default must survive mergeIndexingSettings"
    );
    let mut partial = JsObject::new();
    partial.insert(
        "reasoningMode".into(),
        JsValue::String("answer_first".into()),
    );
    service
        .save_settings(&JsValue::Object(partial))
        .expect("#775 nonfinite saveSettings");
    let after = service
        .get_settings()
        .expect("#775 nonfinite getSettings after save");
    assert!(
        after.auto_index_interval_minutes.is_nan(),
        "#775 stored NaN (JSON null) falls back to the NaN default"
    );
    assert_eq!(
        after.auto_dream_interval_minutes,
        f64::INFINITY,
        "#775 stored Infinity (JSON null) falls back to the Infinity default"
    );
    service.close().expect("#775 close nonfinite service");
}

/// `mergeIndexingSettings` is TS `Math.max(0, Math.floor(value))`: every zero
/// projection is `+0`, including numeric `-0` and values that floor to `-1`
/// then clamp to `0`. Rust `f64::max` may return either signed zero.
#[test]
fn default_indexing_settings_project_negative_zero_like_ts() {
    let scratch = Scratch::new("edgeclaw-interval-negative-zero");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).expect("#775 interval-negative-zero home");
    for (index, (label, value)) in [("numeric -0", -0.0), ("numeric -0.4", -0.4)]
        .into_iter()
        .enumerate()
    {
        let workspace = scratch.path().join(format!("workspace-{index}"));
        let root = scratch.path().join(format!("root-{index}"));
        let mut defaults = JsObject::new();
        defaults.insert("autoIndexIntervalMinutes".into(), JsValue::Number(value));
        defaults.insert("autoDreamIntervalMinutes".into(), JsValue::Number(value));
        let mut options = ServiceOptions::new(workspace);
        options.root_dir = Some(root);
        options.default_indexing_settings = Some(JsValue::Object(defaults));
        let service = service_for(options, &home, None);
        let settings = service
            .get_settings()
            .unwrap_or_else(|error| panic!("#775 defaultIndexingSettings {label}: {error}"));
        assert_eq!(
            settings.auto_index_interval_minutes.to_bits(),
            0,
            "#775 defaultIndexingSettings {label} autoIndexIntervalMinutes must be +0 like TS Math.max"
        );
        assert_eq!(
            settings.auto_dream_interval_minutes.to_bits(),
            0,
            "#775 defaultIndexingSettings {label} autoDreamIntervalMinutes must be +0 like TS Math.max"
        );
        service
            .close()
            .expect("#775 close interval-negative-zero service");
    }
}

/// `saveSettings` exposes each interval through the returned `IndexingSettings`,
/// the indexer copy and the SQLite readback. TS `clampInt` ends with
/// `Math.max(min, …)`, so numeric `-0` and the strings `"-0"`/`"-0.0"` all
/// project to `+0`; Rust `f64::clamp` would keep the sign. Node v22.22.0
/// probe: `_tmp/20261026-issue-775/fix-r3/probe/negative-zero.json`.
#[test]
fn save_settings_projects_negative_zero_like_ts() {
    let scratch = Scratch::new("edgeclaw-negative-zero");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).expect("#775 negative-zero home");
    let mut options = ServiceOptions::new(scratch.path().join("workspace"));
    options.root_dir = Some(scratch.path().join("root"));
    let service = service_for(options, &home, None);
    for (label, value) in [
        ("numeric -0", JsValue::Number(-0.0)),
        ("numeric 0", JsValue::Number(0.0)),
        ("string \"-0\"", JsValue::from("-0")),
        ("string \"-0.0\"", JsValue::from("-0.0")),
    ] {
        let mut partial = JsObject::new();
        partial.insert("autoIndexIntervalMinutes".into(), value.clone());
        partial.insert("autoDreamIntervalMinutes".into(), value);
        let saved = service
            .save_settings(&JsValue::Object(partial))
            .expect("#775 negative-zero saveSettings");
        assert!(
            !saved.auto_index_interval_minutes.is_sign_negative(),
            "#775 saveSettings {label} autoIndexIntervalMinutes returned -0"
        );
        assert!(
            !saved.auto_dream_interval_minutes.is_sign_negative(),
            "#775 saveSettings {label} autoDreamIntervalMinutes returned -0"
        );
        let indexer = service.indexer_settings();
        assert!(
            !indexer.auto_index_interval_minutes.is_sign_negative()
                && !indexer.auto_dream_interval_minutes.is_sign_negative(),
            "#775 saveSettings {label} indexer kept -0"
        );
        let readback = service
            .get_settings()
            .expect("#775 negative-zero getSettings");
        assert!(
            !readback.auto_index_interval_minutes.is_sign_negative()
                && !readback.auto_dream_interval_minutes.is_sign_negative(),
            "#775 saveSettings {label} SQLite readback kept -0"
        );
        let returned = saved.to_js();
        let index = returned
            .get("autoIndexIntervalMinutes")
            .and_then(JsValue::as_number)
            .expect("#775 negative-zero returned autoIndexIntervalMinutes");
        let dream = returned
            .get("autoDreamIntervalMinutes")
            .and_then(JsValue::as_number)
            .expect("#775 negative-zero returned autoDreamIntervalMinutes");
        assert!(
            !index.is_sign_negative() && !dream.is_sign_negative(),
            "#775 saveSettings {label} returned object kept -0"
        );
    }
    service.close().expect("#775 close negative-zero service");
}

#[test]
fn reopen_restores_persisted_indexer_settings() {
    let scratch = Scratch::new("edgeclaw-reopen-settings");
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).expect("#775 reopen home");
    let root = scratch.path().join("root");
    let workspace = scratch.path().join("workspace");
    fs::create_dir_all(&workspace).expect("#775 reopen workspace");
    let mut options = ServiceOptions::new(&workspace);
    options.root_dir = Some(root);
    let service = service_for(options, &home, None);
    let mut partial = JsObject::new();
    partial.insert("autoIndexIntervalMinutes".into(), JsValue::Number(7.0));
    partial.insert("autoDreamIntervalMinutes".into(), JsValue::Number(9.0));
    service
        .save_settings(&JsValue::Object(partial))
        .expect("#775 save persisted settings");
    service.close().expect("#775 close persisted settings");
    let mut options = ServiceOptions::new(&workspace);
    options.root_dir = Some(scratch.path().join("root"));
    let reopened = service_for(options, &home, None);
    let settings = reopened.indexer_settings();
    assert_eq!(
        settings.auto_index_interval_minutes, 7.0,
        "#775 reopened indexer restores autoIndexIntervalMinutes"
    );
    assert_eq!(
        settings.auto_dream_interval_minutes, 9.0,
        "#775 reopened indexer restores autoDreamIntervalMinutes"
    );
}

#[test]
fn ts_service_facade_scenarios_match() {
    let oracle_js = baseline::oracle(ORACLE);
    assert!(
        oracle_js.get("sourceCommit").is_some(),
        "#775 oracle provenance"
    );
    let oracle: Value = serde_json::from_str(ORACLE).expect("#775 oracle is valid JSON");
    compare(&oracle, &oracle_js);
}

/// fix-r5 P2: the facade must reuse the indexer's catch-block message table.
/// `IndexError` carries raw UTF-16 units; the old hand-copied `Display` path
/// went through `utf8_lossy` and turned a lone surrogate into U+FFFD.
#[test]
fn service_error_heartbeat_message_reuses_heartbeat_message() {
    let lone = JsString::from_utf16(vec![0xD800]);
    let index = ServiceError::Heartbeat(HeartbeatError::Index(IndexError::Type(lone.clone())));
    assert_eq!(
        index.js_message(),
        lone,
        "#775 ServiceError::js_message must reuse HeartbeatError::message (lone surrogate preserved)"
    );
    let json = ServiceError::Heartbeat(HeartbeatError::Json("boom".into()));
    assert_eq!(
        json.js_message(),
        JsString::from("boom"),
        "#775 ServiceError::js_message must reuse HeartbeatError::message for Json"
    );
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
    let _guard = cwd_guard();
    let oracle: Value = serde_json::from_str(ORACLE).expect("#775 oracle is valid JSON");
    let driver = baseline::oracle(ORACLE);
    let scratch = Scratch::new("edgeclaw-service-facade-verify");
    let observed = observations(&oracle, &driver);
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
    let _guard = cwd_guard();
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
