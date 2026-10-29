//! Real-TS replay for `EdgeClawMemoryService` async orchestration
//! (A6.6-edgeclaw-19 / M19).
//!
//! `tests/fixtures/edgeclaw-service-orchestration/generate.mts` ran the fixed
//! `service.ts` (cc7c4e6, Node v22.22.0) with an isolated HOME, a frozen clock,
//! a fixed `Math.random` sequence and an HTTP stub whose script is consumed
//! in arrival order. The oracle records every observed value (flush / dream /
//! rollback / maintenance results and error shapes), the file trees, the
//! pipeline state and the SQLite rows. This test replays the same declarative
//! operation sequence against the Rust service over a scripted
//! `ModelHttpTransport` and compares the outputs cell for cell.
//!
//! Interleaving is deterministic on both sides: only the request at `gate`
//! (by send ordinal — the TS fixture's fetch-call counter) parks, the `during`
//! event runs while it is parked, and the parked request only reaches the stub
//! after `release`, so the arrival order — and therefore the script
//! consumption order — is fixed (`observe.mts` header).
//!
//! The only intentional difference from the TS observation path: the
//! `dream-stage-error` cell's `stageCreated` / `disposed` counters come from
//! the service's `dream_stage_counters` seam instead of a monkey-patched
//! `createDreamStage`, and `retrieve*` (`service.ts:889-913`) is out of scope
//! for this package.
#![allow(clippy::too_many_lines)]

#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{Request, Response};
use pilotdeck_gateway::runtime::{
    memory::edgeclaw::{
        llm::config::{LlmOptions, build_llm_config_with},
        service::{
            CaptureTurnInput, DreamTrigger, EdgeClawMemoryService, FlushOptions,
            ServiceDependencies, ServiceError, ServiceOptions,
        },
        text::hash_text,
    },
    model::http::{ModelHttpTransport, TransportFuture},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use scratch::Scratch;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

const ORACLE: &str = include_str!("fixtures/edgeclaw-service-orchestration/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";
const NOW_MS: i64 = 1_792_627_200_000; // Date.parse("2026-10-22T00:00:00.000Z")
/// `observe.mts::RANDOM_VALUES` and the base-36 fragments
/// `Math.random().toString(36).slice(2, 10)` produces for them
/// (`0.9₃₆` / `0.i₃₆` / `0.r₃₆`): every value has exactly one digit after
/// "0.", so the fragment is that single character.
const RANDOM_VALUES: [f64; 3] = [0.25, 0.5, 0.75];
const RANDOM_FRAGMENTS: [&str; 3] = ["9", "i", "r"];

// -- oracle helpers --------------------------------------------------------

fn text_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .unwrap_or_else(|| panic!("#776 missing field {key}"))
        .as_str()
        .unwrap_or_else(|| panic!("#776 field {key} is not a string"))
}

/// `cell.captures ?? []` / `cell.setState ?? []` in the TS driver.
fn array_or_empty(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// TS property access: `result.indexStats` on the maintenance outcome is
/// `undefined` when the key is absent (both runDueScheduledMaintenance sides
/// spread the key only when present), and `observe.mts::encode` renders that
/// explicitly. A serde `Null` (the indexing of a missing key) must not fake
/// the JS `undefined`.
fn ts_undefined_or(value: &Value, key: &str) -> Value {
    match value.get(key) {
        Some(field) if !field.is_null() => field.clone(),
        _ => json!({"$type": "undefined"}),
    }
}

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
                .expect("#776 fixture strings are valid UTF-8"),
        ),
        JsValue::Array(items) => Value::Array(items.iter().map(encode_js).collect()),
        JsValue::Object(object) => Value::Object(
            object
                .entries()
                .iter()
                .map(|(key, value)| {
                    (
                        key.to_utf8()
                            .expect("#776 fixture keys are valid UTF-8")
                            .to_string(),
                        encode_js(value),
                    )
                })
                .collect(),
        ),
    }
}

/// Declarative scenario inputs come back as `JsValue` for the store calls.
fn js_from(value: &Value) -> JsValue {
    match value {
        Value::Null => JsValue::Null,
        Value::Bool(value) => JsValue::Bool(*value),
        Value::Number(number) => JsValue::Number(number.as_f64().unwrap_or_default()),
        Value::String(value) => JsValue::String(value.as_str().into()),
        Value::Array(items) => JsValue::Array(items.iter().map(js_from).collect()),
        Value::Object(entries) => {
            let mut object = JsObject::new();
            for (key, item) in entries {
                object.insert(key.as_str().into(), js_from(item));
            }
            JsValue::Object(object)
        }
    }
}

fn js_object_from(value: &Value) -> JsObject {
    let converted = js_from(value);
    if let JsValue::Object(object) = &converted {
        object.clone()
    } else {
        panic!("#776 expected an object, got {converted:?}")
    }
}

fn js_text(value: &JsValue, key: &str) -> String {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("#776 missing string field {key}"))
        .to_utf8()
        .expect("#776 fixture strings are valid UTF-8")
}

fn error_shape(error: &ServiceError) -> Value {
    json!({
        "name": error.js_name(),
        "message": error
            .js_message()
            .to_utf8()
            .expect("#776 error messages are valid UTF-8"),
    })
}

// -- path cleaning (mirrors observe.mts::clean) ----------------------------

fn clean_string(value: &str, root: &Path, expected_workspace_hash: &str) -> String {
    use std::sync::OnceLock;
    static HASH: OnceLock<regex::Regex> = OnceLock::new();
    let root = root.to_string_lossy();
    let replaced = value.replace(root.as_ref(), "$ROOT");
    let pattern = HASH
        .get_or_init(|| regex::Regex::new(r"/workspaces/([0-9a-f]{10})(/|$)").expect("#776 regex"));
    let mut cleaned = String::new();
    let mut last = 0;
    for captures in pattern.captures_iter(&replaced) {
        let whole = captures.get(0).expect("#776 whole workspace hash match");
        let hash = captures
            .get(1)
            .expect("#776 workspace hash capture")
            .as_str();
        let slash = captures
            .get(2)
            .expect("#776 workspace hash suffix")
            .as_str();
        // Bind the observed slug to sha1(resolve(workspaceDir))[:10], like the
        // TS cleaner: a service that seeded the data dir from a different
        // input must not compare equal.
        assert_eq!(
            hash, expected_workspace_hash,
            "#776 workspace data-dir slug {hash} is not sha1(resolve(workspaceDir))={expected_workspace_hash}"
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
                .map(|(key, item)| (key, clean(item, root, expected_workspace_hash)))
                .collect(),
        ),
        other => other,
    }
}

// -- observation helpers (mirrors observe.mts) ------------------------------

fn tree(root: &Path) -> Vec<Value> {
    fn walk(root: &Path, base: &Path, out: &mut Vec<Value>) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        let mut entries = entries
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("#776 read memory tree");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if path.is_file() {
                let relative = path
                    .strip_prefix(base)
                    .expect("#776 memory tree path is inside the tree")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(json!({
                    "relativePath": relative,
                    "content": fs::read_to_string(&path).expect("#776 memory file is UTF-8"),
                }));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files
}

fn sqlite_dump(path: &Path) -> Value {
    let connection = rusqlite::Connection::open(path).expect("#776 open sqlite for dump");
    let pipeline = connection
        .prepare("SELECT state_key, state_json, updated_at FROM pipeline_state ORDER BY state_key")
        .expect("#776 prepare pipeline dump")
        .query_map([], |row| {
            Ok(json!({
                "state_key": row.get::<_, String>(0)?,
                "state_json": row.get::<_, String>(1)?,
                "updated_at": row.get::<_, String>(2)?,
            }))
        })
        .expect("#776 query pipeline")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("#776 collect pipeline");
    let l0 = connection
        .prepare("SELECT l0_index_id, session_key, timestamp, messages_json, source, indexed, created_at FROM l0_sessions ORDER BY l0_index_id")
        .expect("#776 prepare l0 dump")
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
        .expect("#776 query l0")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("#776 collect l0");
    json!({"pipeline": pipeline, "l0": l0})
}

// -- scripted transport with a single-request gate --------------------------

struct DebugLogger;
impl pilotdeck_gateway::runtime::memory::edgeclaw::service::ServiceLogger for DebugLogger {
    fn info(&self, message: &JsString) {
        eprintln!("#776 LOG info: {}", message.to_utf8().unwrap_or_default());
    }
    fn warn(&self, message: &JsString) {
        eprintln!("#776 LOG warn: {}", message.to_utf8().unwrap_or_default());
    }
    fn error(&self, message: &JsString) {
        eprintln!("#776 LOG error: {}", message.to_utf8().unwrap_or_default());
    }
}

struct TransportCore {
    steps: Mutex<VecDeque<(u16, String)>>,
    kinds: Mutex<Vec<String>>,
    ordinal: AtomicUsize,
    /// TS installs its fetch wrapper — and starts its call counter — at
    /// `ctx.gate(...)`, which for the rollback cells is only after the first
    /// dream completed; requests before that point bypass the gate.
    armed: AtomicBool,
    /// 1-based send ordinal that parks, matching the TS fetch-call counter.
    hold: Option<usize>,
    channels: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}

#[derive(Clone)]
struct ScriptedTransport {
    core: Arc<TransportCore>,
}

/// The gated request signals `seen` when it is issued and only proceeds once
/// `release` fires, so the driver's `during` event runs strictly between them.
struct Gate {
    seen: Option<oneshot::Receiver<()>>,
    release: Option<oneshot::Sender<()>>,
}

impl Gate {
    /// Bounded wait: a regression that never issues the gated request must
    /// fail here instead of hanging the suite.
    async fn wait(&mut self, cell: &str) {
        let seen = self
            .seen
            .take()
            .unwrap_or_else(|| panic!("#776 {cell}: gate already waited for"));
        tokio::time::timeout(Duration::from_secs(30), seen)
            .await
            .unwrap_or_else(|_| panic!("#776 {cell}: gated request never arrived"))
            .expect("#776 gate seen channel");
    }
    fn release(&mut self) {
        self.release
            .take()
            .expect("#776 gate released twice")
            .send(())
            .expect("#776 gate release channel");
    }
}

/// `/"content":"You are the ([^"\\]{0,60})/` from the stub server: the first
/// system prompt in the serialized request body, else `unknown`.
fn kind_from_body(body: &str) -> String {
    const NEEDLE: &str = "\"content\":\"You are the ";
    match body.find(NEEDLE) {
        Some(start) => body[start + NEEDLE.len()..]
            .chars()
            .take_while(|character| *character != '"' && *character != '\\')
            .take(60)
            .collect(),
        None => "unknown".into(),
    }
}

impl ScriptedTransport {
    /// `steps` are consumed by arrival index; a request beyond the script gets
    /// the last step, and an empty script answers 599, like the stub server.
    fn new(steps: Vec<(u16, String)>, hold: Option<usize>) -> (Self, Option<Gate>) {
        let (seen_tx, seen_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let gate = hold.map(|_| Gate {
            seen: Some(seen_rx),
            release: Some(release_tx),
        });
        (
            Self {
                core: Arc::new(TransportCore {
                    steps: Mutex::new(steps.into()),
                    kinds: Mutex::new(Vec::new()),
                    ordinal: AtomicUsize::new(0),
                    armed: AtomicBool::new(false),
                    hold,
                    channels: Mutex::new(if hold.is_some() {
                        Some((seen_tx, release_rx))
                    } else {
                        None
                    }),
                }),
            },
            gate,
        )
    }
    /// Start counting requests for the gate, discarding any earlier sends.
    fn arm(&self) {
        self.core.ordinal.store(0, Ordering::SeqCst);
        self.core.armed.store(true, Ordering::SeqCst);
    }
    fn kinds(&self) -> Value {
        json!(self.core.kinds.lock().expect("#776 arrival kinds").clone())
    }
}

impl ModelHttpTransport for ScriptedTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let core = self.core.clone();
        let ordinal = core.ordinal.fetch_add(1, Ordering::SeqCst);
        let channels = if core.armed.load(Ordering::SeqCst) && core.hold == Some(ordinal + 1) {
            core.channels.lock().expect("#776 gate channels").take()
        } else {
            None
        };
        TransportFuture::io(async move {
            if let Some((seen, release)) = channels {
                seen.send(()).expect("#776 gate seen channel");
                release.await.expect("#776 gate release channel");
            }
            // Arrival (and therefore script consumption) happens only after
            // any gate, exactly like the stub server recording a request.
            let body = request
                .into_body()
                .collect()
                .await
                .expect("#776 collect request body")
                .to_bytes();
            let text = String::from_utf8_lossy(&body).into_owned();
            let kind = kind_from_body(&text);
            let index = {
                let mut kinds = core.kinds.lock().expect("#776 arrival kinds");
                kinds.push(kind);
                kinds.len() - 1
            };
            let (status, response_body) = {
                let steps = core.steps.lock().expect("#776 transport steps");
                match steps.get(index) {
                    Some(step) => step.clone(),
                    None => steps
                        .back()
                        .cloned()
                        .unwrap_or((599, "script exhausted".into())),
                }
            };
            let body: BoxBody<Bytes, pilotdeck_gateway::runtime::model::http::BodyFailure> =
                Full::new(Bytes::from(response_body))
                    .map_err(|error| match error {})
                    .boxed();
            Ok(Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(body)
                .expect("#776 transport response"))
        })
    }
}

// -- scenario replay --------------------------------------------------------

struct Ctx {
    service: Arc<EdgeClawMemoryService>,
    transport: ScriptedTransport,
    root: PathBuf,
    workspace_hash: String,
}

impl Ctx {
    fn pipeline(&self, keys: &[&str]) -> Value {
        let mut out = serde_json::Map::new();
        for key in keys {
            let value = match self.service.repository.storage.get_pipeline_state(key) {
                Ok(Some(value)) => encode_js(&value),
                Ok(None) => Value::Null,
                Err(error) => json!({"error": error.to_string()}),
            };
            out.insert((*key).to_string(), value);
        }
        Value::Object(out)
    }
    fn l0(&self) -> Value {
        Value::Array(
            self.service
                .repository
                .storage
                .list_all_l0()
                .expect("#776 listAllL0")
                .into_iter()
                .map(|row| json!({"key": row.session_key, "indexed": row.indexed}))
                .collect(),
        )
    }
    fn files(&self) -> Value {
        Value::Array(tree(&self.service.memory_dir))
    }
    fn file_names(&self) -> Value {
        Value::Array(
            tree(&self.service.memory_dir)
                .into_iter()
                .map(|file| file["relativePath"].clone())
                .collect(),
        )
    }
    fn last_dream_tree(&self) -> Value {
        Value::Array(tree(
            &self
                .service
                .memory_dir
                .parent()
                .expect("#776 memory dir parent")
                .join("last_dream"),
        ))
    }
    /// The dream stage's workspace/global/db temps are SIBLINGS of their live
    /// targets (`.memory.dream-*` / `.control.sqlite.dream-*` next to the
    /// data dir entries, `.global.dream-*` next to the global root), not
    /// inside `memoryDir`. Scan the actual parent directories and report the
    /// leftover paths themselves, so a skipped cleanup fails on content, not
    /// on a counter.
    fn stage_temp_left(&self) -> Value {
        let global_root = self
            .service
            .repository
            .global_user_memory
            .get_root_dir()
            .to_path_buf();
        let mut parents = vec![
            self.service.data_dir.clone(),
            global_root
                .parent()
                .expect("#776 global root has a parent")
                .to_path_buf(),
        ];
        parents.sort();
        parents.dedup();
        let mut left: Vec<Value> = Vec::new();
        for parent in &parents {
            let entries = match fs::read_dir(parent) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            let mut names: Vec<String> = entries
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains(".dream-"))
                .collect();
            names.sort();
            left.extend(
                names
                    .into_iter()
                    .map(|name| json!(parent.join(name).to_string_lossy().replace('\\', "/"))),
            );
        }
        Value::Array(left)
    }
    fn names(&self) -> Vec<String> {
        let mut options = JsObject::new();
        options.insert("includeDeprecated".into(), JsValue::Bool(true));
        let entries = self.service.list(&options).expect("#776 listMemoryEntries");
        if let JsValue::Array(items) = &entries {
            items.iter().map(|entry| js_text(entry, "name")).collect()
        } else {
            panic!("#776 listMemoryEntries returned {entries:?}")
        }
    }
    fn kinds(&self) -> Value {
        self.transport.kinds()
    }
    fn clean(&self, value: Value) -> Value {
        clean(value, &self.root, &self.workspace_hash)
    }
}

fn build_service(
    dir: &Path,
    transport: Arc<dyn ModelHttpTransport>,
    math_index: &Arc<AtomicUsize>,
) -> EdgeClawMemoryService {
    // Fully explicit LLM options: `build_llm_config_with` with no openclaw
    // store and no env resolves exactly the fields below, which is what the
    // isolated-HOME TS fixture produced, and keeps the replay independent of
    // the host environment.
    let llm = LlmOptions {
        provider: Some("p".into()),
        model: Some("m".into()),
        model_ref: Some("p/m".into()),
        api_type: Some("openai-completions".into()),
        // The scripted transport never opens a socket, but the request URL must
        // still clear Node 22's fetch bad-port table before `follow` hands it
        // to the transport (port 9 is blocked and fails before `send`).
        base_url: Some("http://127.0.0.1/v1".into()),
        api_key: Some("fixture-key".into()),
        headers: None,
    };
    let config = build_llm_config_with(Some(&llm), None, &|_| None);
    let mut options = ServiceOptions::new(dir.join("workspace"));
    options.root_dir = Some(dir.join("root"));
    let math_index = math_index.clone();
    // observe.mts freezes `Math.random` behind one process-wide cycling
    // counter shared by all cells, so each cell's fragments depend on the
    // cumulative draw count of every preceding cell; `math_index` is therefore
    // owned by the whole oracle run, not one scenario.
    // The dream trace suffix reads the draw `Math.random` just made, so the
    // token seam must answer with that value's base-36 fragment.
    let last_random = Arc::new(Mutex::new(0.0_f64));
    EdgeClawMemoryService::with_dependencies(
        options,
        ServiceDependencies {
            home: Some(dir.join("home")),
            clock: Some(Arc::new(|| NOW.to_owned())),
            now_ms: Some(Arc::new(|| NOW_MS)),
            random_token: {
                let last_random = last_random.clone();
                Some(Arc::new(move || {
                    let value = *last_random.lock().expect("#776 last Math.random draw");
                    let fragment = RANDOM_VALUES
                        .iter()
                        .position(|candidate| *candidate == value)
                        .map(|index| RANDOM_FRAGMENTS[index])
                        .unwrap_or_else(|| {
                            panic!("#776 Math.random draw {value} is outside the frozen sequence")
                        });
                    JsString::from(fragment)
                }))
            },
            math_random: {
                let last_random = last_random.clone();
                Some(Arc::new(move || {
                    let value = RANDOM_VALUES
                        [math_index.fetch_add(1, Ordering::SeqCst) % RANDOM_VALUES.len()];
                    *last_random.lock().expect("#776 last Math.random draw") = value;
                    value
                }))
            },
            transport: Some(transport),
            logger: Some(Arc::new(DebugLogger)),
            llm_config: Some(config),
            ..ServiceDependencies::default()
        },
    )
    .expect("#776 construct test service")
}

async fn run_scenario(
    spec: &Value,
    root: &Path,
    index: usize,
    math_index: &Arc<AtomicUsize>,
) -> Value {
    let name = text_at(spec, "name");
    let dir = root.join(format!("svc-{index}"));
    fs::create_dir_all(dir.join("workspace")).expect("#776 scenario workspace");
    fs::create_dir_all(dir.join("home")).expect("#776 scenario home");
    let steps: Vec<(u16, String)> = spec["script"]
        .as_array()
        .expect("#776 scenario script")
        .iter()
        .map(|step| {
            (
                step["status"].as_u64().expect("#776 step status") as u16,
                step["body"].as_str().expect("#776 step body").to_string(),
            )
        })
        .collect();
    let hold = spec["gate"].as_u64().map(|gate| gate as usize);
    let (transport, mut gate) = ScriptedTransport::new(steps, hold);
    let service = Arc::new(build_service(&dir, Arc::new(transport.clone()), math_index));
    let ctx = Ctx {
        service: service.clone(),
        transport,
        root: root.to_path_buf(),
        workspace_hash: hash_text(&JsString::from(
            service.workspace_dir.to_string_lossy().into_owned(),
        )),
    };

    // -- declarative inputs, applied in the same order as observe.mts -------
    for capture in array_or_empty(&spec["captures"]) {
        let messages: Vec<JsValue> = capture["messages"]
            .as_array()
            .expect("#776 capture messages")
            .iter()
            .map(js_from)
            .collect();
        let input = &capture["input"];
        service
            .capture_turn(
                &messages,
                CaptureTurnInput {
                    session_key: text_at(input, "sessionKey").to_string(),
                    timestamp: Some(text_at(input, "timestamp").into()),
                    source: None,
                },
            )
            .expect("#776 seed captureTurn");
    }
    if !spec["settings"].is_null() {
        service
            .save_settings(&js_from(&spec["settings"]))
            .expect("#776 seed saveSettings");
    }
    for pair in array_or_empty(&spec["setState"]) {
        service
            .repository
            .storage
            .set_pipeline_state(
                pair[0].as_str().expect("#776 state key"),
                &JsValue::String(pair[1].as_str().expect("#776 state value").into()),
            )
            .expect("#776 seed setPipelineState");
    }
    let seed_round = |round: usize| -> Vec<JsValue> {
        spec["seeds"]
            .get(round)
            .and_then(Value::as_array)
            .expect("#776 seed round")
            .iter()
            .map(|input| {
                ctx.service
                    .repository
                    .get_file_memory_store()
                    .execute("upsertCandidate", &js_object_from(input))
                    .expect("#776 seed upsertCandidate")
            })
            .collect()
    };

    // -- the cell itself ----------------------------------------------------
    // The TS cells install their fetch gate before the first request except
    // the rollback pair, which arms it only after the first dream completed.
    if !matches!(
        name,
        "dream-stage-rollback-guard" | "dream-stage-rollback-ok"
    ) {
        ctx.transport.arm();
    }
    let observed: Value = match name {
        "idle-flush-empty" => {
            let stats = service
                .flush(FlushOptions::default())
                .await
                .expect("#776 idle flush");
            ctx.clean(json!({
                "stats": encode_js(&stats.to_value()),
                "pipeline": ctx.pipeline(&[
                    "autoIndexAnchorAt", "autoDreamAnchorAt", "lastIndexedAt", "lastDreamAt",
                ]),
            }))
        }
        "idle-rollback-none" => ctx.clean(match service.rollback_last_dream() {
            Ok(result) => json!({"ok": true, "result": encode_js(&result)}),
            Err(error) => json!({"ok": false, "error": error_shape(&error)}),
        }),
        "idle-maintenance-none" => {
            let outcome = service
                .run_due_scheduled_maintenance("scheduled")
                .await
                .expect("#776 idle maintenance");
            let full = encode_js(&outcome.to_value());
            ctx.clean(
                json!({"result": {"indexRan": full["indexRan"], "dreamRan": full["dreamRan"]}}),
            )
        }
        "flush-inflight-flush" => {
            let gate = gate.as_mut().expect("#776 flush-inflight-flush gate");
            let gated = service.clone();
            let first = tokio::task::spawn_local(async move {
                gated
                    .flush(FlushOptions {
                        reason: Some("manual".into()),
                        session_keys: Some(vec!["s1".into()]),
                        ..FlushOptions::default()
                    })
                    .await
            });
            gate.wait(name).await;
            let second = service
                .flush(FlushOptions {
                    reason: Some("manual".into()),
                    ..FlushOptions::default()
                })
                .await
                .expect("#776 second flush");
            gate.release();
            let stats = first
                .await
                .expect("#776 first flush join")
                .expect("#776 first flush");
            ctx.clean(json!({
                "first": encode_js(&stats.to_value()),
                "second": encode_js(&second.to_value()),
                "files": ctx.file_names(),
                "l0": ctx.l0(),
                "pipeline": ctx.pipeline(&["autoIndexAnchorAt", "lastIndexedAt"]),
                "requestKinds": ctx.kinds(),
            }))
        }
        "flush-inflight-capture" => {
            let gate = gate.as_mut().expect("#776 flush-inflight-capture gate");
            let gated = service.clone();
            let flush = tokio::task::spawn_local(async move {
                gated
                    .flush(FlushOptions {
                        reason: Some("manual".into()),
                        ..FlushOptions::default()
                    })
                    .await
            });
            gate.wait(name).await;
            let during = &spec["during"];
            let capture = service
                .capture_turn(
                    &during["messages"]
                        .as_array()
                        .expect("#776 during messages")
                        .iter()
                        .map(js_from)
                        .collect::<Vec<_>>(),
                    CaptureTurnInput {
                        session_key: text_at(&during["input"], "sessionKey").to_string(),
                        timestamp: Some(text_at(&during["input"], "timestamp").into()),
                        source: None,
                    },
                )
                .expect("#776 capture during flush");
            gate.release();
            let stats = flush
                .await
                .expect("#776 flush join")
                .expect("#776 flush after capture");
            ctx.clean(json!({
                "stats": encode_js(&stats.to_value()),
                "capture": encode_js(&capture.to_value()),
                "l0": ctx.l0(),
                "requestKinds": ctx.kinds(),
            }))
        }
        "flush-inflight-close" => {
            let gate = gate.as_mut().expect("#776 flush-inflight-close gate");
            let gated = service.clone();
            let flush = tokio::task::spawn_local(async move {
                gated
                    .flush(FlushOptions {
                        reason: Some("manual".into()),
                        ..FlushOptions::default()
                    })
                    .await
            });
            gate.wait(name).await;
            let close_error = match service.close() {
                Ok(()) => Value::Null,
                Err(error) => json!(error_shape(&error)),
            };
            gate.release();
            let mut out = match flush.await.expect("#776 flush join") {
                Ok(stats) => json!({"ok": true, "stats": encode_js(&stats.to_value())}),
                Err(error) => json!({"ok": false, "error": error_shape(&error)}),
            };
            out["closeError"] = close_error;
            out["files"] = ctx.file_names();
            ctx.clean(out)
        }
        "dream-stage-act" => {
            let seeds = seed_round(0);
            let gate = gate.as_mut().expect("#776 dream-stage-act gate");
            let gated = service.clone();
            let dream =
                tokio::task::spawn_local(async move { gated.dream(DreamTrigger::Manual).await });
            gate.wait(name).await;
            let mut input = JsObject::new();
            input.insert("action".into(), JsValue::String("edit_entry".into()));
            input.insert(
                "id".into(),
                JsValue::String(js_text(&seeds[0], "relativePath").into()),
            );
            input.insert(
                "name".into(),
                JsValue::String("Alpha one EDITED DURING DREAM".into()),
            );
            input.insert(
                "description".into(),
                JsValue::String("edited during dream".into()),
            );
            let act = service.act(&input).expect("#776 act during dream");
            gate.release();
            let result = dream
                .await
                .expect("#776 dream join")
                .expect("#776 dream after act");
            let full = encode_js(&result.to_value());
            let names = ctx.names();
            ctx.clean(json!({
                "act": encode_js(&act),
                "result": {
                    "rewrittenProjects": full["rewrittenProjects"],
                    "deletedFiles": full["deletedFiles"],
                    "status": full["status"],
                    "summary": full["summary"],
                },
                "namesAfter": names,
                "editedSurvived": names.contains(&"Alpha one EDITED DURING DREAM".to_string()),
                "files": ctx.files(),
                "requestKinds": ctx.kinds(),
            }))
        }
        "dream-stage-file-write" => {
            let _seeds = seed_round(0);
            let gate = gate.as_mut().expect("#776 dream-stage-file-write gate");
            let gated = service.clone();
            let dream =
                tokio::task::spawn_local(async move { gated.dream(DreamTrigger::Manual).await });
            gate.wait(name).await;
            let written = service
                .repository
                .get_file_memory_store()
                .execute("upsertCandidate", &js_object_from(&spec["during"]["input"]))
                .expect("#776 upsert during dream");
            gate.release();
            let result = dream
                .await
                .expect("#776 dream join")
                .expect("#776 dream after write");
            let full = encode_js(&result.to_value());
            let names = ctx.names();
            ctx.clean(json!({
                "writtenName": js_text(&written, "name"),
                "result": {"rewrittenProjects": full["rewrittenProjects"], "status": full["status"]},
                "writeSurvived": names.contains(&"Written During Dream".to_string()),
                "requestKinds": ctx.kinds(),
            }))
        }
        "dream-stage-capture" => {
            let _seeds = seed_round(0);
            let gate = gate.as_mut().expect("#776 dream-stage-capture gate");
            let gated = service.clone();
            let dream =
                tokio::task::spawn_local(async move { gated.dream(DreamTrigger::Manual).await });
            gate.wait(name).await;
            let during = &spec["during"];
            let capture = service
                .capture_turn(
                    &during["messages"]
                        .as_array()
                        .expect("#776 during messages")
                        .iter()
                        .map(js_from)
                        .collect::<Vec<_>>(),
                    CaptureTurnInput {
                        session_key: text_at(&during["input"], "sessionKey").to_string(),
                        timestamp: Some(text_at(&during["input"], "timestamp").into()),
                        source: None,
                    },
                )
                .expect("#776 capture during dream");
            gate.release();
            let result = dream
                .await
                .expect("#776 dream join")
                .expect("#776 dream after capture");
            let full = encode_js(&result.to_value());
            ctx.clean(json!({
                "capture": encode_js(&capture.to_value()),
                "result": {"status": full["status"], "rewrittenProjects": full["rewrittenProjects"]},
                "l0": ctx.l0(),
                "requestKinds": ctx.kinds(),
            }))
        }
        "dream-stage-clear-session" => {
            let _seeds = seed_round(0);
            let gate = gate.as_mut().expect("#776 dream-stage-clear-session gate");
            let gated = service.clone();
            let dream =
                tokio::task::spawn_local(async move { gated.dream(DreamTrigger::Manual).await });
            gate.wait(name).await;
            let cleared = service
                .clear_session("s1")
                .expect("#776 clear during dream");
            gate.release();
            let result = dream
                .await
                .expect("#776 dream join")
                .expect("#776 dream after clear");
            let full = encode_js(&result.to_value());
            ctx.clean(json!({
                "cleared": encode_js(&cleared),
                "result": {"status": full["status"], "rewrittenProjects": full["rewrittenProjects"]},
                "l0": ctx.l0(),
                "requestKinds": ctx.kinds(),
            }))
        }
        "dream-stage-rollback-guard" | "dream-stage-rollback-ok" => {
            let _seeds = seed_round(0);
            let dream1 = service
                .dream(DreamTrigger::Manual)
                .await
                .expect("#776 first dream");
            let names_after_dream1 = ctx.names();
            let _gamma = seed_round(1);
            // `ctx.gate(1)` in the TS cell is installed here, after dream1's
            // requests, with a fresh fetch-call counter.
            ctx.transport.arm();
            let gate = gate.as_mut().expect("#776 rollback gate");
            let gated = service.clone();
            let dream2 =
                tokio::task::spawn_local(async move { gated.dream(DreamTrigger::Manual).await });
            gate.wait(name).await;
            let rollback = match service.rollback_last_dream() {
                Ok(result) if name == "dream-stage-rollback-guard" => {
                    // TS: `(await rollbackLastDream()).rolledBackAt != null` —
                    // false for both null and undefined in JS.
                    json!({
                        "ok": true,
                        "rolledBackAt": !matches!(
                            result.get("rolledBackAt"),
                            None | Some(JsValue::Null)
                        ),
                    })
                }
                Ok(result) => json!({"ok": true, "result": encode_js(&result)}),
                Err(error) => json!({"ok": false, "error": error_shape(&error)}),
            };
            gate.release();
            let dream2result = match dream2.await.expect("#776 second dream join") {
                Ok(result) => {
                    let full = encode_js(&result.to_value());
                    json!({
                        "ok": true,
                        "result": {
                            "status": full["status"],
                            "rewrittenProjects": full["rewrittenProjects"],
                            "summary": full["summary"],
                        },
                    })
                }
                Err(error) => json!({"ok": false, "error": error_shape(&error)}),
            };
            let full1 = encode_js(&dream1.to_value());
            ctx.clean(json!({
                "dream1": {"rewrittenProjects": full1["rewrittenProjects"], "names": names_after_dream1},
                "rollbackDuringDream": rollback,
                "dream2": dream2result,
                "namesFinal": ctx.names(),
                "files": ctx.files(),
                "pipeline": ctx.pipeline(&["lastDreamAt", "lastDreamStatus", "autoDreamAnchorAt"]),
                "requestKinds": ctx.kinds(),
            }))
        }
        "dream-stage-error" => {
            let _seeds = seed_round(0);
            let outcome = service.dream(DreamTrigger::Manual).await;
            let (created, disposed) = service.dream_stage_counters();
            let out = match outcome {
                // The oracle cell requires the failure surface; a success is a
                // replay divergence and fails the comparison either way.
                Ok(_) => panic!("#776 dream-stage-error: dream unexpectedly succeeded"),
                Err(error) => json!({"ok": false, "error": error_shape(&error)}),
            };
            ctx.clean(json!({
                "ok": out["ok"],
                "error": out["error"],
                "stageCreated": created,
                "disposed": disposed,
                "namesAfter": ctx.names(),
                "files": ctx.files(),
                "pipeline": ctx.pipeline(&["lastDreamAt", "lastDreamStatus", "lastDreamSummary"]),
                "lastDreamTree": ctx.last_dream_tree(),
                "stageTempLeft": ctx.stage_temp_left(),
            }))
        }
        "dream-noop" => {
            let result = service
                .dream(DreamTrigger::Manual)
                .await
                .expect("#776 noop dream");
            let full = encode_js(&result.to_value());
            ctx.clean(json!({
                "result": {
                    "status": full["status"],
                    "rewrittenProjects": full["rewrittenProjects"],
                    "deletedFiles": full["deletedFiles"],
                    "deletedProjects": full["deletedProjects"],
                    "profileUpdated": full["profileUpdated"],
                    "duplicateTopicCount": full["duplicateTopicCount"],
                    "conflictTopicCount": full["conflictTopicCount"],
                    "reviewedFiles": full["reviewedFiles"],
                    "summary": full["summary"],
                    "trigger": full["trigger"],
                },
                "files": ctx.files(),
                "pipeline": ctx.pipeline(&[
                    "lastDreamAt", "lastDreamStatus", "lastDreamSummary", "autoDreamAnchorAt",
                    "autoIndexAnchorAt",
                ]),
                "lastDreamTree": ctx.last_dream_tree(),
            }))
        }
        "dream-stage-close" => {
            let _seeds = seed_round(0);
            let gate = gate.as_mut().expect("#776 dream-stage-close gate");
            let gated = service.clone();
            let dream =
                tokio::task::spawn_local(async move { gated.dream(DreamTrigger::Manual).await });
            gate.wait(name).await;
            let close_error = match service.close() {
                Ok(()) => Value::Null,
                Err(error) => json!(error_shape(&error)),
            };
            gate.release();
            let mut out = match dream.await.expect("#776 dream join") {
                Ok(result) => {
                    let full = encode_js(&result.to_value());
                    json!({"ok": true, "result": {"status": full["status"]}})
                }
                Err(error) => json!({"ok": false, "error": error_shape(&error)}),
            };
            out["closeError"] = close_error;
            out["files"] = ctx.files();
            out["lastDreamTree"] = ctx.last_dream_tree();
            out["pipeline"] = ctx.pipeline(&["lastDreamAt", "lastDreamStatus"]);
            ctx.clean(out)
        }
        "dream-stage-dream" => {
            let _seeds = seed_round(0);
            let gate = gate.as_mut().expect("#776 dream-stage-dream gate");
            let gated = service.clone();
            let dream1 =
                tokio::task::spawn_local(async move { gated.dream(DreamTrigger::Manual).await });
            gate.wait(name).await;
            let b = service
                .dream(DreamTrigger::Manual)
                .await
                .expect("#776 second dream");
            gate.release();
            let a = dream1
                .await
                .expect("#776 first dream join")
                .expect("#776 first dream");
            let first = encode_js(&a.to_value());
            let second = encode_js(&b.to_value());
            ctx.clean(json!({
                "first": {"status": first["status"], "rewrittenProjects": first["rewrittenProjects"], "summary": first["summary"]},
                "second": {"status": second["status"], "rewrittenProjects": second["rewrittenProjects"], "summary": second["summary"]},
                "names": ctx.names(),
                "files": ctx.files(),
                "pipeline": ctx.pipeline(&["lastDreamAt", "lastDreamStatus"]),
                "requestKinds": ctx.kinds(),
            }))
        }
        "maintenance-backlog" => {
            let outcome = service
                .run_due_scheduled_maintenance("scheduled")
                .await
                .expect("#776 maintenance");
            let full = encode_js(&outcome.to_value());
            ctx.clean(json!({
                "result": {
                    "indexRan": full["indexRan"],
                    "dreamRan": full["dreamRan"],
                    "indexStats": full["indexStats"],
                },
                "files": ctx.files(),
                "pipeline": ctx.pipeline(&["autoIndexAnchorAt", "autoDreamAnchorAt", "lastIndexedAt"]),
                "l0": ctx.l0(),
                "requestKinds": ctx.kinds(),
            }))
        }
        // The TS cell's return object for this scenario has no `l0` field;
        // the harness compares observed shapes, not just values.
        "maintenance-interval" => {
            let outcome = service
                .run_due_scheduled_maintenance("scheduled")
                .await
                .expect("#776 maintenance");
            let full = encode_js(&outcome.to_value());
            ctx.clean(json!({
                "result": {
                    "indexRan": full["indexRan"],
                    "dreamRan": full["dreamRan"],
                    "indexStats": full["indexStats"],
                },
                "files": ctx.files(),
                "pipeline": ctx.pipeline(&["autoIndexAnchorAt", "autoDreamAnchorAt", "lastIndexedAt"]),
                "requestKinds": ctx.kinds(),
            }))
        }
        "maintenance-interval-zero" => {
            let outcome = service
                .run_due_scheduled_maintenance("scheduled")
                .await
                .expect("#776 interval-zero maintenance");
            let full = encode_js(&outcome.to_value());
            ctx.clean(json!({
                "result": {"indexRan": full["indexRan"], "dreamRan": full["dreamRan"]},
                "files": ctx.file_names(),
                "pipeline": ctx.pipeline(&["autoIndexAnchorAt", "autoDreamAnchorAt"]),
                "l0": ctx.l0(),
            }))
        }
        "maintenance-dream-due" => {
            let _seeds = seed_round(0);
            let outcome = service
                .run_due_scheduled_maintenance("scheduled")
                .await
                .expect("#776 dream-due maintenance");
            let full = encode_js(&outcome.to_value());
            let dream_result = &full["dreamResult"];
            ctx.clean(json!({
                "result": {
                    "indexRan": full["indexRan"],
                    "dreamRan": full["dreamRan"],
                    "dreamResult": {
                        "trigger": dream_result["trigger"],
                        "status": dream_result["status"],
                        "rewrittenProjects": dream_result["rewrittenProjects"],
                    },
                },
                "files": ctx.files(),
                "pipeline": ctx.pipeline(&["lastDreamAt", "lastDreamStatus", "autoDreamAnchorAt"]),
                "lastDreamTree": ctx.last_dream_tree(),
                "requestKinds": ctx.kinds(),
            }))
        }
        // #1036 item 1: elapsed == the default 30-minute interval exactly —
        // the inclusive `>=` boundary must index (probed on real TS).
        "maintenance-interval-exact" => {
            let outcome = service
                .run_due_scheduled_maintenance("scheduled")
                .await
                .expect("#1036 interval-exact maintenance");
            let full = encode_js(&outcome.to_value());
            ctx.clean(json!({
                "result": {
                    "indexRan": full["indexRan"],
                    "dreamRan": full["dreamRan"],
                    "indexStats": ts_undefined_or(&full, "indexStats"),
                },
                "files": ctx.files(),
                "pipeline": ctx.pipeline(&["autoIndexAnchorAt", "autoDreamAnchorAt", "lastIndexedAt"]),
                "requestKinds": ctx.kinds(),
            }))
        }
        // #1036 item 1: one millisecond short of the interval — must NOT
        // index. The TS cell's return object keeps `l0` here (the pending
        // session survives unindexed), so this arm mirrors that shape.
        "maintenance-interval-just-before" => {
            let outcome = service
                .run_due_scheduled_maintenance("scheduled")
                .await
                .expect("#1036 just-before maintenance");
            let full = encode_js(&outcome.to_value());
            ctx.clean(json!({
                "result": {
                    "indexRan": full["indexRan"],
                    "dreamRan": full["dreamRan"],
                    "indexStats": ts_undefined_or(&full, "indexStats"),
                },
                "files": ctx.files(),
                "pipeline": ctx.pipeline(&["autoIndexAnchorAt", "autoDreamAnchorAt", "lastIndexedAt"]),
                "l0": ctx.l0(),
                "requestKinds": ctx.kinds(),
            }))
        }
        // #1036 item 2: 19 pending dialogue turns — below the `>= 20`
        // backlog threshold, so no indexing must run.
        "maintenance-backlog-below" => {
            let outcome = service
                .run_due_scheduled_maintenance("scheduled")
                .await
                .expect("#1036 backlog-below maintenance");
            let full = encode_js(&outcome.to_value());
            ctx.clean(json!({
                "result": {
                    "indexRan": full["indexRan"],
                    "dreamRan": full["dreamRan"],
                    "indexStats": ts_undefined_or(&full, "indexStats"),
                },
                "files": ctx.files(),
                "pipeline": ctx.pipeline(&["autoIndexAnchorAt", "autoDreamAnchorAt", "lastIndexedAt"]),
                "l0": ctx.l0(),
                "requestKinds": ctx.kinds(),
            }))
        }
        other => panic!("#776 unknown scenario {other}"),
    };

    // -- teardown and the scenario record ------------------------------------
    let _ = service.close();
    let sqlite = ctx.clean(sqlite_dump(&service.db_path));
    json!({
        "name": name,
        "script": spec["script"].clone(),
        "seeds": ctx.clean(spec["seeds"].clone()),
        "captures": ctx.clean(spec["captures"].clone()),
        "settings": ctx.clean(spec["settings"].clone()),
        "setState": spec["setState"].clone(),
        "gate": spec["gate"].clone(),
        "during": ctx.clean(spec["during"].clone()),
        "awaitDuring": spec["awaitDuring"].clone(),
        "observed": observed,
        "sqlite": sqlite,
    })
}

fn observations(oracle: &Value) -> Value {
    let scratch = Scratch::new("edgeclaw-service-orchestration");
    let root = scratch.path();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#776 current-thread runtime");
    // The service's async paths hold non-Send `&mut dyn FnMut(JsValue)` seams,
    // so the gated calls run as `spawn_local` tasks on a `LocalSet` — still a
    // single thread, like the TS event loop the oracle recorded on.
    let scenarios = runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        let mut scenarios = Vec::new();
        // One cycling Math.random for the whole run, like observe.mts's
        // process-wide `randomIndex`.
        let math_index = Arc::new(AtomicUsize::new(0));
        for (index, spec) in oracle["scenarios"]
            .as_array()
            .expect("#776 scenarios")
            .iter()
            .enumerate()
        {
            // Every wait inside a cell is bounded (the gate waits 30s), but a
            // regression that makes any completion wait unbounded — e.g. the
            // second operation waiting for a first that is itself waiting on
            // the test — must fail here instead of hanging the suite (TS has
            // no such bound either: the parked-flush probe in
            // `_tmp/20261026-issue-776/fix-r1/probe/hang.mts` stays pending
            // past the 30s request timeout).
            let name = text_at(spec, "name");
            scenarios.push(
                tokio::time::timeout(
                    Duration::from_secs(180),
                    run_scenario(spec, root, index + 1, &math_index),
                )
                .await
                .unwrap_or_else(|_| panic!("#776 scenario {name} exceeded the 180s cell deadline")),
            );
        }
        scenarios
    }));
    json!({"scenarios": scenarios})
}

#[test]
fn rust_matches_ts_oracle() {
    // Provenance (hash format checks; git checks run when PDRS_BASELINE is
    // set for the ignored verifiers).
    let _ = baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).expect("#776 oracle is valid JSON");
    let observed = observations(&oracle);
    let expected = oracle["scenarios"].as_array().expect("#776 scenarios");
    let actual = observed["scenarios"].as_array().expect("#776 scenarios");
    assert_eq!(
        actual.len(),
        expected.len(),
        "#776 scenario count differs from TS"
    );
    for (actual, expected) in actual.iter().zip(expected) {
        let name = text_at(expected, "name");
        for key in ["observed", "sqlite", "seeds", "captures", "settings"] {
            assert_eq!(
                actual.get(key),
                expected.get(key),
                "#776 scenario {name}: {key} differs from the real TS"
            );
        }
    }
}

/// P1-3 (fix-r1): the completion waits a regression could make unbounded
/// must fail bounded, not hang the suite. The first flush parks at its
/// classify request and is never released — the real TS service has no
/// built-in bound either (`_tmp/20261026-issue-776/fix-r1/probe/hang.mts`
/// stays pending past the 30s request timeout, which only aborts the fetch
/// that actually went out). The second flush must still complete while the
/// first is parked, and cancelling the parked predecessor must leave the
/// service usable.
#[test]
fn flush_second_completes_when_first_never_does() {
    let scratch = Scratch::new("edgeclaw-service-orchestration-hang");
    let root = scratch.path();
    let dir = root.join("svc-hang");
    fs::create_dir_all(dir.join("workspace")).expect("#776 hang scenario workspace");
    fs::create_dir_all(dir.join("home")).expect("#776 hang scenario home");
    // flush2's four steps reach the transport while flush1's classify stays
    // parked before it, exactly like the flush-inflight-flush cell.
    let step = |value: Value| {
        (
            200,
            json!({"choices": [{"message": {"content": value.to_string()}}]}).to_string(),
        )
    };
    let note = |name: &str| {
        json!({
            "skip": false,
            "reason": "",
            "name": name,
            "description": format!("{name} description"),
            "markdown": format!("## Summary\n{name} body"),
        })
    };
    let steps = vec![
        step(json!({"should_store": true, "labels": [{"type": "project", "reason": "r"}]})),
        step(note("Alpha")),
        step(json!({"should_store": true, "labels": [{"type": "project", "reason": "r"}]})),
        step(note("Beta")),
    ];
    let (transport, mut gate) = ScriptedTransport::new(steps, Some(1));
    let gate = gate.as_mut().expect("#776 hang scenario gate");
    let math_index = Arc::new(AtomicUsize::new(0));
    let service = Arc::new(build_service(
        &dir,
        Arc::new(transport.clone()),
        &math_index,
    ));
    let messages: Vec<JsValue> = [
        json!({"role": "user", "content": "alpha user"}),
        json!({"role": "assistant", "content": "alpha assistant"}),
    ]
    .iter()
    .map(js_from)
    .collect();
    for (session, timestamp) in [
        ("s1", "2026-10-22T00:00:01.000Z"),
        ("s2", "2026-10-22T00:00:02.000Z"),
    ] {
        service
            .capture_turn(
                &messages,
                CaptureTurnInput {
                    session_key: session.into(),
                    timestamp: Some(timestamp.into()),
                    source: None,
                },
            )
            .expect("#776 hang scenario seed captureTurn");
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#776 hang scenario runtime");
    runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        transport.arm();
        let gated = service.clone();
        let first = tokio::task::spawn_local(async move {
            gated
                .flush(FlushOptions {
                    reason: Some("manual".into()),
                    session_keys: Some(vec!["s1".into()]),
                    ..FlushOptions::default()
                })
                .await
        });
        gate.wait("flush-never-released").await;
        // The parked predecessor must not block the second flush, and this
        // wait must fail bounded (not hang) if a regression couples them.
        let second = tokio::time::timeout(
            Duration::from_secs(30),
            service.flush(FlushOptions {
                reason: Some("manual".into()),
                ..FlushOptions::default()
            }),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "#776 flush-never-released: the second flush waited for the \
                 never-completing first flush (unbounded completion wait)"
            )
        })
        .expect("#776 flush-never-released: the second flush failed");
        let second = encode_js(&second.to_value());
        assert_eq!(
            second.get("capturedSessions").and_then(Value::as_f64),
            Some(2.0),
            "#776 flush-never-released: the second flush did not process both sessions"
        );
        assert_eq!(
            tree(&service.memory_dir).len(),
            4,
            "#776 flush-never-released: the second flush did not index both sessions"
        );
        // Cancel the parked predecessor; the service must stay usable.
        first.abort();
        let cancelled = first.await;
        assert!(
            cancelled.is_err(),
            "#776 flush-never-released: the parked first flush completed after abort"
        );
    }));
    service
        .close()
        .expect("#776 flush-never-released close service");
}

/// fix-r2: `flush-inflight-flush` cannot observe a widened `session_keys`.
/// The parked flush resolves `list_pending_session_keys` before its gated
/// request, and the concurrent second flush indexes s2 before the first
/// resumes, so the fix-r1 `flush-concurrent-consumption` mutant
/// (`HeartbeatRunOptions { session_keys: None }`) produced the same cell and
/// survived on the host and locally. This deterministic cell walks the same
/// `flush` → `run_heartbeat` filter without that interleaving: a flush scoped
/// to s1 must not consume s2.
#[test]
fn flush_session_filter_is_honored() {
    let scratch = Scratch::new("edgeclaw-service-orchestration-session-filter");
    let root = scratch.path();
    let dir = root.join("svc-filter");
    fs::create_dir_all(dir.join("workspace")).expect("#776 session-filter workspace");
    fs::create_dir_all(dir.join("home")).expect("#776 session-filter home");
    let step = |value: Value| {
        (
            200,
            json!({"choices": [{"message": {"content": value.to_string()}}]}).to_string(),
        )
    };
    let note = |name: &str| {
        json!({
            "skip": false,
            "reason": "",
            "name": name,
            "description": format!("{name} description"),
            "markdown": format!("## Summary\n{name} body"),
        })
    };
    let steps = vec![
        step(json!({"should_store": true, "labels": [{"type": "project", "reason": "r"}]})),
        step(note("Alpha")),
        step(json!({"should_store": true, "labels": [{"type": "project", "reason": "r"}]})),
        step(note("Beta")),
    ];
    let (transport, gate) = ScriptedTransport::new(steps, None);
    assert!(
        gate.is_none(),
        "#776 session-filter: unexpected request gate"
    );
    let math_index = Arc::new(AtomicUsize::new(0));
    let service = Arc::new(build_service(
        &dir,
        Arc::new(transport.clone()),
        &math_index,
    ));
    let messages: Vec<JsValue> = [
        json!({"role": "user", "content": "alpha user"}),
        json!({"role": "assistant", "content": "alpha assistant"}),
    ]
    .iter()
    .map(js_from)
    .collect();
    for (session, timestamp) in [
        ("s1", "2026-10-22T00:00:01.000Z"),
        ("s2", "2026-10-22T00:00:02.000Z"),
    ] {
        service
            .capture_turn(
                &messages,
                CaptureTurnInput {
                    session_key: session.into(),
                    timestamp: Some(timestamp.into()),
                    source: None,
                },
            )
            .expect("#776 session-filter: seed captureTurn");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#776 session-filter: runtime");
    runtime.block_on(async {
        let stats = service
            .flush(FlushOptions {
                reason: Some("manual".into()),
                session_keys: Some(vec!["s1".into()]),
                ..FlushOptions::default()
            })
            .await
            .expect("#776 session-filter: flush failed");
        let stats = encode_js(&stats.to_value());
        assert_eq!(
            stats.get("capturedSessions").and_then(Value::as_f64),
            Some(1.0),
            "#776 session-filter: flush captured sessions outside its sessionKeys"
        );
        assert_eq!(
            stats.get("writtenFiles").and_then(Value::as_f64),
            Some(1.0),
            "#776 session-filter: flush wrote files outside its sessionKeys"
        );
        let pending = service
            .repository
            .storage
            .list_pending_session_keys(30, None)
            .expect("#776 session-filter: list pending sessions");
        assert_eq!(
            pending,
            vec!["s2".to_string()],
            "#776 session-filter: the filtered-out session was consumed"
        );
    });
    service.close().expect("#776 session-filter: close service");
}

/// #1036 P2-5: a dropped (cancelled) `dream` future must still dispose its
/// stage. TS never cancels — `dispose` always runs in `finally` — so no oracle
/// cell can cover it; a Rust caller can drop the future at any await point
/// (`tokio::time::timeout`, `select!`, `abort`). Park the dream at its first
/// staged LLM request, abort it there, and prove the Drop fallback removed
/// every `.<root>.dream-*` staged temp sibling and kept `dream_stages_disposed`
/// balanced with `dream_stages_created`.
#[test]
fn dream_cancellation_disposes_stage_and_balances_counters() {
    let scratch = Scratch::new("edgeclaw-service-orchestration-cancel");
    let root = scratch.path();
    let dir = root.join("svc-cancel");
    fs::create_dir_all(dir.join("workspace")).expect("#1036 cancel scenario workspace");
    fs::create_dir_all(dir.join("home")).expect("#1036 cancel scenario home");
    let step = |value: Value| {
        (
            200,
            json!({"choices": [{"message": {"content": value.to_string()}}]}).to_string(),
        )
    };
    // The plan request parks and the dream future is aborted there; the step
    // content never reaches the dream.
    let steps = vec![step(
        json!({"clusters": [], "summary": "cancel plan summary"}),
    )];
    let (transport, mut gate) = ScriptedTransport::new(steps, Some(1));
    let gate = gate.as_mut().expect("#1036 cancel scenario gate");
    let math_index = Arc::new(AtomicUsize::new(0));
    let service = Arc::new(build_service(
        &dir,
        Arc::new(transport.clone()),
        &math_index,
    ));
    let store = service.repository.get_file_memory_store();
    for name in ["Cancel one", "Cancel two"] {
        let input = json!({
            "type": "project",
            "scope": "project",
            "name": name,
            "description": format!("{name} description"),
            "body": format!("## Summary\n{name} body"),
            "capturedAt": NOW,
            "sourceSessionKey": format!("cancel-seed-{}", name.to_lowercase().replace(' ', "-")),
        });
        store
            .execute("upsertCandidate", &js_object_from(&input))
            .expect("#1036 cancel scenario seed upsertCandidate");
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#1036 cancel scenario runtime");
    runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        transport.arm();
        let gated = service.clone();
        let dream =
            tokio::task::spawn_local(async move { gated.dream(DreamTrigger::Manual).await });
        gate.wait("dream-cancel").await;
        // Cancel the parked dream: the future is dropped inside its staged LLM
        // await, exactly where a `tokio::time::timeout` would drop it.
        dream.abort();
        let cancelled = dream.await;
        assert!(
            cancelled.is_err(),
            "#1036 dream-cancel: the parked dream completed after abort"
        );
    }));
    let (created, disposed) = service.dream_stage_counters();
    assert_eq!(
        (created, disposed),
        (1, 1),
        "#1036 dream-cancel: a cancelled dream must still dispose its stage \
         (created {created}, disposed {disposed})"
    );
    let ctx = Ctx {
        service: service.clone(),
        transport,
        root: root.to_path_buf(),
        workspace_hash: hash_text(&JsString::from(
            service.workspace_dir.to_string_lossy().into_owned(),
        )),
    };
    assert_eq!(
        ctx.stage_temp_left(),
        json!([]),
        "#1036 dream-cancel: the cancelled dream left staged dream temp dirs behind"
    );
    service.close().expect("#1036 dream-cancel: close service");
}

#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn real_ts_reads_rust_service_observations() {
    let oracle: Value = serde_json::from_str(ORACLE).expect("#776 oracle is valid JSON");
    let observed = observations(&oracle);
    let scratch = Scratch::new("edgeclaw-service-orchestration-verify");
    let path = scratch.path().join("rust-observations.json");
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&observed).expect("#776 observations serialize")
        ),
    )
    .expect("#776 write rust observations");
    let baseline = std::env::var_os("PDRS_BASELINE").expect("#776 PDRS_BASELINE");
    let mut command = baseline::node_command();
    command
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/edgeclaw-service-orchestration/verify.mts"),
        )
        .arg(&path)
        .env("PDRS_BASELINE", &baseline)
        .stdin(Stdio::null());
    run_node(&mut command, scratch.path());
}

#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-service-orchestration");
}

fn run_node(command: &mut Command, scratch: &Path) {
    let stdout = scratch.join("node.stdout");
    let stderr = scratch.join("node.stderr");
    let mut child = command
        .stdout(Stdio::from(
            fs::File::create(&stdout).expect("#776 create node stdout"),
        ))
        .stderr(Stdio::from(
            fs::File::create(&stderr).expect("#776 create node stderr"),
        ))
        .spawn()
        .expect("#776 spawn node verify");
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("#776 poll node verify") {
            break status;
        }
        if start.elapsed() > Duration::from_secs(180) {
            let _ = child.kill();
            // Reap before panicking: a killed-but-unwaited child is a zombie
            // on Unix and would keep the scratch/output files open.
            let _ = child.wait();
            panic!("#776 verify.mts timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let stderr_text = fs::read_to_string(stderr).unwrap_or_default();
    // Keep the fixture's dedicated mismatch line in the panic header so the
    // negative control binds the encoding difference, not the wrapper failure.
    let dedicated = stderr_text
        .lines()
        .find(|line| line.contains("#776 negative-zero encoding mismatch at"))
        .map(|line| format!("\n{line}"))
        .unwrap_or_default();
    assert!(
        status.success(),
        "#776 verify.mts failed{dedicated}\nstdout:\n{}\nstderr:\n{stderr_text}",
        fs::read_to_string(stdout).unwrap_or_default()
    );
}
