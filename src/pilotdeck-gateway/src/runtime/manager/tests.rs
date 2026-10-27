//! U4 manager projection tests: real-TS oracle replay, catalog/writer shared
//! state, browser provider injection, and the "not registered" guard.
use super::catalog::{
    GatewaySessionCatalogConsumer, MANAGER_SESSION_LIMIT, SessionCatalogListInput,
    SessionCatalogPort, TranscriptSessionCatalog,
};
use super::projection::{
    ManagerBrowserPort, ManagerBrowsers, ManagerDispatch, ManagerError, ManagerGateway,
    ManagerProjectResolver, ManagerProjects, ManagerSessions,
};
use crate::storage::{
    AdapterResult, AsyncTranscriptAdapter, JsonlTranscriptStore, LedgerError, ProjectKey,
    SessionStorageInput, StoreResult, StoredSession, TranscriptKey, TranscriptStore,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, UNIX_EPOCH},
};

const ORACLE: &str = include_str!("../../../tests/fixtures/manager-gateway/oracle.json");
static UNDEFINED: JsValue = JsValue::Undefined;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let dir = std::env::temp_dir().join(format!(
                "pdrs-manager-934-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&dir) {
                Ok(()) => return Self(fs::canonicalize(dir).expect("#934 canonicalize scratch")),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("#934 create scratch: {error}"),
            }
        }
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn project(&self) -> PathBuf {
        let path = self.0.join("project");
        fs::create_dir_all(&path).expect("#934 create project");
        fs::canonicalize(path).expect("#934 canonicalize project")
    }
    fn home(&self) -> PathBuf {
        let path = self.0.join("home");
        fs::create_dir_all(&path).expect("#934 create home");
        fs::canonicalize(path).expect("#934 canonicalize home")
    }
    fn chat(&self) -> PathBuf {
        let path = self.0.join("chats");
        fs::create_dir_all(&path).expect("#934 create chats");
        fs::canonicalize(path).expect("#934 canonicalize chats")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn js(text: &str) -> JsValue {
    parse_json(text).expect("#934 parse test JSON")
}

fn item<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("#934 expected a JS string")
        .to_utf8()
        .expect("#934 expected UTF-8")
}

fn items(value: &JsValue) -> Vec<JsValue> {
    match value.get("items") {
        Some(JsValue::Array(items)) => items.clone(),
        other => panic!("#934 expected an items array, got {other:?}"),
    }
}

fn message(text: &str) -> JsValue {
    let mut block = JsObject::new();
    block.insert("type".into(), "text".into());
    block.insert("text".into(), text.into());
    let mut content = JsObject::new();
    content.insert(
        "content".into(),
        JsValue::Array(vec![JsValue::Object(block)]),
    );
    JsValue::Object(content)
}

fn append_session(
    adapter: &AsyncTranscriptAdapter,
    root: &Path,
    home: &Path,
    session_id: &str,
    prompt: &str,
) -> AdapterResult<()> {
    let session_id = JsString::from(session_id);
    let storage = adapter
        .create_session_storage(SessionStorageInput {
            project_root: root.to_path_buf(),
            pilot_home: home.to_path_buf(),
            session_id: session_id.clone(),
            now: Some(Arc::new(|| "2026-10-18T00:00:00.000Z".into())),
        })
        .expect("#934 create session storage");
    storage.transcript.record_accepted_input(
        &session_id,
        &JsString::from("turn-1"),
        vec![message(prompt)],
        None,
    )
}

fn adapter_for(store: Arc<TestStore>, chat: PathBuf) -> AsyncTranscriptAdapter {
    adapter_with_store(store, chat)
}

fn adapter_with_store(store: Arc<dyn TranscriptStore>, chat: PathBuf) -> AsyncTranscriptAdapter {
    AsyncTranscriptAdapter::new(store, Arc::new(move |_: &ProjectKey| chat.clone()))
}

fn manager_list(manager: &ManagerSessions, root: &Path, home: &Path) -> Vec<JsValue> {
    let value = manager
        .list(root, home, &JsValue::Undefined)
        .expect("#934 manager session list");
    items(&value)
}

// ─────────────────────────────────────────────── test store / gates

#[derive(Default)]
struct TestStore {
    inner: JsonlTranscriptStore,
    records: Mutex<Vec<StoredSession>>,
    gate: Mutex<Option<Arc<AppendGate>>>,
}

impl TestStore {
    fn set_gate(&self, gate: Arc<AppendGate>) {
        *self.gate.lock().expect("#934 lock gate slot") = Some(gate);
    }

    fn records(&self) -> Vec<StoredSession> {
        self.records.lock().expect("#934 lock records").clone()
    }

    fn set_records(&self, records: Vec<StoredSession>) {
        *self.records.lock().expect("#934 lock records") = records;
    }

    fn record_after_append(&self, key: &TranscriptKey) {
        let size = fs::metadata(&key.transcript_path)
            .map(|metadata| metadata.len() as f64)
            .ok();
        let mut records = self.records.lock().expect("#934 lock records");
        if let Some(record) = records
            .iter_mut()
            .find(|record| record.session_id == key.session_id)
        {
            record.file_size = size;
            record.last_modified += 1.0;
        } else {
            records.push(StoredSession {
                session_id: key.session_id.clone(),
                last_modified: 1_000.0,
                file_size: size,
            });
        }
    }
}

impl TranscriptStore for TestStore {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        let gate = self.gate.lock().expect("#934 lock gate slot").clone();
        if let Some(gate) = gate {
            gate.enter_and_wait()?;
        }
        self.inner.append(key, entry)?;
        self.record_after_append(key);
        Ok(())
    }

    fn read(
        &self,
        key: &TranscriptKey,
        max_bytes: Option<u64>,
    ) -> StoreResult<crate::storage::TranscriptRead> {
        self.inner.read(key, max_bytes)
    }

    fn list(&self, _key: &ProjectKey) -> Option<StoreResult<Vec<StoredSession>>> {
        Some(Ok(self.records()))
    }
}

/// A bounded writer gate: the writer signals entry, then waits for release.
/// Drop releases the gate so a failed test cannot leave a writer thread blocked.
struct AppendGate {
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<bool>,
    released: Condvar,
    fail: AtomicBool,
}

impl AppendGate {
    fn new(entered: Option<mpsc::Sender<()>>) -> Self {
        Self {
            entered: Mutex::new(entered),
            release: Mutex::new(false),
            released: Condvar::new(),
            fail: AtomicBool::new(false),
        }
    }

    fn enter_and_wait(&self) -> StoreResult<()> {
        if let Some(sender) = self.entered.lock().expect("#934 lock entered").take() {
            let _ = sender.send(());
        }
        let mut released = self.release.lock().expect("#934 lock release");
        while !*released {
            let (next, timeout) = self
                .released
                .wait_timeout(released, Duration::from_secs(10))
                .expect("#934 wait release");
            released = next;
            if timeout.timed_out() && !*released {
                return Err(LedgerError::Invalid(
                    "#934 writer gate did not receive a release".into(),
                ));
            }
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(LedgerError::Invalid(
                "#934 injected append cancellation".into(),
            ));
        }
        Ok(())
    }

    fn release(&self) {
        *self.release.lock().expect("#934 lock release") = true;
        self.released.notify_all();
    }

    fn cancel(&self) {
        self.fail.store(true, Ordering::SeqCst);
        self.release();
    }
}

impl Drop for AppendGate {
    fn drop(&mut self) {
        self.release();
    }
}

// ─────────────────────────────────────────────── oracle replay

struct ReplayResolver {
    root: PathBuf,
}

impl ManagerProjectResolver for ReplayResolver {
    fn resolve_project_key(&self, project_key: &[u16]) -> Result<PathBuf, ManagerError> {
        let key = String::from_utf16_lossy(project_key);
        if key == "<dir>" || key == self.root.to_string_lossy() {
            Ok(self.root.clone())
        } else {
            Err(ManagerError::unknown_project_key(project_key))
        }
    }
}

struct ReplayCatalog {
    behavior: JsValue,
    calls: Mutex<Vec<JsValue>>,
}

impl ReplayCatalog {
    fn new(behavior: JsValue) -> Self {
        Self {
            behavior,
            calls: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<JsValue> {
        self.calls.lock().expect("#934 lock catalog calls").clone()
    }
}

impl SessionCatalogPort for ReplayCatalog {
    fn list(&self, input: &SessionCatalogListInput) -> Result<Vec<JsValue>, ManagerError> {
        self.calls
            .lock()
            .expect("#934 lock catalog calls")
            .push(input.as_call_json());
        if let Some(JsValue::Array(items)) = self.behavior.get("items") {
            return Ok(items.clone());
        }
        let error = item(&self.behavior, "error");
        Err(replay_error(error))
    }
}

struct ReplayBrowsers {
    behavior: JsValue,
    calls: Mutex<Vec<JsValue>>,
}

impl ReplayBrowsers {
    fn new(behavior: JsValue) -> Self {
        Self {
            behavior,
            calls: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<JsValue> {
        self.calls.lock().expect("#934 lock browser calls").clone()
    }
}

impl ManagerBrowserPort for ReplayBrowsers {
    fn list(&self, input: &JsValue) -> Result<JsValue, ManagerError> {
        self.calls
            .lock()
            .expect("#934 lock browser calls")
            .push(input.clone());
        if let Some(result) = self.behavior.get("result") {
            return Ok(result.clone());
        }
        Err(replay_error(item(&self.behavior, "error")))
    }
}

fn replay_error(value: &JsValue) -> ManagerError {
    let name = value
        .get("name")
        .and_then(JsValue::as_string)
        .map(|name| name.to_utf8().unwrap_or_else(|_| "Error".into()))
        .unwrap_or_else(|| "Error".into());
    let code = value
        .get("code")
        .and_then(JsValue::as_string)
        .and_then(|code| code.to_utf8().ok());
    let message = value
        .get("message")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    ManagerError::with_message(name, code, message)
}

fn replace_dir(value: &JsValue, root: &Path) -> JsValue {
    match value {
        JsValue::String(text) => {
            let text =
                String::from_utf16_lossy(text.units()).replace("<dir>", &root.to_string_lossy());
            JsValue::from(text.as_str())
        }
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(|item| replace_dir(item, root)).collect())
        }
        JsValue::Object(object) => {
            let mut out = JsObject::new();
            for (key, value) in object.entries() {
                out.insert(key.clone(), replace_dir(value, root));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn normalize_dir(value: &JsValue, root: &Path) -> JsValue {
    match value {
        JsValue::String(text) => {
            let text = String::from_utf16_lossy(text.units())
                .replace(&root.to_string_lossy().to_string(), "<dir>");
            JsValue::from(text.as_str())
        }
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(|item| normalize_dir(item, root)).collect())
        }
        JsValue::Object(object) => {
            let mut out = JsObject::new();
            for (key, value) in object.entries() {
                out.insert(key.clone(), normalize_dir(value, root));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn compact_str(value: &JsValue) -> String {
    stringify_json(value)
        .expect("#934 stringify replay value")
        .unwrap_or_default()
}

#[test]
fn manager_sessions_matches_real_ts_projection() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    let root = scratch.project();
    let operations = match item(&oracle, "operations") {
        JsValue::Array(operations) => operations,
        _ => panic!("#934 oracle operations must be an array"),
    };
    assert_eq!(
        operations.len(),
        18,
        "#934 manager gateway fixture operation count is pinned"
    );
    let mut methods = 0;
    for operation in operations {
        let name = text(item(operation, "name"));
        let params = replace_dir(item(operation, "params"), &root);
        let catalog = Arc::new(ReplayCatalog::new(item(operation, "catalog").clone()));
        let browser_behavior = item(operation, "browser");
        let replay_browsers = match browser_behavior {
            JsValue::String(value) if value.equals_str("absent") => None,
            _ => Some(Arc::new(ReplayBrowsers::new(browser_behavior.clone()))),
        };
        let browsers = replay_browsers
            .as_ref()
            .map(|browsers| browsers.clone() as Arc<dyn ManagerBrowserPort>);
        let gateway = ManagerGateway::from_parts(
            Some(catalog.clone() as Arc<dyn SessionCatalogPort>),
            browsers,
            ReplayResolver { root: root.clone() },
            root.clone(),
            root.clone(),
        );
        let method = text(item(operation, "method"));
        let result = if method == "manager_sessions" {
            gateway.sessions(&params)
        } else {
            gateway.browsers(&params)
        };
        let actual = match result {
            Ok(value) => {
                let mut out = JsObject::new();
                out.insert("ok".into(), value);
                JsValue::Object(out)
            }
            Err(error) => {
                let mut out = JsObject::new();
                out.insert("error".into(), error.as_json());
                JsValue::Object(out)
            }
        };
        assert_eq!(
            compact_str(&normalize_dir(&actual, &root)),
            compact_str(item(operation, "outcome")),
            "#934 manager gateway oracle replay differs at {name}"
        );
        assert_eq!(
            compact_str(&normalize_dir(&JsValue::Array(catalog.calls()), &root)),
            compact_str(item(operation, "catalogCalls")),
            "#934 manager catalog calls differ at {name}"
        );
        let browser_calls = replay_browsers
            .as_ref()
            .map(|browser| browser.calls())
            .unwrap_or_default();
        assert_eq!(
            compact_str(&JsValue::Array(
                browser_calls
                    .iter()
                    .map(|call| normalize_dir(call, &root))
                    .collect()
            )),
            compact_str(item(operation, "browserCalls")),
            "#934 manager browser calls differ at {name}"
        );
        methods += 1;
    }
    assert_eq!(methods, 18, "#934 every oracle operation is replayed");
}

#[test]
#[ignore = "requires PDRS_BASELINE at the fixed TS commit"]
fn manager_gateway_fixture_regenerates_identically() {
    crate::test_baseline::regenerates_identically("manager-gateway");
}

// ─────────────────────────────────────────────── catalog / projection

struct RecordingCatalog {
    input: Mutex<Option<SessionCatalogListInput>>,
    items: Vec<JsValue>,
}

impl SessionCatalogPort for RecordingCatalog {
    fn list(&self, input: &SessionCatalogListInput) -> Result<Vec<JsValue>, ManagerError> {
        *self.input.lock().expect("#934 lock catalog input") = Some(input.clone());
        Ok(self.items.clone())
    }
}

#[test]
fn manager_catalog_consumer_requests_ts_limit() {
    let catalog = Arc::new(RecordingCatalog {
        input: Mutex::new(None),
        items: Vec::new(),
    });
    let consumer = GatewaySessionCatalogConsumer::new(catalog.clone());
    consumer
        .list_manager_sessions("/project", "/home")
        .expect("#934 manager catalog list");
    let input = catalog
        .input
        .lock()
        .expect("#934 lock catalog input")
        .clone()
        .expect("#934 catalog was called");
    assert_eq!(
        input.limit,
        Some(MANAGER_SESSION_LIMIT),
        "#934 manager catalog must request TS limit 100"
    );
    assert_eq!(
        input.offset,
        Some(0.0),
        "#934 manager catalog must request offset 0"
    );
    assert!(
        !input.include_internal,
        "#934 manager catalog must not include internal sessions"
    );
}

fn number(value: &JsValue) -> f64 {
    value.as_number().expect("#934 expected a JS number")
}

fn records_from_chat(chat: &Path) -> Vec<StoredSession> {
    let mut records: Vec<StoredSession> = fs::read_dir(chat)
        .expect("#934 read matrix chat")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let session_id = name.strip_suffix(".jsonl")?.to_owned();
            let metadata = entry.metadata().ok()?;
            let modified = metadata
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis() as f64;
            Some(StoredSession {
                session_id: JsString::from(session_id),
                last_modified: modified,
                file_size: Some(metadata.len() as f64),
            })
        })
        .collect();
    records.sort_by(|left, right| {
        right
            .last_modified
            .partial_cmp(&left.last_modified)
            .expect("#934 matrix mtimes are finite")
    });
    records
}

fn set_mtime(path: &Path, millis: f64) {
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("#934 open matrix file for mtime");
    file.set_modified(UNIX_EPOCH + Duration::from_millis(millis as u64))
        .expect("#934 set matrix mtime");
}

fn session_ids(value: &JsValue) -> Vec<String> {
    match value {
        JsValue::Array(items) => items
            .iter()
            .map(|item| text(item.get("sessionId").unwrap_or(&UNDEFINED)))
            .collect(),
        other => panic!("#934 expected session array, got {other:?}"),
    }
}

#[test]
fn manager_catalog_matrix_matches_real_ts() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let matrix = match item(&oracle, "matrix") {
        JsValue::Array(rows) => rows,
        _ => panic!("#934 oracle matrix must be an array"),
    };
    assert_eq!(matrix.len(), 8, "#934 catalog/writer matrix is pinned");
    for row in matrix {
        let name = text(item(row, "name"));
        let scratch = Scratch::new();
        let root = scratch.project();
        let home = scratch.home();
        let chat = scratch.chat();
        if let JsValue::Array(files) = item(row, "files") {
            for file in files {
                let path = chat.join(text(item(file, "name")));
                fs::write(&path, text(item(file, "content")))
                    .expect("#934 write matrix transcript");
                set_mtime(&path, number(item(file, "mtime")));
            }
        }
        let store = Arc::new(TestStore::default());
        store.set_records(records_from_chat(&chat));
        let catalog = TranscriptSessionCatalog::new(Arc::new(adapter_for(store, chat)));
        let input = SessionCatalogListInput {
            project_root: root,
            pilot_home: home,
            limit: item(row, "limit").as_number(),
            offset: item(row, "offset").as_number(),
            include_internal: matches!(item(row, "includeInternal"), JsValue::Bool(true)),
        };
        let actual = catalog.list(&input).expect("#934 matrix catalog list");
        let expected = item(row, "outcome");
        assert_eq!(
            session_ids(&JsValue::Array(actual)),
            session_ids(expected),
            "#934 catalog matrix differs from real TS at {name}"
        );
    }
}

#[test]
fn manager_sessions_uses_catalog_consumer_not_direct_transcript() {
    let scratch = Scratch::new();
    let root = scratch.project();
    let home = scratch.home();
    let chat = scratch.chat();
    let store = Arc::new(TestStore::default());
    let adapter = Arc::new(adapter_for(store.clone(), chat));
    append_session(&adapter, &root, &home, "normal", "visible")
        .expect("#934 append normal session");
    append_session(&adapter, &root, &home, "always-on-discovery:1", "internal")
        .expect("#934 append internal session");
    let catalog = Arc::new(TranscriptSessionCatalog::new(adapter.clone()));
    let manager = ManagerSessions::new(catalog.clone());
    let listed = manager_list(&manager, &root, &home);
    assert_eq!(
        listed.len(),
        1,
        "#934 catalog consumer must filter internal sessions before pagination"
    );
    assert_eq!(
        text(item(&listed[0], "sessionId")),
        "normal",
        "#934 catalog consumer must return the non-internal session"
    );

    let all = catalog
        .list(&SessionCatalogListInput {
            project_root: root.clone(),
            pilot_home: home.clone(),
            limit: None,
            offset: None,
            include_internal: true,
        })
        .expect("#934 catalog includeInternal list");
    assert_eq!(
        all.len(),
        2,
        "#934 includeInternal must expose internal sessions"
    );
}

#[test]
fn manager_catalog_filters_before_pagination() {
    let scratch = Scratch::new();
    let root = scratch.project();
    let home = scratch.home();
    let chat = scratch.chat();
    let store = Arc::new(TestStore::default());
    let adapter = Arc::new(adapter_for(store, chat));
    append_session(&adapter, &root, &home, "normal", "visible")
        .expect("#934 append normal session");
    append_session(&adapter, &root, &home, "always-on-workspace-2", "internal")
        .expect("#934 append internal session");
    let catalog = TranscriptSessionCatalog::new(adapter);
    let page = catalog
        .list(&SessionCatalogListInput {
            project_root: root,
            pilot_home: home,
            limit: Some(1.0),
            offset: Some(0.0),
            include_internal: false,
        })
        .expect("#934 paged catalog list");
    assert_eq!(
        page.len(),
        1,
        "#934 internal filtering must happen before pagination"
    );
    assert_eq!(
        text(item(&page[0], "sessionId")),
        "normal",
        "#934 pagination must not consume the limit with an internal session"
    );
}

struct EmptyStore;
impl TranscriptStore for EmptyStore {
    fn append(&self, _key: &TranscriptKey, _entry: &JsValue) -> StoreResult<()> {
        Ok(())
    }
    fn read(
        &self,
        _key: &TranscriptKey,
        _max_bytes: Option<u64>,
    ) -> StoreResult<crate::storage::TranscriptRead> {
        Err(LedgerError::Invalid("#934 unavailable store read".into()))
    }
}

#[test]
fn manager_catalog_reports_unavailable_store() {
    let scratch = Scratch::new();
    let chat = scratch.chat();
    let adapter = Arc::new(adapter_with_store(Arc::new(EmptyStore), chat));
    let catalog = TranscriptSessionCatalog::new(adapter);
    let error = catalog
        .list(&SessionCatalogListInput {
            project_root: scratch.project(),
            pilot_home: scratch.home(),
            limit: Some(100.0),
            offset: Some(0.0),
            include_internal: false,
        })
        .expect_err("#934 a store without list must not become an empty catalog");
    assert_eq!(
        error.name(),
        "ProjectSessionCatalogUnavailableError",
        "#934 unavailable catalog name"
    );
    assert_eq!(
        String::from_utf16_lossy(error.message().units()),
        "The selected project session storage provider does not support session catalog reads.",
        "#934 unavailable catalog message"
    );
}

#[test]
fn manager_browsers_uses_injected_provider() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    struct Provider {
        calls: Arc<Mutex<Vec<JsValue>>>,
    }
    impl ManagerBrowserPort for Provider {
        fn list(&self, input: &JsValue) -> Result<JsValue, ManagerError> {
            self.calls
                .lock()
                .expect("#934 lock browser provider calls")
                .push(input.clone());
            Ok(js(
                r#"{"items":[{"browserId":"browser-1","state":"running"}]}"#,
            ))
        }
    }
    let browsers = ManagerBrowsers::new(Arc::new(Provider {
        calls: calls.clone(),
    }));
    let input = js(r#"{"projectKey":"/project","sessionKey":"s-1"}"#);
    let result = browsers
        .list(&input)
        .expect("#934 browser provider success path must return its items");
    assert_eq!(
        items(&result).len(),
        1,
        "#934 browser provider success path must not be unavailable"
    );
    assert_eq!(
        calls.lock().expect("#934 lock calls").len(),
        1,
        "#934 browser provider must receive the raw manager input"
    );
    assert_eq!(
        compact_str(&result),
        r#"{"items":[{"browserId":"browser-1","state":"running"}]}"#,
        "#934 browser provider result must be returned unchanged"
    );

    let unavailable = ManagerBrowsers::unavailable()
        .list(&input)
        .expect_err("#934 absent browser provider must fail");
    assert_eq!(
        unavailable.code(),
        Some("CAPABILITY_UNAVAILABLE"),
        "#934 absent browser provider code"
    );
    assert_eq!(
        String::from_utf16_lossy(unavailable.message().units()),
        "Gateway manager browser provider is unavailable.",
        "#934 absent browser provider message"
    );
}

#[test]
fn manager_sessions_unavailable_provider_matches_ts() {
    let error = ManagerSessions::unavailable()
        .list(Path::new("/project"), Path::new("/home"), &UNDEFINED)
        .expect_err("#934 absent session provider must fail");
    assert_eq!(
        error.code(),
        Some("CAPABILITY_UNAVAILABLE"),
        "#934 absent session provider code"
    );
    assert_eq!(
        String::from_utf16_lossy(error.message().units()),
        "Gateway manager session provider is unavailable.",
        "#934 absent session provider message"
    );
}

#[test]
fn manager_error_projection_matches_ts() {
    let dialog = ManagerError::new(
        "DialogGatewayError",
        Some("CATALOG_DOWN".into()),
        "catalog offline",
    );
    assert_eq!(
        compact_str(&dialog.as_json()),
        r#"{"name":"DialogGatewayError","code":"CATALOG_DOWN","message":"catalog offline"}"#,
        "#934 provider error code must be preserved"
    );
    let raw = ManagerError::plain("TypeError", "catalog exploded");
    assert_eq!(
        compact_str(&raw.as_json()),
        r#"{"name":"TypeError","message":"catalog exploded"}"#,
        "#934 raw provider error must keep its name and omit the code"
    );
    let unavailable = ManagerError::catalog_unavailable();
    assert_eq!(
        unavailable.code(),
        None,
        "#934 unavailable catalog error has no wire code"
    );
}

#[test]
fn manager_methods_stay_outside_default_dispatch() {
    for method in ["manager_sessions", "manager_browsers"] {
        let request = js(&format!(
            r#"{{"type":"request","id":"1","method":"{method}","params":{{}}}}"#
        ));
        assert!(
            !crate::dispatch::is_local(&request),
            "#934 {method} must not join LOCAL_METHODS in this package"
        );
    }
    let gateway = ManagerGateway::from_parts(
        Some(Arc::new(RecordingCatalog {
            input: Mutex::new(None),
            items: Vec::new(),
        }) as Arc<dyn SessionCatalogPort>),
        None,
        ReplayResolver {
            root: PathBuf::from("/project"),
        },
        PathBuf::from("/project"),
        PathBuf::from("/project"),
    );
    assert!(
        matches!(
            gateway.dispatch("manager_sessions", &js(r#"{"projectKey":"<dir>"}"#)),
            Some(ManagerDispatch::Response(_))
        ),
        "#934 known manager methods route inside the unwired component"
    );
    assert!(
        gateway
            .dispatch("unknown_manager_method", &js("{}"))
            .is_none(),
        "#934 unknown methods stay forwardable"
    );
}

#[test]
fn manager_projects_keeps_unknown_key_units() {
    let scratch = Scratch::new();
    let resolver = ManagerProjects::new(scratch.home(), scratch.path());
    let key = [0x2f, 0x78, 0xd800];
    let error = resolver
        .resolve_project_key(&key)
        .expect_err("#934 unknown project key must fail");
    assert_eq!(
        error.code(),
        Some("PROJECT_NOT_FOUND"),
        "#934 unknown key code"
    );
    let expected = {
        let mut units = "Unknown projectKey: ".encode_utf16().collect::<Vec<_>>();
        units.extend_from_slice(&key);
        units
    };
    assert_eq!(
        error.message().units(),
        expected.as_slice(),
        "#934 unknown key message must keep raw UTF-16 units"
    );
}

// ─────────────────────────────────────────────── catalog/writer shared state

#[test]
fn manager_catalog_shares_state_with_writer() {
    let scratch = Scratch::new();
    let root = scratch.project();
    let home = scratch.home();
    let chat = scratch.chat();
    let store = Arc::new(TestStore::default());
    let adapter = Arc::new(adapter_for(store, chat));
    let manager = ManagerSessions::new(Arc::new(TranscriptSessionCatalog::new(adapter.clone())));
    assert_eq!(
        manager_list(&manager, &root, &home).len(),
        0,
        "#934 empty catalog reports no sessions"
    );
    append_session(&adapter, &root, &home, "session-1", "shared state")
        .expect("#934 append shared session");
    let listed = manager_list(&manager, &root, &home);
    assert_eq!(
        listed.len(),
        1,
        "#934 catalog sees the committed writer state"
    );
    assert_eq!(
        text(item(&listed[0], "summary")),
        "shared state",
        "#934 catalog summary comes from the committed transcript"
    );
}

#[test]
fn manager_catalog_read_does_not_wait_for_blocked_writer() {
    let scratch = Scratch::new();
    let root = scratch.project();
    let home = scratch.home();
    let chat = scratch.chat();
    let store = Arc::new(TestStore::default());
    let adapter = Arc::new(adapter_for(store.clone(), chat));
    let manager = Arc::new(ManagerSessions::new(Arc::new(
        TranscriptSessionCatalog::new(adapter.clone()),
    )));
    let (entered_tx, entered_rx) = mpsc::channel();
    let gate = Arc::new(AppendGate::new(Some(entered_tx)));
    store.set_gate(gate.clone());

    let writer_adapter = adapter.clone();
    let writer_root = root.clone();
    let writer_home = home.clone();
    let writer = thread::spawn(move || {
        append_session(
            &writer_adapter,
            &writer_root,
            &writer_home,
            "session-blocked",
            "blocked",
        )
    });
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("#934 writer must reach the append gate within the bound");

    let (result_tx, result_rx) = mpsc::channel();
    let reader_manager = manager.clone();
    let reader_root = root.clone();
    let reader_home = home.clone();
    thread::spawn(move || {
        let _ = result_tx.send(manager_list(&reader_manager, &reader_root, &reader_home));
    });
    let before = match result_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(value) => value,
        Err(error) => {
            gate.release();
            panic!("#934 catalog read must not wait for a blocked writer: {error}");
        }
    };
    assert_eq!(
        before.len(),
        0,
        "#934 blocked writer must not expose an uncommitted session"
    );
    gate.release();
    writer
        .join()
        .expect("#934 writer thread must join")
        .expect("#934 blocked writer must commit after release");
    assert_eq!(
        manager_list(&manager, &root, &home).len(),
        1,
        "#934 catalog sees the session after the writer commits"
    );
}

#[test]
fn manager_catalog_after_cancelled_writer_keeps_committed_prefix() {
    let scratch = Scratch::new();
    let root = scratch.project();
    let home = scratch.home();
    let chat = scratch.chat();
    let store = Arc::new(TestStore::default());
    let adapter = Arc::new(adapter_for(store.clone(), chat));
    let manager = ManagerSessions::new(Arc::new(TranscriptSessionCatalog::new(adapter.clone())));
    append_session(&adapter, &root, &home, "session-1", "committed")
        .expect("#934 append the committed prefix");

    let (entered_tx, entered_rx) = mpsc::channel();
    let gate = Arc::new(AppendGate::new(Some(entered_tx)));
    store.set_gate(gate.clone());
    let writer_adapter = adapter.clone();
    let writer_root = root.clone();
    let writer_home = home.clone();
    let writer = thread::spawn(move || {
        append_session(
            &writer_adapter,
            &writer_root,
            &writer_home,
            "session-2",
            "cancelled",
        )
    });
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("#934 cancelled writer must reach the append gate");
    gate.cancel();
    let cancelled = writer.join().expect("#934 cancelled writer must join");
    assert!(
        cancelled.is_err(),
        "#934 cancelled writer must report its injected failure"
    );
    let listed = manager_list(&manager, &root, &home);
    assert_eq!(
        listed.len(),
        1,
        "#934 cancelled writer must leave the committed prefix unchanged"
    );
    assert_eq!(
        text(item(&listed[0], "sessionId")),
        "session-1",
        "#934 cancelled writer must not add a half-committed session"
    );
}
