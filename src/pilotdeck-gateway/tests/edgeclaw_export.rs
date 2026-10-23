//! Real TS repository export/import transcript: TS .db -> Rust, and Rust -> TS.
//!
//! Operations and inputs are carried as JS-owned values (`pilotdeck_protocol`
//! keeps JSON key order), and every observation is compared as serialized
//! bytes with JSON.stringify semantics, so key order, number formatting and
//! optional-field presence are all byte-exact against the TS recording.
use base64::{Engine, engine::general_purpose::STANDARD};
use pilotdeck_gateway::runtime::memory::edgeclaw::{
    EdgeClawFileStores, FileMemoryError, FileMemoryStore, FileMemoryStoreOptions, IndexingSettings,
    L0SessionRecord, StorageError,
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
#[path = "common/baseline.rs"]
mod baseline;
const ORACLE: &str = include_str!("fixtures/edgeclaw-export/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("pdrs-export-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn oracle() -> Value {
    baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).unwrap()
}
fn expand(v: &Value, root: &Path) -> Value {
    match v {
        Value::String(s) => json!(s.replace("$ROOT", &root.to_string_lossy())),
        Value::Array(a) => Value::Array(a.iter().map(|v| expand(v, root)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), expand(v, root)))
                .collect(),
        ),
        v => v.clone(),
    }
}
fn clean(v: &Value, root: &Path) -> Value {
    match v {
        Value::String(s) => json!(s.replace(root.to_str().unwrap(), "$ROOT")),
        Value::Array(a) => Value::Array(a.iter().map(|v| clean(v, root)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), clean(v, root))).collect())
        }
        v => v.clone(),
    }
}
fn js_text(v: &JsValue, key: &str) -> String {
    v.get(key)
        .and_then(JsValue::as_string)
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_default()
}
fn js_number(v: &JsValue, key: &str) -> Option<f64> {
    v.get(key).and_then(JsValue::as_number)
}
/// `expand` for JS-owned values: `$ROOT` substitution and the `$NAN` marker
/// (a NaN number, unencodable in JSON) without losing key order.
fn expand_js(v: &JsValue, root: &Path) -> JsValue {
    match v {
        JsValue::String(s) => {
            let text = String::from_utf16_lossy(s.units());
            if text == "$NAN" {
                JsValue::Number(f64::NAN)
            } else {
                JsValue::String(text.replace("$ROOT", &root.to_string_lossy()).into())
            }
        }
        JsValue::Array(items) => JsValue::Array(items.iter().map(|v| expand_js(v, root)).collect()),
        JsValue::Object(object) => {
            let mut expanded = JsObject::new();
            for (key, v) in object.entries() {
                expanded.insert(key.clone(), expand_js(v, root));
            }
            JsValue::Object(expanded)
        }
        v => v.clone(),
    }
}
/// `clean` for JS-owned values: masking the scratch root without losing key
/// order, so observations compare equal as serialized bytes.
fn clean_js(v: &JsValue, root: &Path) -> JsValue {
    match v {
        JsValue::String(s) => JsValue::String(
            String::from_utf16_lossy(s.units())
                .replace(root.to_str().unwrap(), "$ROOT")
                .into(),
        ),
        JsValue::Array(items) => JsValue::Array(items.iter().map(|v| clean_js(v, root)).collect()),
        JsValue::Object(object) => {
            let mut cleaned = JsObject::new();
            for (key, v) in object.entries() {
                cleaned.insert(key.clone(), clean_js(v, root));
            }
            JsValue::Object(cleaned)
        }
        // The TS harness renders `undefined` observations as null in JSON.
        JsValue::Undefined => JsValue::Null,
        v => v.clone(),
    }
}
/// An array member of the oracle parsed as a JS value, preserving the key
/// order the TS run recorded (serde_json's map would sort it away).
fn ordered_array(key: &str) -> Vec<JsValue> {
    match baseline::oracle(ORACLE).get(key) {
        Some(JsValue::Array(items)) => items.clone(),
        _ => panic!("oracle.json is missing its {key} array"),
    }
}
fn bytes(v: &JsValue) -> String {
    stringify_json(v).unwrap().unwrap()
}
fn error(e: FileMemoryError) -> Value {
    match e {
        FileMemoryError::Storage(StorageError::Closed) => {
            json!({"error":"ERR_INVALID_STATE","message":"database is not open"})
        }
        FileMemoryError::Bundle(message) => {
            json!({"error":"MemoryBundleValidationError","message":message})
        }
        // `TypeError`s raised before any store call: the ToNumber/ToPrimitive
        // failures of the `Math.max` pagination coercions (js_value's checked
        // ports), reported like the TS harness (`e.code ?? e.name`).
        FileMemoryError::Type(message) => json!({"error":"TypeError","message":message}),
        e => panic!("unexpected repository error: {e}"),
    }
}
fn open_with_clock(
    root: &Path,
    id: &str,
    workspace: &str,
    clock: Arc<dyn Fn() -> String + Send + Sync>,
) -> EdgeClawFileStores {
    EdgeClawFileStores::with_workspace_clock(
        root.join("workspaces").join(id).join("control.sqlite"),
        root.join("workspaces").join(id).join("memory"),
        root.join("global"),
        workspace,
        root.join("home/.pilotdeck"),
        clock,
    )
    .unwrap()
}
/// The generator's clock phases: seeding runs frozen (`freezeClock()`), the
/// observed phase steps 1ms per read (`stepClock()`), so timestamped
/// observations expose the number and order of `nowIso()` reads and a replay
/// that reads the clock differently drifts and fails the byte comparison.
enum PhaseClock {
    Frozen,
    Stepping(Arc<Mutex<u64>>),
}
impl PhaseClock {
    fn clock(&self) -> Arc<dyn Fn() -> String + Send + Sync> {
        match self {
            PhaseClock::Frozen => Arc::new(|| NOW.into()),
            PhaseClock::Stepping(step) => {
                let step = Arc::clone(step);
                Arc::new(move || {
                    let value = {
                        let mut next = step.lock().unwrap();
                        let value = *next;
                        *next += 1;
                        value as i64
                    };
                    pilotdeck_transport::system_clock::format_epoch_iso(now_base_ms() + value)
                })
            }
        }
    }
}
fn now_base_ms() -> i64 {
    static BASE: OnceLock<i64> = OnceLock::new();
    *BASE.get_or_init(|| {
        pilotdeck_transport::system_clock::parse_node22_date(&pilotdeck_protocol::JsString::from(
            NOW,
        ))
        .expect("the fixture NOW must parse")
        .expect("the fixture NOW must be a valid date") as i64
    })
}
fn repos(root: &Path, o: &Value, clock: &PhaseClock) -> HashMap<String, EdgeClawFileStores> {
    o["configs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let id = c["id"].as_str().unwrap();
            (
                id.into(),
                open_with_clock(
                    root,
                    id,
                    expand(&c["workspace"], root).as_str().unwrap(),
                    clock.clock(),
                ),
            )
        })
        .collect()
}
fn files(root: &Path, dir: &Path, out: &mut Vec<Value>) {
    let mut entries = fs::read_dir(dir)
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        if e.file_type().unwrap().is_dir() {
            files(root, &path, out);
        } else {
            let name = path.file_name().unwrap().to_str().unwrap();
            if ["control.sqlite", "control.sqlite-wal", "control.sqlite-shm"].contains(&name) {
                continue;
            }
            out.push(json!({"relativePath":path.strip_prefix(root).unwrap().to_str().unwrap(),"content":fs::read_to_string(&path).unwrap()}));
        }
    }
}
fn tree(root: &Path) -> Value {
    let mut out = vec![];
    files(root, root, &mut out);
    json!(out)
}
fn invoke(
    repos: &mut HashMap<String, EdgeClawFileStores>,
    op: &JsValue,
    root: &Path,
    vars: &mut HashMap<String, JsValue>,
    clock: &PhaseClock,
) -> JsValue {
    let repo = js_text(op, "repo");
    let repo = if repo.is_empty() { "main" } else { &repo };
    let r = &repos[repo];
    let empty = JsValue::Object(JsObject::new());
    let input = expand_js(op.get("input").unwrap_or(&empty), root);
    let input_object = input.as_object().cloned().unwrap_or_default();
    let method = js_text(op, "method");
    let result: Result<JsValue, FileMemoryError> = match method.as_str() {
        "close" => r.close().map(|_| JsValue::Null),
        "mode" => Ok(JsValue::from(r.get_workspace_mode())),
        "workspace" => r
            .get_file_memory_store()
            .execute(&js_text(op, "action"), &input_object),
        "global" => r
            .get_global_user_store()
            .execute(&js_text(op, "action"), &input_object),
        "state" => {
            let value = input.get("value").cloned().unwrap_or(JsValue::Null);
            r.storage
                .set_pipeline_state(&js_text(&input, "key"), &value)
                .map(|_| JsValue::Null)
                .map_err(Into::into)
        }
        "readState" => r
            .storage
            .get_pipeline_state(&js_text(&input, "key"))
            .map(|v| v.unwrap_or(JsValue::Null))
            .map_err(Into::into),
        "settings" => {
            let empty_defaults = JsValue::Null;
            let defaults_input = input.get("defaults").unwrap_or(&empty_defaults);
            let defaults = IndexingSettings {
                reasoning_mode: if js_text(defaults_input, "reasoningMode").is_empty() {
                    "answer_first".into()
                } else {
                    js_text(defaults_input, "reasoningMode")
                },
                auto_index_interval_minutes: js_number(defaults_input, "autoIndexIntervalMinutes")
                    .unwrap_or(30.0) as i64,
                auto_dream_interval_minutes: js_number(defaults_input, "autoDreamIntervalMinutes")
                    .unwrap_or(60.0) as i64,
            };
            let partial = input.get("partial").cloned().unwrap_or(JsValue::Null);
            r.storage
                .save_indexing_settings(&partial, &defaults)
                .map(|s| {
                    let mut object = JsObject::new();
                    object.insert(
                        "reasoningMode".into(),
                        JsValue::String(s.reasoning_mode.as_str().into()),
                    );
                    object.insert(
                        "autoIndexIntervalMinutes".into(),
                        JsValue::Number(s.auto_index_interval_minutes as f64),
                    );
                    object.insert(
                        "autoDreamIntervalMinutes".into(),
                        JsValue::Number(s.auto_dream_interval_minutes as f64),
                    );
                    JsValue::Object(object)
                })
                .map_err(Into::into)
        }
        "export" => r.export_memory_bundle(),
        "import" => {
            // `@var` reference feeding a saved export back in (`resolveRefs`).
            let bundle = input.get("bundle").cloned().unwrap_or(JsValue::Null);
            let bundle = match &bundle {
                JsValue::String(s) => {
                    match String::from_utf16_lossy(s.units())
                        .strip_prefix('@')
                        .map(|key| key.to_string())
                    {
                        Some(key) => vars[&key].clone(),
                        None => bundle.clone(),
                    }
                }
                _ => bundle.clone(),
            };
            r.import_memory_bundle(&bundle)
        }
        "overview" => r.get_overview(),
        // `r.getUiSnapshot(input.limit)` passes any value through; only a
        // missing key uses the `limit = 50` default (like `getUiSnapshot()`).
        "uiSnapshot" => match input.get("limit") {
            Some(limit) => r.get_ui_snapshot(Some(limit)),
            None => r.get_ui_snapshot(None),
        },
        "list" => r.list_memory_entries(&input_object),
        "count" => r.count_memory_entries(&input_object),
        "l0" => r
            .storage
            .insert_l0_session(&L0SessionRecord {
                l0_index_id: js_text(&input, "l0IndexId"),
                session_key: js_text(&input, "sessionKey"),
                timestamp: js_text(&input, "timestamp"),
                messages: input.get("messages").cloned().unwrap_or(JsValue::Null),
                source: js_text(&input, "source"),
                indexed: matches!(input.get("indexed"), Some(JsValue::Bool(true))),
                created_at: js_text(&input, "createdAt"),
            })
            .map(|_| JsValue::Null)
            .map_err(Into::into),
        "case" => r
            .storage
            .save_case_trace(
                &input.get("record").cloned().unwrap_or(JsValue::Null),
                js_number(&input, "limit").unwrap_or(30.0) as usize,
            )
            .map(|_| JsValue::Null)
            .map_err(Into::into),
        "index" => r
            .storage
            .save_index_trace(
                &input.get("record").cloned().unwrap_or(JsValue::Null),
                js_number(&input, "limit").unwrap_or(30.0) as usize,
            )
            .map(|_| JsValue::Null)
            .map_err(Into::into),
        "dream" => r
            .storage
            .save_dream_trace(
                &input.get("record").cloned().unwrap_or(JsValue::Null),
                js_number(&input, "limit").unwrap_or(30.0) as usize,
            )
            .map(|_| JsValue::Null)
            .map_err(Into::into),
        "raw" => {
            let p = root.join(js_text(&input, "path"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, js_text(&input, "content")).unwrap();
            Ok(JsValue::Null)
        }
        "remove" => {
            fs::remove_dir_all(root.join(js_text(&input, "path"))).unwrap();
            Ok(JsValue::Null)
        }
        "reopen" => {
            let _ = r.close();
            let r = open_with_clock(root, repo, &js_text(&input, "workspace"), clock.clock());
            repos.insert(repo.into(), r);
            Ok(JsValue::Null)
        }
        _ => panic!("unknown method {method}"),
    };
    match result {
        Ok(observed) => {
            let save_as = js_text(op, "saveAs");
            if !save_as.is_empty() {
                vars.insert(save_as, observed.clone());
            }
            observed
        }
        Err(e) => parse_json(&error(e).to_string()).unwrap(),
    }
}
fn replay(root: &Path, o: &Value) {
    let expected = ordered_array("observations");
    let operations = ordered_array("operations");
    let clock = PhaseClock::Stepping(Arc::new(Mutex::new(0)));
    let mut repos = repos(root, o, &clock);
    let mut vars = HashMap::new();
    for (i, op) in operations.iter().enumerate() {
        let observed = invoke(&mut repos, op, root, &mut vars, &clock);
        // Byte-exact: JSON key order, number formatting and optional-field
        // presence all surface in the serialized form.
        assert_eq!(
            bytes(&clean_js(&observed, root)),
            bytes(&expected[i]),
            "observation {i}: {}",
            bytes(op)
        );
    }
    for r in repos.values() {
        let _ = r.close();
    }
    assert_eq!(clean(&tree(root), root), o["files"], "final file tree");
}
#[test]
fn ts_databases_and_file_tree_replay_repository_export_import() {
    let o = oracle();
    let scratch = Scratch::new();
    for f in o["seedFiles"].as_array().unwrap() {
        let p = scratch.0.join(f["relativePath"].as_str().unwrap());
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, expand(&f["content"], &scratch.0).as_str().unwrap()).unwrap();
    }
    for db in o["databases"].as_array().unwrap() {
        let p = scratch
            .0
            .join("workspaces")
            .join(db["id"].as_str().unwrap())
            .join("control.sqlite");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, STANDARD.decode(db["base64"].as_str().unwrap()).unwrap()).unwrap();
    }
    replay(&scratch.0, &o);
}
fn seed_rust(root: &Path, o: &Value) {
    let clock = PhaseClock::Frozen;
    let mut repos = repos(root, o, &clock);
    let mut vars = HashMap::new();
    for op in ordered_array("seed") {
        invoke(&mut repos, &op, root, &mut vars, &clock);
    }
    for r in repos.values() {
        r.close().unwrap();
    }
}
#[test]
fn rust_seed_and_replay_match_real_ts() {
    let o = oracle();
    let scratch = Scratch::new();
    seed_rust(&scratch.0, &o);
    assert_eq!(
        clean(&tree(&scratch.0), &scratch.0),
        o["seedFiles"],
        "Rust seed file tree"
    );
    replay(&scratch.0, &o);
}
#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn real_ts_reads_rust_databases_and_files() {
    let o = oracle();
    let scratch = Scratch::new();
    seed_rust(&scratch.0, &o);
    let baseline = std::env::var("PDRS_BASELINE").unwrap();
    let output_dir = Scratch::new();
    let output = output_dir.0.join("verify.json");
    let root = &scratch.0;
    let mut child = Command::new("node")
        .arg("--import")
        .arg(Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/edgeclaw-export/verify.mts"),
        )
        .arg(root)
        .arg(&output)
        .env("PDRS_BASELINE", baseline)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(45) {
            child.kill().unwrap();
            panic!("Node verifier exceeded 45s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let verify = fs::read_to_string(output).unwrap();
    let actual: Value = serde_json::from_str(&verify).unwrap();
    assert_eq!(actual["observations"], o["observations"]);
    assert_eq!(actual["files"], o["files"]);
    // The TS side's own JSON key order, compared as bytes.
    let parsed = parse_json(&verify).unwrap();
    let actual_observations = match parsed.get("observations") {
        Some(JsValue::Array(items)) => JsValue::Array(items.clone()),
        _ => panic!("verify.json is missing its observations array"),
    };
    assert_eq!(
        bytes(&actual_observations),
        bytes(&JsValue::Array(ordered_array("observations"))),
        "TS observations differ in serialized form"
    );
}
#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn fixture_regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-export");
}

// -- review fix round 1 (rv-codex-r1 P0 / rv-grok-r1 #1): import locking ----
//
// `importMemoryBundle` must hold both live stores' per-root operation locks
// across the directory swap, manifest repair and runtime reset: without them,
// a concurrent writer can rebuild the live root after it was moved to the
// backup, fail the staged rename with ENOTEMPTY and strand the pre-import
// tree in an orphaned `.memory.backup-*` directory.

mod import_locking {
    use super::*;
    use std::sync::{Condvar, Mutex, mpsc};

    /// A clock that parks only the thread that armed it — the concurrent
    /// writer sitting inside a store operation, whose first clock read is
    /// under that store's operation lock. Clock reads from other threads
    /// (the import's normalization and staging) proceed, so a blocked probe
    /// measures the lock wait, not staging work.
    #[derive(Clone)]
    struct GatedClock(Arc<GateInner>);
    struct GateInner {
        state: Mutex<GateState>,
        released: Condvar,
    }
    struct GateState {
        armed: Option<std::thread::ThreadId>,
        released: bool,
        parked: bool,
    }
    impl GatedClock {
        fn new() -> Self {
            Self(Arc::new(GateInner {
                state: Mutex::new(GateState {
                    armed: None,
                    released: false,
                    parked: false,
                }),
                released: Condvar::new(),
            }))
        }
        fn clock(&self) -> Arc<dyn Fn() -> String + Send + Sync> {
            let inner = self.0.clone();
            Arc::new(move || {
                let mut state = inner.state.lock().unwrap();
                if state.armed == Some(std::thread::current().id()) && !state.released {
                    state.parked = true;
                    while !state.released {
                        state = inner.released.wait(state).unwrap();
                    }
                }
                NOW.into()
            })
        }
        fn arm(&self) {
            let mut state = self.0.state.lock().unwrap();
            state.armed = Some(std::thread::current().id());
            state.released = false;
            state.parked = false;
        }
        fn is_parked(&self) -> bool {
            self.0.state.lock().unwrap().parked
        }
        fn release(&self) {
            let mut state = self.0.state.lock().unwrap();
            state.released = true;
            self.0.released.notify_all();
        }
    }
    /// A one-file bundle: after a successful import the workspace live root
    /// holds `Imported/marker.md`, which is what both probes look for.
    fn marker_bundle() -> JsValue {
        parse_json(
            r#"{"formatVersion":"clawxmemory-memory-snapshot.v4","scope":"current_project","exportedAt":"2026-10-22T00:00:00.000Z","files":[{"relativePath":"Imported/marker.md","content":"imported marker"}]}"#,
        )
        .unwrap()
    }
    fn writer_candidate() -> JsObject {
        parse_json(
            r#"{"type":"project","scope":"project","name":"parked writer","description":"parked writer description"}"#,
        )
        .unwrap()
        .as_object()
        .cloned()
        .unwrap()
    }
    /// Spawns the writer thread, waits until its upsert has parked on the
    /// gated clock inside the target store's operation lock.
    fn spawn_parked_writer<'scope, F>(
        scope: &'scope std::thread::Scope<'scope, '_>,
        gate: &GatedClock,
        upsert: F,
    ) -> std::thread::ScopedJoinHandle<'scope, ()>
    where
        F: FnOnce() + Send + 'scope,
    {
        let armed_gate = gate.clone();
        let handle = scope.spawn(move || {
            armed_gate.arm();
            upsert();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !gate.is_parked() {
            assert!(
                Instant::now() < deadline,
                "writer never reached a clock read inside its operation"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        handle
    }
    fn assert_no_swap_leftovers(memory_root: &Path) {
        let parent = memory_root.parent().unwrap();
        let leftovers: Vec<String> = fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".memory."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no orphaned import/backup directories may remain: {leftovers:?}"
        );
    }

    /// A writer parked inside a *workspace* store operation blocks the import
    /// before the directory swap: the probe below must time out, not observe
    /// a completed (or failed) import. `chflags uchg` on the live root makes
    /// the unsynchronized swap of the broken variant fail with a permission
    /// error instead of racing, so the probe discriminates deterministically.
    #[test]
    #[cfg(target_os = "macos")]
    fn import_waits_for_workspace_store_writer() {
        fn chflags(path: &Path, flags: &str) {
            let status = std::process::Command::new("chflags")
                .arg(flags)
                .arg(path)
                .status()
                .unwrap();
            assert!(status.success(), "chflags {flags} {}", path.display());
        }
        let scratch = Scratch::new();
        let root = scratch.0.clone();
        let gate = GatedClock::new();
        let repo = Mutex::new(open_with_clock(&root, "lock", "/fixture/w", gate.clock()));
        let bundle = marker_bundle();
        let memory_root = root.join("workspaces").join("lock").join("memory");
        let writer = FileMemoryStore::with_clock(
            &memory_root,
            FileMemoryStoreOptions::default(),
            gate.clock(),
        )
        .unwrap();
        let (tx, rx) = mpsc::channel();
        // Make the unsynchronized swap fail instead of silently racing.
        chflags(&memory_root, "uchg");
        std::thread::scope(|scope| {
            let writer = spawn_parked_writer(scope, &gate, || {
                writer
                    .execute("upsertCandidate", &writer_candidate())
                    .unwrap();
            });
            let importer = scope.spawn(|| {
                let repo = repo.lock().unwrap();
                tx.send(repo.import_memory_bundle(&bundle)).unwrap();
            });
            let probe = rx.recv_timeout(Duration::from_secs(1));
            chflags(&memory_root, "nouchg");
            gate.release();
            writer.join().unwrap();
            let completed_early = probe.is_ok();
            let imported = match probe {
                Err(_blocked) => rx.recv_timeout(Duration::from_secs(10)).unwrap(),
                // Completed while the writer still held the lock.
                Ok(early) => early,
            };
            importer.join().unwrap();
            assert!(
                !completed_early,
                "import finished while a writer held the workspace store's operation lock"
            );
            assert!(
                imported.is_ok(),
                "import after the writer released the lock failed: {:?}",
                imported.err()
            );
            assert_no_swap_leftovers(&memory_root);
        });
    }

    /// A writer parked inside a *global* store operation also blocks the
    /// import — the workspace live root must not even be swapped while that
    /// operation is in flight. The broken variant swaps the workspace root
    /// (its locks are not held) and only then blocks on the global lock
    /// through the manifest provider, so the marker file appearing in the
    /// live root is the deterministic red signal.
    #[test]
    fn import_waits_for_global_store_writer() {
        let scratch = Scratch::new();
        let root = scratch.0.clone();
        let gate = GatedClock::new();
        let repo = Mutex::new(open_with_clock(&root, "lock", "/fixture/w", gate.clock()));
        let bundle = marker_bundle();
        let memory_root = root.join("workspaces").join("lock").join("memory");
        let writer = FileMemoryStore::with_clock(
            root.join("global"),
            FileMemoryStoreOptions::default(),
            gate.clock(),
        )
        .unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let writer = spawn_parked_writer(scope, &gate, || {
                writer
                    .execute("upsertCandidate", &writer_candidate())
                    .unwrap();
            });
            let importer = scope.spawn(|| {
                let repo = repo.lock().unwrap();
                tx.send(repo.import_memory_bundle(&bundle)).unwrap();
            });
            // Give the import enough time to reach its blocking point; a
            // correct import cannot swap the live root at all while the
            // global store's operation lock is held.
            std::thread::sleep(Duration::from_secs(1));
            let swapped = memory_root.join("Imported").join("marker.md").exists();
            gate.release();
            writer.join().unwrap();
            let imported = rx.recv_timeout(Duration::from_secs(10)).unwrap();
            importer.join().unwrap();
            assert!(
                !swapped,
                "the workspace live root was swapped while a writer held the global store's operation lock"
            );
            assert!(
                imported.is_ok(),
                "import after the writer released the lock failed: {:?}",
                imported.err()
            );
            assert!(
                memory_root.join("Imported").join("marker.md").exists(),
                "the successful import must have installed the bundle's marker file"
            );
            assert_no_swap_leftovers(&memory_root);
        });
    }
}
