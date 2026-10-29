//! #778: real factory, M21 bundle/controller interleavings and TS storage interchange.
#![allow(clippy::duplicate_mod)]
#[path = "common/baseline.rs"]
mod baseline;
#[allow(dead_code)]
#[path = "support/llm_stub.rs"]
mod llm_stub;
use pilotdeck_gateway::{
    host::Cancellation,
    runtime::{
        context::memory::{MemoryCaptureTurnInput, MemoryRetrieveInput},
        memory::{
            MemoryCaptureStrategy, MemoryConfig, MemoryModelConfig, MemoryModelProvider,
            MemoryProviderError, MemoryScheduleConfig, NativeMemoryProviderOptions,
            NativeMemoryService, ProjectMemoryBundle, ProjectMemoryBundleOptions,
            ProjectMemoryResources,
            edgeclaw::{
                factory::{CreatedMemoryProvider, MemoryTelemetry, create_with_dependencies},
                llm::config::build_llm_config_with,
                service::{EdgeClawMemoryService, ServiceDependencies},
                text::hash_text,
            },
            maintenance::{
                MaintenanceRuntime, MaintenanceTelemetry, ProjectMemoryMaintenanceController,
            },
        },
        model::http::{HyperModelTransport, ModelHttpTransport, TransportFuture},
    },
};
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};
use tokio::sync::Semaphore;
const ORACLE: &str = include_str!("fixtures/edgeclaw-native-default/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";
const MS: u64 = 1792627200000;
fn js(v: &Value) -> JsValue {
    parse_json(&v.to_string()).expect("#778 fixture JSON")
}
fn enc(v: &JsValue) -> Value {
    match v {
        JsValue::Undefined => json!({"$undefined":true}),
        JsValue::Null => Value::Null,
        JsValue::Bool(v) => json!(v),
        JsValue::Number(_) => serde_json::from_str(
            &pilotdeck_protocol::stringify_json(v)
                .expect("#778 number encoding")
                .expect("#778 JSON number"),
        )
        .expect("#778 number"),
        JsValue::String(v) => json!(String::from_utf16_lossy(v.units())),
        JsValue::Array(v) => Value::Array(v.iter().map(enc).collect()),
        JsValue::Object(v) => Value::Object(
            v.entries()
                .iter()
                .map(|(k, v)| (String::from_utf16_lossy(k.units()), enc(v)))
                .collect(),
        ),
    }
}
fn oracle() -> Value {
    enc(&baseline::oracle(ORACLE))
}
fn string(v: &Value) -> Option<JsString> {
    v.as_str().map(JsString::from)
}
fn error(e: &MemoryProviderError) -> Value {
    let mut v = json!({"name":e.name,"message":String::from_utf16_lossy(e.message.units())});
    if let Some(code) = &e.code {
        v["code"] = json!(code)
    }
    v
}
fn compare(actual: &Value, expected: &Value, label: &str) {
    fn first(a: &Value, b: &Value, path: &str) -> Option<String> {
        if a == b {
            return None;
        }
        match (a, b) {
            (Value::Object(a), Value::Object(b)) => {
                for k in a.keys().chain(b.keys()) {
                    match (a.get(k), b.get(k)) {
                        (Some(a), Some(b)) => {
                            if let Some(diff) = first(a, b, &format!("{path}/{k}")) {
                                return Some(diff);
                            }
                        }
                        _ => return Some(format!("{path}/{k}: missing field")),
                    }
                }
                None
            }
            (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
                .iter()
                .zip(b)
                .enumerate()
                .find_map(|(i, (a, b))| first(a, b, &format!("{path}/{i}"))),
            _ => Some(format!("{path}: actual={a} expected={b}")),
        }
    }
    if let Some(dir) = std::env::var_os("PDRS_778_OBSERVATIONS") {
        let path = PathBuf::from(dir).join(label.replace(' ', "-") + ".json");
        fs::create_dir_all(path.parent().expect("#778 observations parent"))
            .expect("#778 observations directory");
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("#778 exclusive observation output");
        serde_json::to_writer_pretty(file, actual).expect("#778 write observation");
    }
    assert!(
        actual == expected,
        "#778 {label}: {}",
        first(actual, expected, "$").unwrap_or_default()
    );
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-778-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).expect("#778 create isolated root");
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
async fn bounded<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), f)
        .await
        .expect("#778 bounded operation completes")
}
fn clean(v: Value, root: &Path, workspaces: &[PathBuf], origin: &str) -> Value {
    match v {
        Value::String(mut s) => {
            for p in workspaces {
                s = s.replace(
                    &format!(
                        "/workspaces/{}",
                        hash_text(&JsString::from(p.to_string_lossy().as_ref()))
                    ),
                    "/workspaces/$HASH",
                );
            }
            if !origin.is_empty() {
                s = s.replace(origin, "$HTTP");
            }
            json!(s.replace(root.to_string_lossy().as_ref(), "$ROOT"))
        }
        Value::Array(v) => Value::Array(
            v.into_iter()
                .map(|v| clean(v, root, workspaces, origin))
                .collect(),
        ),
        Value::Object(v) => Value::Object(
            v.into_iter()
                .map(|(k, v)| (k, clean(v, root, workspaces, origin)))
                .collect(),
        ),
        v => v,
    }
}
fn input(root: &Path, spec: &Value) -> NativeMemoryProviderOptions {
    let config = spec.get("config").map(|c| {
        let mut out = MemoryConfig::new(
            c["enabled"] == true,
            string(&c["provider"]).unwrap_or_default(),
        );
        out.root_dir = c["rootDir"]
            .as_str()
            .unwrap_or("")
            .replace("$ROOT", root.to_string_lossy().as_ref())
            .into();
        out.capture_strategy = if c["captureStrategy"] == "full_session" {
            MemoryCaptureStrategy::FullSession
        } else {
            MemoryCaptureStrategy::LastTurn
        };
        out.include_assistant = c["includeAssistant"].as_bool().unwrap_or(true);
        out.max_message_chars = c["maxMessageChars"].as_f64();
        out.heartbeat_batch_size = c["heartbeatBatchSize"].as_f64();
        out.model = string(&c["model"]);
        out.api_type = string(&c["apiType"]);
        out.schedule = c.get("schedule").map(|s| MemoryScheduleConfig {
            reasoning_mode: string(&s["reasoningMode"]),
            auto_index_interval_minutes: s["autoIndexIntervalMinutes"].as_f64(),
            auto_dream_interval_minutes: s["autoDreamIntervalMinutes"].as_f64(),
        });
        out
    });
    let model_config = spec.get("modelConfig").map(|m| MemoryModelConfig {
        providers: m["providers"]
            .as_object()
            .expect("#778 model providers")
            .iter()
            .map(|(id, p)| {
                (
                    id.clone(),
                    MemoryModelProvider {
                        protocol: string(&p["protocol"]),
                        url: string(&p["url"]),
                        api_key: string(&p["apiKey"]),
                    },
                )
            })
            .collect(),
    });
    NativeMemoryProviderOptions {
        project_root: root.join("workspace"),
        config,
        model_config,
        agent_model: spec["agentModel"].as_str().map(str::to_owned),
        now: Some(Arc::new(|| UNIX_EPOCH + Duration::from_millis(MS))),
        telemetry: None,
    }
}
#[derive(Default)]
struct Random {
    next: usize,
    pending: Option<usize>,
}
fn dependencies(
    home: &Path,
    random: Arc<Mutex<Random>>,
    transport: Option<Arc<dyn ModelHttpTransport>>,
) -> ServiceDependencies {
    let math = random.clone();
    ServiceDependencies {
        home: Some(home.into()),
        clock: Some(Arc::new(|| NOW.into())),
        now_ms: Some(Arc::new(|| MS as i64)),
        math_random: Some(Arc::new(move || {
            let mut r = math.lock().expect("#778 random lock");
            let i = r.next % 8;
            r.next += 1;
            r.pending = Some(i);
            [0.25, 0.5, 0.75, 0.125, 0.875, 0.375, 0.625, 0.0625][i]
        })),
        random_token: Some(Arc::new(move || {
            let mut r = random.lock().expect("#778 token lock");
            let i = r.pending.take().unwrap_or_else(|| {
                let i = r.next % 8;
                r.next += 1;
                i
            });
            ["9", "i", "r", "4i", "vi", "di", "mi", "29"][i].into()
        })),
        transport,
        llm_config_resolver: Some(Arc::new(|options| {
            build_llm_config_with(options, None, &|_| None)
        })),
        ..Default::default()
    }
}
#[tokio::test]
async fn memory_native_default() {
    let root = Root::new();
    let mut options = ProjectMemoryBundleOptions::new(root.0.join("workspace"));
    let mut cfg = MemoryConfig::new(true, "edgeclaw");
    cfg.root_dir = root.0.join("store").to_string_lossy().as_ref().into();
    options.config = Some(cfg);
    let bundle = ProjectMemoryBundle::new(options);
    let resources = bundle.stage().expect("#778 native default stage");
    assert!(
        resources.memory.is_some()
            && resources.maintenance.is_some()
            && resources.management.is_some(),
        "#778 native default must publish real three handles"
    );
    let management = resources.management.expect("#778 management handle");
    management
        .clear("current_project")
        .expect("#778 real database is open");
    bounded(bundle.dispose()).await.expect("#778 default close");
    bounded(bundle.dispose())
        .await
        .expect("#778 repeated disposal does not close twice");
    assert_eq!(
        management
            .clear("current_project")
            .expect_err("#778 real database closes")
            .message,
        JsString::from("database is not open"),
        "#778 closed database witness"
    );
}
#[tokio::test]
async fn factory_config_matrix() {
    let root = Root::new();
    let expected = oracle();
    for cell in expected["configurations"].as_array().expect("#778 configs") {
        let name = cell["name"].as_str().expect("#778 config name");
        let dir = root.0.join("config").join(name);
        fs::create_dir_all(dir.join("workspace")).expect("#778 config workspace");
        let mut input = input(&dir, &cell["input"]);
        // Preserve the original UTF-16 input; enc() is only the JSON output
        // comparator and must not replace the lone surrogate before the factory.
        if name == "root-lone-surrogate" {
            let mut units = dir
                .join("root-")
                .to_string_lossy()
                .encode_utf16()
                .collect::<Vec<_>>();
            units.push(0xD800);
            input
                .config
                .as_mut()
                .expect("#778 surrogate config")
                .root_dir = JsString::from_utf16(units);
        }
        if cell["input"]["rootIsFile"] == true {
            fs::write(dir.join("not-a-directory"), "fixture occupies rootDir")
                .expect("#778 root file fixture");
        }
        let data = dir.join("store/workspaces").join(hash_text(&JsString::from(
            input.project_root.to_string_lossy().as_ref(),
        )));
        if let Some(fault) = cell["input"]["sqliteFault"].as_str() {
            fs::create_dir_all(&data).expect("#778 faulty SQLite directory");
            if fault == "directory" {
                fs::create_dir(data.join("control.sqlite")).expect("#778 SQLite directory fault");
            } else {
                fs::write(data.join("control.sqlite"), "not a database".repeat(256))
                    .expect("#778 corrupt SQLite fault");
            }
        }
        let deps = dependencies(&root.0.join("home"), Arc::default(), None);
        let created = create_with_dependencies(&input, deps);
        if let Some(expected_error) = cell.get("error") {
            let e = created.err().expect("#778 invalid root must fail");
            let e = e.factory_error().expect("#778 factory error shape");
            if let Some(expected_files) = cell.get("files") {
                let mut files = tree(&data.join("memory"), "memory");
                files.extend(tree(&dir.join("store/global"), "global"));
                assert_eq!(
                    json!(files),
                    *expected_files,
                    "#778 database failure must precede memory-file creation"
                );
            }
            assert_eq!(
                clean(error(e), &root.0, &[input.project_root], ""),
                *expected_error,
                "#778 factory error {name}"
            );
            continue;
        }
        let created = created.expect("#778 config construction");
        assert_eq!(
            created.is_some(),
            cell["created"] == true,
            "#778 config selection {name}"
        );
        if let Some(created) = created {
            assert_eq!(
                clean(
                    enc(&created.service.factory_configuration()),
                    &root.0,
                    &[input.project_root],
                    ""
                ),
                cell["service"],
                "#778 factory configuration {name}"
            );
            created.service.close().expect("#778 config close");
        }
    }
}
struct Events {
    items: Mutex<Vec<Value>>,
    done: Semaphore,
}
impl Default for Events {
    fn default() -> Self {
        Self {
            items: Mutex::new(vec![]),
            done: Semaphore::new(0),
        }
    }
}
impl Events {
    fn push(&self, v: Value) {
        self.items.lock().expect("#778 events").push(v);
    }
    fn snapshot(&self) -> Value {
        json!(*self.items.lock().expect("#778 events snapshot"))
    }
}
impl MaintenanceTelemetry for Events {
    fn track_feature_loop_stage(&self, input: JsValue) {
        let event = enc(&input);
        let done = event["phase"] == "maintenance";
        self.push(json!({"kind":"stage","event":event}));
        if done {
            self.done.add_permits(1)
        }
    }
    fn track_error(&self, e: &MemoryProviderError, input: JsValue) {
        self.push(json!({"kind":"error","error":error(e),"event":enc(&input)}));
    }
}
struct Gate {
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
    count: AtomicUsize,
    inner: HyperModelTransport,
}
impl ModelHttpTransport for Gate {
    fn send(&self, request: hyper::Request<http_body_util::Full<bytes::Bytes>>) -> TransportFuture {
        let pending = self.count.fetch_add(1, Ordering::SeqCst) == 0;
        let inner = self.inner.clone();
        let entered = self.entered.clone();
        let release = self.release.clone();
        TransportFuture::io(async move {
            if pending {
                entered.add_permits(1);
                release.acquire().await.expect("#778 gate release").forget();
            }
            inner.send(request).await
        })
    }
}
struct ObservedService {
    inner: Arc<EdgeClawMemoryService>,
    closes: AtomicUsize,
}
impl NativeMemoryService for ObservedService {
    fn run_due_scheduled_maintenance(
        &self,
        r: &str,
    ) -> pilotdeck_gateway::runtime::memory::MemoryFuture<'_, ()> {
        NativeMemoryService::run_due_scheduled_maintenance(self.inner.as_ref(), r)
    }
    fn list(&self, n: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        NativeMemoryService::list(self.inner.as_ref(), n)
    }
    fn clear(&self, s: &str) -> Result<(), MemoryProviderError> {
        NativeMemoryService::clear(self.inner.as_ref(), s)
    }
    fn clear_session(&self, s: &JsString) -> Result<(), MemoryProviderError> {
        NativeMemoryService::clear_session(self.inner.as_ref(), s)
    }
    fn close(&self) -> Result<(), MemoryProviderError> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        NativeMemoryService::close(self.inner.as_ref())
    }
}
struct Generation {
    observed: Arc<ObservedService>,
    bundle: ProjectMemoryBundle,
    resources: ProjectMemoryResources,
    created: CreatedMemoryProvider,
    workspace: PathBuf,
    store: PathBuf,
}
impl MaintenanceRuntime for Generation {
    fn project_root(&self) -> JsString {
        self.workspace.to_string_lossy().as_ref().into()
    }
    fn memory_service(
        &self,
    ) -> Option<Arc<dyn pilotdeck_gateway::runtime::memory::MemoryMaintenanceService>> {
        self.resources.maintenance.clone()
    }
}
fn generation(
    root: &Path,
    label: &str,
    origin: &str,
    events: Arc<Events>,
    random: Arc<Mutex<Random>>,
    transport: Option<Arc<dyn ModelHttpTransport>>,
) -> Arc<Generation> {
    let mut input = input(
        &root.join(label),
        &json!({"config":{"enabled":true,"provider":"edgeclaw","model":"fixture/model","rootDir":"$ROOT/store","schedule":{"autoIndexIntervalMinutes":1,"autoDreamIntervalMinutes":999999}},"modelConfig":{"providers":{"fixture":{"protocol":"openai","url":format!("{origin}/v1"),"apiKey":"fixture-key"}}}}),
    );
    input.project_root = root.join("old/workspace");
    input.telemetry = Some(Arc::new(MemoryTelemetry(events)));
    let deps = dependencies(&root.join("home"), random, transport);

    let created = create_with_dependencies(&input, deps)
        .expect("#778 real factory")
        .expect("#778 enabled factory");
    let native_provider = created.provider.clone();
    let observed = Arc::new(ObservedService {
        inner: created.service.clone(),
        closes: AtomicUsize::new(0),
    });
    let native_service = observed.clone();
    let mut options = ProjectMemoryBundleOptions::new(input.project_root.clone());
    options.create_provider = Some(Arc::new(move |_| {
        Ok(Some(
            pilotdeck_gateway::runtime::memory::NativeProjectMemoryProvider {
                provider: native_provider.clone(),
                service: native_service.clone(),
            },
        ))
    }));
    let bundle = ProjectMemoryBundle::new(options);
    let resources = bundle.stage().expect("#778 stage real factory product");
    Arc::new(Generation {
        observed,
        bundle,
        resources,
        created,
        workspace: input.project_root,
        store: root.join(label).join("store"),
    })
}
fn controller(
    current: Arc<Mutex<Arc<Generation>>>,
    events: Arc<Events>,
) -> ProjectMemoryMaintenanceController<Generation> {
    let diag = events.clone();
    ProjectMemoryMaintenanceController::new(
        Arc::new(move |_| Ok(current.lock().expect("#778 generation lookup").clone())),
        events,
        Some(Arc::new(move |message, error_value| {
            diag.push(json!({"kind":"diagnostic","message":String::from_utf16_lossy(message.units()),"error":error(error_value)}));
            diag.done.add_permits(1)
        })),
    )
}
async fn maintenance_done(events: &Events) {
    bounded(events.done.acquire())
        .await
        .expect("#778 maintenance result")
        .forget();
    tokio::task::yield_now().await;
}
fn retrieve(g: &Generation, signal: Cancellation, query: &str) -> super_future::Retrieve {
    g.created.provider.retrieve(MemoryRetrieveInput {
        query: query.into(),
        session_id: "session-old".into(),
        project_root: g.workspace.to_string_lossy().into_owned(),
        recent_messages: vec![],
        signal,
    })
}
mod super_future {
    pub type Retrieve =
        pilotdeck_gateway::runtime::memory::edgeclaw::provider::ProviderRetrieveFuture;
}
async fn capture(g: &Generation, session: &str) {
    let messages = js(
        &json!([{"role":"user","content":[{"type":"text","text":"Project Aurora must use Rust and retain the blue deployment rule."}]},{"role":"assistant","content":[{"type":"text","text":"I will retain the blue deployment rule for Project Aurora."}]}]),
    );
    let JsValue::Array(messages) = &messages else {
        panic!("#778 messages")
    };
    g.created
        .provider
        .capture_turn(MemoryCaptureTurnInput {
            session_id: session.into(),
            project_root: g.workspace.to_string_lossy().into_owned(),
            messages: messages.clone(),
            errored: JsValue::Bool(false),
        })
        .await
        .expect("#778 capture turn");
}
fn due(g: &Generation) {
    g.created
        .service
        .repository
        .storage
        .set_pipeline_state(
            "autoIndexAnchorAt",
            &JsValue::String("2026-10-21T23:58:00.000Z".into()),
        )
        .expect("#778 due maintenance anchor");
}
fn database(path: &Path) -> Value {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("#778 read SQLite");
    let tables=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").expect("#778 tables").query_map([],|r|r.get::<_,String>(0)).expect("#778 table rows").collect::<Result<Vec<_>,_>>().expect("#778 table names");
    let mut result = serde_json::Map::new();
    for table in tables {
        let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        let mut q = db
            .prepare(&format!("SELECT * FROM {}", quote(&table)))
            .expect("#778 table query");
        let names = q
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        q = db
            .prepare(&format!(
                "SELECT * FROM {} ORDER BY {}",
                quote(&table),
                names.iter().map(|s| quote(s)).collect::<Vec<_>>().join(",")
            ))
            .expect("#778 ordered rows");
        let rows = q
            .query_map([], |r| {
                let mut row = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    let value = match r.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(n) => json!(n),
                        rusqlite::types::ValueRef::Real(n) => json!(n),
                        rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        rusqlite::types::ValueRef::Blob(_) => panic!("#778 unexpected blob"),
                    };
                    row.insert(name.clone(), value);
                }
                Ok(Value::Object(row))
            })
            .expect("#778 SQLite rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("#778 SQLite read");
        result.insert(table, json!(rows));
    }
    Value::Object(result)
}
fn tree(root: &Path, prefix: &str) -> Vec<Value> {
    fn walk(root: &Path, path: &Path, prefix: &str, out: &mut Vec<Value>) {
        if !path.exists() {
            return;
        }
        let mut entries = fs::read_dir(path)
            .expect("#778 tree directory")
            .map(|e| e.expect("#778 tree entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(root, &p, prefix, out)
            } else {
                out.push(json!({"path":format!("{prefix}/{}",p.strip_prefix(root).expect("#778 tree relative").to_string_lossy()),"text":fs::read_to_string(&p).expect("#778 tree file")}))
            }
        }
    }
    let mut out = vec![];
    walk(root, root, prefix, &mut out);
    out
}
fn persisted(g: &Generation, root: &Path, origin: &str) -> Value {
    let mut files = tree(&g.created.service.memory_dir, "memory");
    files.extend(tree(&g.store.join("global"), "global"));
    clean(
        json!({"database":database(&g.created.service.db_path),"files":files}),
        root,
        std::slice::from_ref(&g.workspace),
        origin,
    )
}
async fn matrix(name: &str) {
    let root = Root::new();
    let expected = oracle()["scenarios"]
        .as_array()
        .expect("#778 scenarios")
        .iter()
        .find(|c| c["name"] == name)
        .expect("#778 registered M21 cell")
        .clone();
    let body=json!({"choices":[{"message":{"content":"{\"route\":\"none\",\"should_store\":false,\"labels\":[]}"}}]}).to_string();
    let server =
        llm_stub::LlmStub::start(vec![json!({"body":body})], Arc::new(AtomicU64::new(MS))).await;
    let events = Arc::new(Events::default());
    let random = Arc::new(Mutex::new(Random::default()));
    let gate = Arc::new(Gate {
        entered: Arc::new(Semaphore::new(0)),
        release: Arc::new(Semaphore::new(0)),
        count: AtomicUsize::new(0),
        inner: HyperModelTransport::default(),
    });
    let old = generation(
        &root.0,
        "old",
        &server.origin,
        events.clone(),
        random.clone(),
        Some(gate.clone()),
    );
    *random.lock().expect("#778 reset random") = Random::default();
    let current = Arc::new(Mutex::new(old.clone()));
    let c = controller(current.clone(), events.clone());
    let mut observed = json!({"name":name,"state":name.split('-').next().expect("#778 state")});
    let mut newer = None;
    if name.ends_with("-dispose") {
        let first = old.bundle.dispose();
        let same = if name == "disposed-dispose" {
            let first = bounded(first).await;
            let second = bounded(old.bundle.dispose()).await;
            let same = first == second;
            first.expect("#778 first dispose");
            second.expect("#778 repeat dispose");
            same
        } else {
            let second = old.bundle.dispose();
            let (first, second) = bounded(async { tokio::join!(first, second) }).await;
            let same = first == second;
            first.expect("#778 first dispose");
            second.expect("#778 concurrent repeat dispose");
            same
        };
        observed["samePromise"] = json!(same);
        observed["stageAfter"] = json!({"name":"Error","message":old.bundle.stage().expect_err("#778 stage once").to_string()});
    } else if name == "disposed-maintenance" {
        bounded(old.bundle.dispose()).await.expect("#778 dispose");
        c.schedule(None).expect("#778 schedule closed");
        maintenance_done(&events).await;
    } else if name == "disposed-retrieve" {
        bounded(old.bundle.dispose()).await.expect("#778 dispose");
        observed["result"] = enc(&bounded(retrieve(
            &old,
            Cancellation::default(),
            "remember the blue deployment rule",
        ))
        .await);
    } else {
        let maintenance = name.contains("maintenance");
        if maintenance {
            capture(&old, "session-old").await;
            due(&old);
        }
        let signal = Cancellation::default();
        let pending = if maintenance {
            c.schedule(None).expect("#778 schedule");
            None
        } else {
            Some(retrieve(
                &old,
                signal.clone(),
                "remember the blue deployment rule",
            ))
        };
        bounded(gate.entered.acquire())
            .await
            .expect("#778 LLM gate entered")
            .forget();
        observed["llmEntered"] = json!(true);
        if name.starts_with("generation-") || name == "never-maintenance" {
            if name == "never-maintenance" {
                bounded(old.bundle.dispose())
                    .await
                    .expect("#778 dispose never predecessor");
                observed["disposeCompletedBeforeRelease"] = json!(true);
            }
            let new = generation(
                &root.0,
                "new",
                &server.origin,
                events.clone(),
                random.clone(),
                None,
            );
            *current.lock().expect("#778 replace generation") = new.clone();
            capture(&new, "session-new").await;
            if maintenance {
                due(&new);
                c.schedule(None).expect("#778 successor schedule");
                maintenance_done(&events).await;
            }
            let before = database(&new.created.service.db_path);
            if name == "never-maintenance" {
                assert_eq!(
                    events.done.available_permits(),
                    0,
                    "#778 predecessor remains pending after successor completion"
                );
                observed["successorCompletedBeforePredecessor"] = json!(true);
                observed["newDatabase"] = before.clone();
            }
            gate.release.add_permits(1);
            if maintenance {
                maintenance_done(&events).await;
            } else {
                observed["result"] = enc(&bounded(pending.expect("#778 retrieve pending")).await);
                capture(&old, "session-old").await;
            }
            let after = database(&new.created.service.db_path);
            assert_eq!(
                after, before,
                "#778 old generation must not write new database"
            );
            if name != "never-maintenance" {
                observed["newDatabaseUnchanged"] = json!(true);
                observed["oldDatabase"] = database(&old.created.service.db_path);
                observed["newDatabase"] = after;
            }
            newer = Some(new);
        } else {
            if name == "cancelled-retrieve" {
                signal.cancel(Some("fixture parent cancelled".into()));
                observed["parentAborted"] = json!(true);
            }
            if name.starts_with("disposing-") || name == "cancelled-retrieve" {
                bounded(old.bundle.dispose())
                    .await
                    .expect("#778 close during await");
                observed["disposeCompletedBeforeRelease"] = json!(true);
            }
            gate.release.add_permits(1);
            if maintenance {
                maintenance_done(&events).await;
            } else {
                observed["result"] = enc(&bounded(pending.expect("#778 pending retrieve")).await);
            }
        }
    }
    bounded(old.bundle.dispose())
        .await
        .expect("#778 final old disposal");
    if let Some(new) = &newer {
        bounded(new.bundle.dispose())
            .await
            .expect("#778 final new disposal");
        observed["newCloseCount"] = json!(new.observed.closes.load(Ordering::SeqCst));
    }
    observed["closeCount"] = json!(old.observed.closes.load(Ordering::SeqCst));
    observed["events"] = events.snapshot();
    let requests = server.requests.lock().expect("#778 requests").clone();
    observed["requestCount"] = json!(requests.len());
    observed["requests"] = json!(requests);
    let observed = clean(
        observed,
        &root.0,
        std::slice::from_ref(&old.workspace),
        &server.origin,
    );
    server.close().await;
    compare(&observed, &expected, &format!("M21 {name}"));
}
macro_rules! cells {($($test:ident => $name:literal),* $(,)?)=>{
    const REGISTERED_M21: &[&str] = &[$($name),*];
    $(#[tokio::test] async fn $test(){matrix($name).await;})*
}}
#[test]
fn m21_denominator() {
    let expected = oracle();
    let expected = expected["scenarios"]
        .as_array()
        .expect("#778 M21 array")
        .iter()
        .map(|c| c["name"].clone())
        .collect::<Vec<_>>();
    let registered = if pilotdeck_gateway::mutants::on("native-missing-m21-cell") {
        &REGISTERED_M21[1..]
    } else {
        REGISTERED_M21
    };
    assert_eq!(
        json!(registered),
        json!(expected),
        "#778 Rust M21 registrations exactly match oracle"
    );
}
cells!(staged_dispose=>"staged-dispose",disposing_dispose=>"disposing-dispose",disposed_dispose=>"disposed-dispose",staged_maintenance=>"staged-maintenance",disposing_maintenance=>"disposing-maintenance",disposed_maintenance=>"disposed-maintenance",staged_retrieve=>"staged-retrieve",disposing_retrieve=>"disposing-retrieve",disposed_retrieve=>"disposed-retrieve",generation_maintenance=>"generation-maintenance",generation_retrieve=>"generation-retrieve",never_maintenance=>"never-maintenance",cancelled_retrieve=>"cancelled-retrieve");
#[test]
#[ignore]
fn edgeclaw_native_default_regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-native-default");
}

async fn end_to_end(root: &Path) -> Value {
    let expected = oracle()["endToEnd"].clone();
    let server = llm_stub::LlmStub::start(
        expected["script"]
            .as_array()
            .expect("#778 E2E script")
            .clone(),
        Arc::new(AtomicU64::new(MS)),
    )
    .await;
    let events = Arc::new(Events::default());
    let random = Arc::new(Mutex::new(Random::default()));
    let g = generation(
        root,
        "old",
        &server.origin,
        events.clone(),
        random.clone(),
        None,
    );
    *random.lock().expect("#778 E2E random") = Random::default();
    let c = controller(Arc::new(Mutex::new(g.clone())), events.clone());
    let first = enc(&bounded(retrieve(
        &g,
        Cancellation::default(),
        "do we already know the deployment rule?",
    ))
    .await);
    assert!(
        first.get("systemContext").is_none(),
        "#778 E2E first recall empty"
    );
    capture(&g, "session-old").await;
    due(&g);
    c.schedule(None).expect("#778 E2E schedule");
    maintenance_done(&events).await;
    let second = enc(&bounded(retrieve(
        &g,
        Cancellation::default(),
        "recall the retained Project Aurora deployment rule",
    ))
    .await);
    assert!(
        second["systemContext"]
            .as_str()
            .is_some_and(|s| s.contains("blue deployment rule")),
        "#778 E2E recall hits captured content"
    );
    let persistence = persisted(&g, root, &server.origin);
    let readback = readback(&g.created.service);
    bounded(g.bundle.dispose()).await.expect("#778 E2E dispose");
    let requests = server.requests.lock().expect("#778 E2E requests").clone();
    let result = clean(
        json!({"first":first,"second":second,"script":expected["script"],"requests":requests,"events":events.snapshot(),"persisted":persistence,"readback":readback,"closeCount":g.observed.closes.load(Ordering::SeqCst),"layout":expected["layout"]}),
        root,
        std::slice::from_ref(&g.workspace),
        &server.origin,
    );
    server.close().await;
    compare(&result, &expected, "E2E");
    result
}
fn readback(service: &EdgeClawMemoryService) -> Value {
    let entries = service
        .list(&pilotdeck_protocol::JsObject::new())
        .expect("#778 read entries");
    let JsValue::Array(entries) = &entries else {
        panic!("#778 entry array")
    };
    let ids = JsValue::Array(
        entries
            .iter()
            .map(|v| v.get("relativePath").expect("#778 entry path").clone())
            .collect(),
    );
    enc(&service.get(&ids, None).expect("#778 read memory records"))
}
#[tokio::test]
async fn memory_native_end_to_end() {
    let root = Root::new();
    end_to_end(&root.0).await;
}

#[tokio::test]
#[ignore]
async fn memory_native_bidirectional_storage() {
    use tokio::process::Command;
    let root = Root::new();
    let baseline = std::env::var("PDRS_BASELINE").expect("#778 PDRS_BASELINE required");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/edgeclaw-native-default");
    let ts = root.0.join("ts-generated/ts-store");
    let output = bounded_node(
        Command::new("node")
            .arg("--import")
            .arg(format!("{baseline}/node_modules/tsx/dist/loader.mjs"))
            .arg(fixture.join("generate.mts"))
            .arg(root.0.join("ts-generated")),
    )
    .await;
    assert!(
        output.status.success(),
        "#778 TS writer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    println!(
        "#778 TS writer: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let expected = oracle()["endToEnd"].clone();
    let events = Arc::new(Events::default());
    let g = generation(
        &ts,
        "old",
        "http://127.0.0.1:1",
        events,
        Arc::default(),
        None,
    );
    compare(
        &persisted(&g, &ts, ""),
        &expected["persisted"],
        "TS-to-Rust-database-files",
    );
    compare(
        &clean(
            readback(&g.created.service),
            &ts,
            std::slice::from_ref(&g.workspace),
            "",
        ),
        &expected["readback"],
        "TS-to-Rust-records",
    );
    bounded(g.bundle.dispose())
        .await
        .expect("#778 TS store Rust close");
    let rust = root.0.join("rust-store");
    fs::create_dir(&rust).expect("#778 Rust store root");
    end_to_end(&rust).await;
    let output = bounded_node(
        Command::new("node")
            .arg("--import")
            .arg(format!("{baseline}/node_modules/tsx/dist/loader.mjs"))
            .arg(fixture.join("verify.mts"))
            .arg("--read-root")
            .arg(&rust)
            .arg(root.0.join("ts-readback.json")),
    )
    .await;
    assert!(
        output.status.success(),
        "#778 TS reads Rust: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    println!(
        "#778 TS reader: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

async fn bounded_node(command: &mut tokio::process::Command) -> std::process::Output {
    command.kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(120), command.output())
        .await
        .expect("#778 Node source probe completes within 120 seconds")
        .expect("#778 launch Node source probe")
}

/// The default branch itself, through the handles it publishes. Deterministic
/// byte parity is checked separately above; this test keeps the real host clock.
#[tokio::test]
async fn memory_native_default_published_end_to_end() {
    let root = Root::new();
    let expected = oracle()["endToEnd"].clone();
    let server = llm_stub::LlmStub::start(
        expected["script"]
            .as_array()
            .expect("#778 default script")
            .clone(),
        Arc::new(AtomicU64::new(MS)),
    )
    .await;
    let input = input(
        &root.0,
        &json!({"config":{"enabled":true,"provider":"edgeclaw","rootDir":"$ROOT/store","model":"fixture/model","schedule":{"autoIndexIntervalMinutes":1,"autoDreamIntervalMinutes":999999}},"modelConfig":{"providers":{"fixture":{"protocol":"openai","url":format!("{}/v1",server.origin),"apiKey":"fixture-key"}}}}),
    );
    let mut options = ProjectMemoryBundleOptions::new(input.project_root.clone());
    options.config = input.config;
    options.model_config = input.model_config;
    options.now = input.now;
    let bundle = ProjectMemoryBundle::new(options);
    let resources = bundle.stage().expect("#778 default published stage");
    let memory = resources.memory.expect("#778 default published memory");
    let query = |text: &str| MemoryRetrieveInput {
        query: text.into(),
        session_id: "session-old".into(),
        project_root: input.project_root.to_string_lossy().into_owned(),
        recent_messages: vec![],
        signal: Cancellation::default(),
    };
    let first = bounded(memory.retrieve(query("do we already know the deployment rule?")))
        .await
        .expect("#778 default first recall");
    assert!(
        first.system_context.is_none(),
        "#778 default initially empty"
    );
    let JsValue::Array(ref messages) = js(
        &json!([{"role":"user","content":[{"type":"text","text":"Project Aurora must use Rust and retain the blue deployment rule."}]}]),
    ) else {
        panic!("#778 default messages")
    };
    bounded(memory.capture_turn(MemoryCaptureTurnInput {
        session_id: "session-old".into(),
        project_root: input.project_root.to_string_lossy().into_owned(),
        messages: messages.clone(),
        errored: JsValue::Bool(false),
    }))
    .await
    .expect("#778 default capture");
    // Provider's injected date is old; runDue's real service clock sees this
    // captured backlog as due without sleeping or changing the system clock.
    bounded(
        resources
            .maintenance
            .expect("#778 default maintenance")
            .run_due_scheduled_maintenance("capture"),
    )
    .await
    .expect("#778 default maintenance flush");
    let second =
        bounded(memory.retrieve(query("recall the retained Project Aurora deployment rule")))
            .await
            .expect("#778 default second recall");
    assert!(
        second
            .system_context
            .is_some_and(|s| s.contains("blue deployment rule")),
        "#778 default published handles retrieve captured memory"
    );
    let requests = server
        .requests
        .lock()
        .expect("#778 default requests")
        .clone();
    assert_eq!(
        requests.len(),
        5,
        "#778 default published flow uses all model steps"
    );
    for request in requests {
        assert_eq!(
            request["headers"]["authorization"], "Bearer fixture-key",
            "#778 default model credentials"
        );
        let body: Value =
            serde_json::from_str(request["body"].as_str().expect("#778 default request body"))
                .expect("#778 default body JSON");
        assert_eq!(body["model"], "model", "#778 default model selection");
    }
    bounded(bundle.dispose())
        .await
        .expect("#778 default E2E close");
    server.close().await;
}

#[tokio::test]
async fn service_get_maxlines_defaults_1052() {
    let root = Root::new();
    let expected = oracle()["maxLines"].clone();
    let input = input(
        &root.0,
        &json!({"config":{"enabled":true,"provider":"edgeclaw","rootDir":"$ROOT/store"}}),
    );
    let created = create_with_dependencies(
        &input,
        dependencies(&root.0.join("home"), Arc::default(), None),
    )
    .expect("#778 #1052 factory")
    .expect("#778 #1052 real provider");
    let candidate = js(
        &json!({"type":"project","scope":"project","name":"Max lines defaults","description":"81 lines","body":expected["body"]}),
    );
    let record = created
        .service
        .repository
        .workspace_memory
        .upsert_candidate(candidate.as_object().expect("#778 #1052 candidate"))
        .expect("#778 #1052 write 81-line record");
    let ids = JsValue::Array(vec![
        record
            .get("relativePath")
            .expect("#778 #1052 record path")
            .clone(),
    ]);
    let mut rows = vec![];
    for (name, limit) in [
        ("omitted", None),
        ("undefined", Some(JsValue::Undefined)),
        ("null", Some(JsValue::Null)),
    ] {
        let records = created
            .service
            .get(&ids, limit.as_ref())
            .expect("#778 #1052 get record");
        let JsValue::Array(records) = &records else {
            panic!("#778 #1052 record array")
        };
        assert_eq!(records.len(), 1, "#778 #1052 exactly one record");
        rows.push(json!({"name":name,"content":enc(records[0].get("content").expect("#778 #1052 content"))}));
    }
    compare(
        &json!({"body":expected["body"],"cases":rows}),
        &expected,
        "#1052 maxLines defaults",
    );
    created.service.close().expect("#778 #1052 close");
}
