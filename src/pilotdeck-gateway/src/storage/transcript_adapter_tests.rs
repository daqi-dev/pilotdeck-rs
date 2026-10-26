//! AsyncTranscriptStorageAdapter contract tests. Scenario outcomes, store
//! calls and on-disk bytes come from the real TS adapter
//! (tests/fixtures/transcript-adapter/generate.mts).
use super::ledger::field;
use super::*;
use crate::metadata::object;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/transcript-adapter"
);
const NOW: &str = "2026-10-18T00:00:00.000Z";
const PROJECT_ROOT: &str = "/pdrs-fixture/project";

fn oracle() -> JsValue {
    let path = std::env::var_os("PDRS_TRANSCRIPT_ADAPTER_ORACLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{FIXTURES}/oracle.json")));
    let text = fs::read_to_string(path).unwrap();
    assert!(
        !text.contains(".generate-"),
        "oracle.json leaks a generation path"
    );
    crate::test_baseline::oracle(&text)
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("pdrs-transcript-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn s(v: &JsValue) -> String {
    String::from_utf16_lossy(v.as_string().expect("string").units())
}
fn js(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
fn path_value(path: &Path) -> JsValue {
    JsValue::String(path.display().to_string().into())
}
fn clock() -> Clock {
    Arc::new(|| NOW.to_string())
}
fn key_value(key: &TranscriptKey) -> JsValue {
    object([
        ("projectRoot", path_value(&key.project_root)),
        ("pilotHome", path_value(&key.pilot_home)),
        ("sessionId", JsValue::String(key.session_id.clone())),
        ("transcriptPath", path_value(&key.transcript_path)),
    ])
}

/// Rust twin of the generator's fixtureStore: JsonlTranscriptStore plus a call
/// log, injected failures and only the configured capabilities.
#[derive(Default)]
struct FaultStore {
    inner: JsonlTranscriptStore,
    calls: Mutex<Vec<JsValue>>,
    appends: AtomicUsize,
    reads: AtomicUsize,
    caps: HashSet<String>,
    fail_before: HashSet<usize>,
    fail_after: HashSet<usize>,
    fail_read: HashSet<usize>,
    list: Option<Vec<StoredSession>>,
}
fn numbers(config: &JsValue, key: &str) -> HashSet<usize> {
    match field(config, key) {
        JsValue::Array(items) => items
            .iter()
            .map(|n| n.as_number().unwrap() as usize)
            .collect(),
        _ => HashSet::new(),
    }
}
impl FaultStore {
    fn from_config(config: &JsValue) -> Self {
        Self {
            caps: match field(config, "caps") {
                JsValue::Array(items) => items.iter().map(s).collect(),
                _ => HashSet::new(),
            },
            fail_before: numbers(config, "failBefore"),
            fail_after: numbers(config, "failAfter"),
            fail_read: numbers(config, "failRead"),
            list: match field(config, "list") {
                JsValue::Array(records) => Some(
                    records
                        .iter()
                        .map(|r| StoredSession {
                            session_id: field(r, "sessionId").as_string().unwrap().clone(),
                            last_modified: field(r, "lastModified").as_number().unwrap(),
                            file_size: field(r, "fileSize").as_number(),
                        })
                        .collect(),
                ),
                _ => None,
            },
            ..Self::default()
        }
    }
    fn log(&self, method: &str, key: JsValue, extra: Option<(&'static str, JsValue)>) {
        let mut call = JsObject::new();
        call.insert("method".into(), method.into());
        call.insert("key".into(), key);
        if let Some((name, value)) = extra {
            call.insert(name.into(), value);
        }
        self.calls.lock().unwrap().push(JsValue::Object(call));
    }
    fn cap(&self, name: &str) -> bool {
        self.caps.contains(name)
    }
    fn calls(&self) -> Vec<JsValue> {
        self.calls.lock().unwrap().clone()
    }
}
impl TranscriptStore for FaultStore {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        let n = self.appends.fetch_add(1, Ordering::SeqCst) + 1;
        self.log("append", key_value(key), Some(("entry", entry.clone())));
        if self.fail_before.contains(&n) {
            return Err(LedgerError::Invalid(format!("injected append failure {n}")));
        }
        self.inner.append(key, entry)?;
        if self.fail_after.contains(&n) {
            return Err(LedgerError::Invalid(format!(
                "injected append failure {n} after write"
            )));
        }
        Ok(())
    }
    fn read(&self, key: &TranscriptKey, max_bytes: Option<u64>) -> StoreResult<TranscriptRead> {
        let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        self.log(
            "read",
            key_value(key),
            max_bytes.map(|m| ("options", object([("maxBytes", JsValue::Number(m as f64))]))),
        );
        if self.fail_read.contains(&n) {
            return Err(LedgerError::Invalid(format!("injected read failure {n}")));
        }
        self.inner.read(key, max_bytes)
    }
    fn list(&self, key: &ProjectKey) -> Option<StoreResult<Vec<StoredSession>>> {
        let records = self.list.clone()?;
        self.log(
            "list",
            object([
                ("projectRoot", path_value(&key.project_root)),
                ("pilotHome", path_value(&key.pilot_home)),
            ]),
            None,
        );
        Some(Ok(records))
    }
    fn has(&self, key: &TranscriptKey) -> Option<StoreResult<bool>> {
        if !self.cap("has") {
            return None;
        }
        self.log("has", key_value(key), None);
        self.inner.has(key)
    }
    fn delete(&self, key: &TranscriptKey) -> Option<StoreResult<()>> {
        if !self.cap("delete") {
            return None;
        }
        self.log("delete", key_value(key), None);
        self.inner.delete(key)
    }
    fn delete_session(&self, key: &TranscriptKey) -> Option<StoreResult<()>> {
        if !self.cap("deleteSession") {
            return None;
        }
        self.log("deleteSession", key_value(key), None);
        Some((|| -> StoreResult<()> {
            self.inner.delete(key).unwrap()?;
            let path = &key.transcript_path;
            let name = path.file_name().unwrap().to_string_lossy();
            let dir = path.with_file_name(name.strip_suffix(".jsonl").unwrap_or(&name));
            match fs::remove_dir_all(dir) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            }
        })())
    }
    fn replace(&self, key: &TranscriptKey, entries: &[JsValue]) -> Option<StoreResult<()>> {
        if !self.cap("replace") {
            return None;
        }
        self.log(
            "replace",
            key_value(key),
            Some(("entries", JsValue::Array(entries.to_vec()))),
        );
        self.inner.replace(key, entries)
    }
}

fn error_outcome(error: &AdapterError) -> JsValue {
    let mut e = JsObject::new();
    match error {
        AdapterError::Validation { code, message } => {
            e.insert("name".into(), "SessionEventValidationError".into());
            e.insert("code".into(), (*code).into());
            e.insert("message".into(), message.as_str().into());
        }
        AdapterError::Type(message) => {
            e.insert("name".into(), "TypeError".into());
            e.insert("message".into(), message.as_str().into());
        }
        AdapterError::Range(message) => {
            e.insert("name".into(), "RangeError".into());
            e.insert("message".into(), message.as_str().into());
        }
        AdapterError::ProjectionUnavailable(message) => {
            e.insert("name".into(), "ProjectionUnavailable".into());
            e.insert("message".into(), message.as_str().into());
        }
        AdapterError::Domain { code, message } => {
            e.insert("name".into(), "SessionDomainValidationError".into());
            e.insert("code".into(), (*code).into());
            e.insert("message".into(), message.as_str().into());
        }
        AdapterError::Store(error) => {
            e.insert("name".into(), "Error".into());
            e.insert("message".into(), error.to_string().as_str().into());
        }
        AdapterError::Io(error) => {
            e.insert("name".into(), "Error".into());
            e.insert("message".into(), error.to_string().as_str().into());
        }
    }
    object([("error", JsValue::Object(e))])
}
fn diagnostic(d: &Diagnostic) -> JsValue {
    let mut o = JsObject::new();
    o.insert("code".into(), d.code.into());
    o.insert("severity".into(), d.severity.into());
    if let Some(line) = d.line {
        o.insert("line".into(), JsValue::Number(line as f64));
    }
    if d.code != "transcript_line_invalid" {
        o.insert("message".into(), d.message.as_str().into());
    }
    JsValue::Object(o)
}
fn read_outcome(read: &TranscriptRead) -> JsObject {
    let mut o = JsObject::new();
    o.insert("entries".into(), JsValue::Array(read.entries.clone()));
    o.insert(
        "diagnostics".into(),
        JsValue::Array(read.diagnostics.iter().map(diagnostic).collect()),
    );
    o
}
fn state_value(state: &WriterState) -> JsValue {
    object([
        ("sequence", JsValue::Number(state.sequence as f64)),
        (
            "lastEntryId",
            state
                .last_entry_id
                .clone()
                .map_or(JsValue::Null, JsValue::String),
        ),
    ])
}
fn ok() -> JsValue {
    object([("ok", JsValue::Bool(true))])
}

struct Ctx {
    adapter: AsyncTranscriptAdapter,
    store: Arc<FaultStore>,
    pilot_home: PathBuf,
    chat_dir: PathBuf,
    storage: Option<SessionStorage>,
    session_id: JsString,
    writers: HashMap<String, SubagentHandle>,
}
impl Ctx {
    fn new(dir: &Path, chat_dir: PathBuf, store: FaultStore) -> Self {
        let store = Arc::new(store);
        let layout = chat_dir.clone();
        Self {
            adapter: AsyncTranscriptAdapter::new(
                store.clone(),
                Arc::new(move |_: &ProjectKey| layout.clone()),
            ),
            store,
            pilot_home: dir.join("home"),
            chat_dir,
            storage: None,
            session_id: JsString::default(),
            writers: HashMap::new(),
        }
    }
    fn storage(&self) -> &SessionStorage {
        self.storage.as_ref().expect("open first")
    }
    fn writer(&self, step: &JsValue) -> &TranscriptWriter {
        match step.get("writer") {
            Some(name) => &self.writers[&s(name)].writer,
            None => &self.storage().transcript,
        }
    }
    fn open(&mut self, session_id: JsString) -> &SessionStorage {
        self.session_id = session_id.clone();
        self.storage = Some(
            self.adapter
                .create_session_storage(SessionStorageInput {
                    project_root: PROJECT_ROOT.into(),
                    pilot_home: self.pilot_home.clone(),
                    session_id,
                    now: Some(clock()),
                })
                .unwrap(),
        );
        self.storage()
    }
}
fn unit(result: AdapterResult<()>) -> JsValue {
    result.map_or_else(|e| error_outcome(&e), |()| ok())
}
fn capability(result: Option<AdapterResult<()>>) -> JsValue {
    unit(result.expect("capability configured"))
}
fn run_step(ctx: &mut Ctx, step: &JsValue) -> JsValue {
    let op = s(field(step, "op"));
    let turn = || {
        field(step, "turnId")
            .as_string()
            .cloned()
            .unwrap_or_default()
    };
    let sid = ctx.session_id.clone();
    match op.as_str() {
        "open" => {
            let storage = ctx.open(field(step, "sessionId").as_string().unwrap().clone());
            let paths = object([
                ("chatDir", path_value(&storage.chat_dir)),
                ("transcriptPath", path_value(&storage.transcript_path)),
                (
                    "projectionCheckpointPath",
                    path_value(&storage.projection_checkpoint_path),
                ),
                ("toolResultsDir", path_value(&storage.tool_results_dir)),
                ("fileHistoryDir", path_value(&storage.file_history_dir)),
                ("subagentsDir", path_value(&storage.subagents_dir)),
            ]);
            let caps = [
                ("has", "transcriptExists"),
                ("delete", "deleteTranscript"),
                ("deleteSession", "deleteSessionTranscripts"),
                ("replace", "replaceTranscript"),
            ]
            .iter()
            .filter(|(cap, _)| ctx.store.cap(cap))
            .map(|(_, name)| JsValue::from(*name))
            .collect();
            object([
                ("paths", paths),
                (
                    "externalTranscriptStore",
                    JsValue::Bool(SessionStorage::EXTERNAL_TRANSCRIPT_STORE),
                ),
                ("capabilities", JsValue::Array(caps)),
            ])
        }
        "restore" => match ctx.storage().restore() {
            Ok(read) => {
                let mut o = read_outcome(&read);
                o.insert(
                    "state".into(),
                    state_value(&ctx.storage().transcript.snapshot_state()),
                );
                JsValue::Object(o)
            }
            Err(e) => error_outcome(&e),
        },
        "accepted" => {
            let metadata = field(step, "metadata").as_object().cloned();
            match field(step, "messages") {
                JsValue::Array(messages) => unit(ctx.writer(step).record_accepted_input(
                    &sid,
                    &turn(),
                    messages.clone(),
                    metadata,
                )),
                // recordAcceptedInput with a non-array value builds the same draft.
                messages => {
                    let mut draft = JsObject::new();
                    draft.insert("type".into(), "accepted_input".into());
                    draft.insert("messages".into(), messages.clone());
                    unit(ctx.writer(step).record_session_event(&sid, &turn(), draft))
                }
            }
        }
        "metadata" => unit(ctx.writer(step).record_session_metadata(
            &sid,
            &turn(),
            field(step, "metadata").clone(),
        )),
        "status" => unit(ctx.writer(step).record_agent_status_message(
            &sid,
            &turn(),
            field(step, "status"),
        )),
        "turnResult" => unit(ctx.writer(step).record_turn_result(
            &sid,
            &turn(),
            field(step, "result").clone(),
        )),
        "event" => unit(ctx.writer(step).record_session_event(
            &sid,
            &turn(),
            field(step, "event").as_object().unwrap().clone(),
        )),
        "entry" => unit(ctx.writer(step).record_entry(field(step, "entry"))),
        "restoreState" => unit(ctx.writer(step).restore_state(
            field(step, "sequence").as_number().unwrap(),
            field(step, "lastEntryId"),
        )),
        "state" => object([("state", state_value(&ctx.writer(step).snapshot_state()))]),
        "close" => {
            ctx.writer(step).close();
            ok()
        }
        "read" => match ctx
            .storage()
            .read_transcript(field(step, "maxBytes").as_number().map(|n| n as u64))
        {
            Ok(read) => JsValue::Object(read_outcome(&read)),
            Err(e) => error_outcome(&e),
        },
        "readAt" => match ctx
            .storage()
            .read_transcript_at_path(&ctx.chat_dir.join(s(field(step, "path"))), None)
        {
            Ok(read) => JsValue::Object(read_outcome(&read)),
            Err(e) => error_outcome(&e),
        },
        "has" => match ctx.storage().transcript_exists().expect("has configured") {
            Ok(exists) => object([("exists", JsValue::Bool(exists))]),
            Err(e) => error_outcome(&e),
        },
        "delete" => capability(ctx.storage().delete_transcript()),
        "deleteSession" => capability(ctx.storage().delete_session_transcripts()),
        "replace" => {
            let JsValue::Array(entries) = field(step, "entries") else {
                panic!("replace entries")
            };
            capability(ctx.storage().replace_transcript(entries))
        }
        "sidechain" => {
            let id = field(step, "subagentId").as_string().unwrap().clone();
            let handle = ctx.writer(step).for_subagent(&id);
            let relative = ctx.writer(step).relative_subagent_path(&id);
            let outcome = object([
                ("subagentId", JsValue::String(handle.subagent_id.clone())),
                ("transcriptPath", path_value(&handle.transcript_path)),
                ("relative", relative.as_str().into()),
            ]);
            ctx.writers.insert(s(field(step, "as")), handle);
            outcome
        }
        "list" => {
            let key = ProjectKey {
                project_root: PROJECT_ROOT.into(),
                pilot_home: ctx.pilot_home.clone(),
            };
            match ctx.adapter.list_sessions(
                &key,
                field(step, "limit").as_number(),
                field(step, "offset").as_number(),
            ) {
                None => object([("absent", JsValue::Bool(true))]),
                Some(Ok(sessions)) => object([("sessions", JsValue::Array(sessions))]),
                Some(Err(e)) => error_outcome(&e),
            }
        }
        "concurrent" => {
            let JsValue::Array(steps) = field(step, "steps") else {
                panic!("concurrent steps")
            };
            // TS enqueues them in array order; the writer lock commits in that order.
            let outcomes = steps.iter().map(|inner| run_step(ctx, inner)).collect();
            object([("outcomes", JsValue::Array(outcomes))])
        }
        other => panic!("unknown step {other}"),
    }
}
/// Every file below `root`, keyed by `/`-joined relative path in UTF-8 byte order.
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
fn setup(scenario: &JsValue, dir: &Path) -> Ctx {
    let chat_dir =
        PathBuf::from(s(field(scenario, "chatDir")).replace("<dir>", &dir.display().to_string()));
    if let Some(files) = field(scenario, "initialFiles").as_object() {
        for (rel, text) in files.entries() {
            let path = chat_dir.join(String::from_utf16_lossy(rel.units()));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, s(text)).unwrap();
        }
    }
    Ctx::new(
        dir,
        chat_dir,
        FaultStore::from_config(field(scenario, "store")),
    )
}

#[test]
fn real_ts_scenarios_match_outcomes_calls_and_bytes() {
    let oracle = oracle();
    let JsValue::Array(scenarios) = field(&oracle, "scenarios") else {
        panic!("scenarios")
    };
    // Keep in sync with transcript-adapter/generate.mts and its README.
    assert_eq!(scenarios.len(), 397, "adapter oracle scenario count");
    let scratch = Scratch::new();
    for scenario in scenarios {
        let name = s(field(scenario, "name"));
        let dir = scratch.0.join(&name);
        fs::create_dir_all(&dir).unwrap();
        let mut ctx = setup(scenario, &dir);
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
        for key in ["name", "store", "initialFiles"] {
            actual.insert(key.into(), field(scenario, key).clone());
        }
        actual.insert("chatDir".into(), path_value(&ctx.chat_dir));
        actual.insert("steps".into(), JsValue::Array(replayed));
        actual.insert("calls".into(), JsValue::Array(ctx.store.calls()));
        actual.insert("files".into(), snapshot_files(&dir));
        let actual = normalize(
            &stringify_json(&JsValue::Object(actual)).unwrap().unwrap(),
            &dir,
        );
        let expected = stringify_json(scenario).unwrap().unwrap();
        if actual != expected {
            // Pinpoint the first differing part before failing on the whole.
            let a = parse_json(&actual).unwrap();
            for part in ["steps", "calls", "files"] {
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

fn fresh(dir: &Path, store: Arc<dyn TranscriptStore>) -> (AsyncTranscriptAdapter, SessionStorage) {
    let chat = dir.join("chats");
    let adapter = AsyncTranscriptAdapter::new(store, Arc::new(move |_: &ProjectKey| chat.clone()));
    let storage = adapter
        .create_session_storage(SessionStorageInput {
            project_root: PROJECT_ROOT.into(),
            pilot_home: dir.join("home"),
            session_id: "s1".into(),
            now: Some(clock()),
        })
        .unwrap();
    (adapter, storage)
}
fn metadata_value(n: usize) -> JsValue {
    object([("title", JsValue::String(format!("t{n}").into()))])
}

#[test]
fn concurrent_appends_commit_in_lock_order_and_failures_leave_no_gap() {
    let scratch = Scratch::new();
    let store = Arc::new(FaultStore {
        fail_before: (1..=200).filter(|n| n % 7 == 0).collect(),
        ..FaultStore::default()
    });
    let (_adapter, storage) = fresh(&scratch.0, store.clone());
    let storage = Arc::new(storage);
    let failures = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let storage = storage.clone();
            let failures = failures.clone();
            thread::spawn(move || {
                for i in 0..25 {
                    if storage
                        .transcript
                        .record_session_metadata(
                            &"s1".into(),
                            &"turn".into(),
                            metadata_value(t * 100 + i),
                        )
                        .is_err()
                    {
                        failures.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let failed = failures.load(Ordering::SeqCst);
    assert_eq!(failed, 28, "every injected failure surfaced to its caller");
    let committed = 200 - failed;
    let read = storage.read_transcript(None).unwrap();
    assert!(read.diagnostics.is_empty());
    assert_eq!(read.entries.len(), committed);
    let state = storage.transcript.snapshot_state();
    assert_eq!(state.sequence, committed as u64);
    // File order is commit order: contiguous sequences and an unbroken chain.
    let bytes = fs::read_to_string(&storage.transcript_path).unwrap();
    let lines: Vec<_> = bytes.lines().map(js).collect();
    for (i, entry) in lines.iter().enumerate() {
        assert_eq!(field(entry, "sequence").as_number(), Some((i + 1) as f64));
        let parent = field(entry, "parentEntryId");
        if i == 0 {
            assert_eq!(parent, &JsValue::Null);
        } else {
            assert_eq!(parent, field(&lines[i - 1], "entryId"));
        }
    }
    assert_eq!(
        state.last_entry_id.as_ref(),
        field(lines.last().unwrap(), "entryId").as_string()
    );
    // Failed calls reached the store with the sequence the next success reused.
    // The log also holds the read_transcript above; like the generator's
    // appendCalls, only appends are counted.
    let all = store.calls();
    let is_append = |c: &&JsValue| s(field(c, "method")) == "append";
    let calls: Vec<_> = all.iter().filter(is_append).collect();
    assert_eq!(calls.len(), 200);
    assert_eq!(all.len(), 201, "200 appends and the one read above");
    assert_eq!(s(field(all.last().unwrap(), "method")), "read");
    for (i, call) in calls.iter().enumerate() {
        if (i + 1) % 7 == 0 {
            assert_eq!(
                field(field(call, "entry"), "sequence"),
                field(field(calls[i + 1], "entry"), "sequence"),
                "call {} failed without publishing",
                i + 1
            );
        }
    }
    let leftovers: Vec<_> = fs::read_dir(storage.transcript_path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn domain_restore_retains_validation_state() {
    let scratch = Scratch::new();
    let store = Arc::new(FaultStore::default());
    let (_adapter, storage) = fresh(&scratch.0, store.clone());
    let entry = js(&format!(
        r#"{{"type":"turn_started","sessionId":"s1","turnId":"t","sequence":1,"createdAt":"{NOW}"}}"#
    ));
    store.inner.append(&storage_key(&storage), &entry).unwrap();
    storage.restore().unwrap();
    let before = storage.transcript.snapshot_state();
    let mut event = JsObject::new();
    event.insert("type".into(), "turn_started".into());
    assert!(matches!(
        storage
            .transcript
            .record_session_event(&"s1".into(), &"t".into(), event),
        Err(AdapterError::Domain {
            code: "duplicate_turn_start",
            ..
        })
    ));
    assert_eq!(before, storage.transcript.snapshot_state());
}
#[test]
fn deep_sequence_array_message_is_a_range_error_not_a_stack_overflow() {
    let mut sequence = JsValue::Number(1.0);
    for _ in 0..1_000_000 {
        sequence = JsValue::Array(vec![sequence]);
    }
    let entries = [object([("sequence", sequence)])];
    match transcript_adapter::validate_event_log(&entries) {
        Err(AdapterError::Range(message)) => {
            let oracle = js(include_str!("../../tests/fixtures/runtime-boundaries.json"));
            let expected = oracle.get("deepErrors").unwrap().get("transcript").unwrap();
            assert_eq!(expected.get("name"), Some(&"RangeError".into()));
            assert_eq!(expected.get("code"), Some(&JsValue::Null));
            assert_eq!(expected.get("message"), Some(&message.as_str().into()));
        }
        other => panic!("expected RangeError, got {other:?}"),
    }
}
fn storage_key(storage: &SessionStorage) -> TranscriptKey {
    TranscriptKey {
        project_root: PROJECT_ROOT.into(),
        pilot_home: PathBuf::new(),
        session_id: "s1".into(),
        transcript_path: storage.transcript_path.clone(),
    }
}

#[test]
fn jsonl_store_failure_keeps_previous_bytes_and_state() {
    let scratch = Scratch::new();
    let store = Arc::new(JsonlTranscriptStore::new());
    let (_adapter, storage) = fresh(&scratch.0, store);
    storage
        .transcript
        .record_session_metadata(&"s1".into(), &"t".into(), metadata_value(1))
        .unwrap();
    let before = fs::read(&storage.transcript_path).unwrap();
    let state = storage.transcript.snapshot_state();
    // Make the append open fail: the transcript is read-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = &storage.transcript_path;
        fs::set_permissions(path, fs::Permissions::from_mode(0o400)).unwrap();
        let result = storage.transcript.record_session_metadata(
            &"s1".into(),
            &"t".into(),
            metadata_value(2),
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(result, Err(AdapterError::Store(_))), "{result:?}");
        assert_eq!(fs::read(&storage.transcript_path).unwrap(), before);
        assert_eq!(storage.transcript.snapshot_state(), state);
        storage
            .transcript
            .record_session_metadata(&"s1".into(), &"t".into(), metadata_value(3))
            .unwrap();
        assert_eq!(storage.transcript.snapshot_state().sequence, 2);
    }
}

#[test]
fn jsonl_store_instances_appending_one_key_keep_every_line() {
    let scratch = Scratch::new();
    let key = TranscriptKey {
        project_root: PROJECT_ROOT.into(),
        pilot_home: PathBuf::new(),
        session_id: "s1".into(),
        transcript_path: scratch.0.join("chat/s1.jsonl"),
    };
    let entry = |n: usize| js(&format!(r#"{{"type":"session_metadata","n":{n}}}"#));
    // Interleaved: each instance's first append prepares the tail once, like
    // TS prepareTail, so crash debris gets exactly one LF.
    fs::create_dir_all(key.transcript_path.parent().unwrap()).unwrap();
    fs::write(&key.transcript_path, r#"{"type":"partial"#).unwrap();
    let (a, b) = (JsonlTranscriptStore::new(), JsonlTranscriptStore::new());
    a.append(&key, &entry(1)).unwrap();
    b.append(&key, &entry(2)).unwrap();
    a.append(&key, &entry(3)).unwrap();
    assert_eq!(
        fs::read_to_string(&key.transcript_path).unwrap(),
        "{\"type\":\"partial\n\
         {\"type\":\"session_metadata\",\"n\":1}\n\
         {\"type\":\"session_metadata\",\"n\":2}\n\
         {\"type\":\"session_metadata\",\"n\":3}\n"
    );
    // Concurrent: two instances (as two Gateway processes would) lose no line.
    fs::remove_file(&key.transcript_path).unwrap();
    let stores = [
        Arc::new(JsonlTranscriptStore::new()),
        Arc::new(JsonlTranscriptStore::new()),
    ];
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let store = stores[t % 2].clone();
            let key = key.clone();
            thread::spawn(move || {
                for i in 0..50 {
                    store.append(&key, &entry(t * 100 + i)).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let text = fs::read_to_string(&key.transcript_path).unwrap();
    let written: HashSet<usize> = text
        .lines()
        .map(|line| field(&js(line), "n").as_number().unwrap() as usize)
        .collect();
    let expected: HashSet<usize> = (0..8)
        .flat_map(|t| (0..50).map(move |i| t * 100 + i))
        .collect();
    assert_eq!(text.lines().count(), 400);
    assert_eq!(written, expected);
}

#[test]
#[ignore = "requires fixed TS baseline and Node/tsx; run with PDRS_BASELINE"]
fn real_ts_and_rust_continue_each_others_transcripts() {
    let baseline = std::env::var("PDRS_BASELINE").expect("set fixed baseline path");
    let scratch = Scratch::new();
    let node = |mode: &str| {
        let output = std::process::Command::new("node")
            .arg("--import")
            .arg(format!("{baseline}/node_modules/tsx/dist/loader.mjs"))
            .arg(format!("{FIXTURES}/generate.mts"))
            .arg(&scratch.0)
            .arg(mode)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        println!("{stdout}{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{mode} failed");
        stdout
    };
    let written = node("--cross-ts-write");
    let line = BufReader::new(written.as_bytes())
        .lines()
        .next()
        .unwrap()
        .unwrap();
    let chat_dir = PathBuf::from(s(field(&js(&line), "chatDir")));
    let open = || {
        let chat = chat_dir.clone();
        AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |_: &ProjectKey| chat.clone()),
        )
        .create_session_storage(SessionStorageInput {
            project_root: PROJECT_ROOT.into(),
            pilot_home: scratch.0.join("home"),
            session_id: "cross".into(),
            now: None,
        })
        .unwrap()
    };
    let storage = open();
    let restored = storage.restore().unwrap();
    assert_eq!(restored.entries.len(), 2);
    assert_eq!(
        restored
            .diagnostics
            .iter()
            .map(|d| (d.code, d.line))
            .collect::<Vec<_>>(),
        [("transcript_line_invalid", Some(3))]
    );
    storage
        .transcript
        .record_session_metadata(
            &"cross".into(),
            &"turn-1".into(),
            object([("title", "from Rust".into())]),
        )
        .unwrap();
    // A sidechain writer starts at 0; the caller restores it from the sidechain read.
    let kid = storage.transcript.for_subagent(&"kid".into());
    let previous = storage
        .read_transcript_at_path(&kid.transcript_path, None)
        .unwrap();
    let last = previous.entries.last().unwrap();
    kid.writer
        .restore_state(
            field(last, "sequence").as_number().unwrap(),
            field(last, "entryId"),
        )
        .unwrap();
    kid.writer
        .record_session_metadata(
            &"cross".into(),
            &"turn-1".into(),
            object([("tag", "rust-kid".into())]),
        )
        .unwrap();
    assert!(node("--cross-ts-verify").contains("PASS"));
    let reopened = open();
    let read = reopened.restore().unwrap();
    assert_eq!(read.entries.len(), 4);
    assert_eq!(reopened.transcript.snapshot_state().sequence, 4);
    assert_eq!(
        field(field(&read.entries[3], "metadata"), "aiTitle"),
        &JsValue::from("TS after Rust")
    );
}

#[test]
fn jsonl_store_control_boundary_without_boundary_throws_like_ts_and_appends_nothing() {
    let scratch = Scratch::new();
    let key = TranscriptKey {
        project_root: PROJECT_ROOT.into(),
        pilot_home: PathBuf::new(),
        session_id: "s1".into(),
        transcript_path: scratch.0.join("chat/s1.jsonl"),
    };
    let store = JsonlTranscriptStore::new();
    store
        .append(&key, &js(r#"{"type":"session_metadata","n":1}"#))
        .unwrap();
    // JsonlSessionPersistence.ts:24-25: entry.boundary.kind on undefined/null throws.
    for (entry, shown) in [
        (r#"{"type":"control_boundary"}"#, "undefined"),
        (r#"{"type":"control_boundary","boundary":null}"#, "null"),
    ] {
        let error = store.append(&key, &js(entry)).unwrap_err().to_string();
        assert_eq!(
            error,
            format!("Cannot read properties of {shown} (reading 'kind')")
        );
    }
    // A primitive boundary reads .kind as undefined in JS: written, not flushed.
    store
        .append(&key, &js(r#"{"type":"control_boundary","boundary":"x"}"#))
        .unwrap();
    assert_eq!(
        fs::read_to_string(&key.transcript_path).unwrap(),
        "{\"type\":\"session_metadata\",\"n\":1}\n{\"type\":\"control_boundary\",\"boundary\":\"x\"}\n"
    );
}

/// #755: TS `getPilotProjectChatDir` resolves an absolute `projectRoot`
/// without `process.cwd()`, but a relative one goes through `path.resolve`
/// and throws `ENOENT ... uv_cwd` after the cwd is deleted. The adapter's
/// host chat-dir callback gets the same treatment.
#[test]
fn deleted_process_cwd_fails_relative_transcript_bases() {
    if std::env::var_os("PDRS_DELETED_CWD_TRANSCRIPT").is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "storage::transcript_adapter_tests::deleted_process_cwd_fails_relative_transcript_bases",
                "--nocapture",
            ])
            .env("PDRS_DELETED_CWD_TRANSCRIPT", "1")
            .output()
            .expect("run deleted-cwd child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "child failed: {stdout}\n{stderr}");
        assert!(
            stdout.contains("PD_TRANSCRIPT_CWD_ERROR=ENOENT: no such file or directory, uv_cwd"),
            "missing Node-shaped error in: {stdout}\n{stderr}"
        );
        return;
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "pdrs-755-transcript-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("create cwd");
    std::env::set_current_dir(&directory).expect("enter cwd");
    fs::remove_dir_all(&directory).expect("delete cwd");

    let absolute = AsyncTranscriptAdapter::new(
        Arc::new(JsonlTranscriptStore::new()),
        Arc::new(|_| PathBuf::from("/pdrs-755/chats")),
    );
    let storage = absolute
        .create_session_storage(SessionStorageInput {
            project_root: PathBuf::from("/pdrs-755/project"),
            pilot_home: PathBuf::from("/pdrs-755/home"),
            session_id: "s".into(),
            now: None,
        })
        .expect("absolute bases must not read the cwd");
    assert_eq!(storage.chat_dir, PathBuf::from("/pdrs-755/chats"));
    assert_eq!(
        storage.tool_results_dir,
        PathBuf::from("/pdrs-755/project/.pilotdeck/tool-results/s")
    );

    let relative = AsyncTranscriptAdapter::new(
        Arc::new(JsonlTranscriptStore::new()),
        Arc::new(|_| PathBuf::from("relative/chats")),
    );
    let error = relative
        .create_session_storage(SessionStorageInput {
            project_root: PathBuf::from("relative/project"),
            pilot_home: PathBuf::from("/pdrs-755/home"),
            session_id: "s".into(),
            now: None,
        })
        .err()
        .expect("relative bases need the deleted cwd");
    println!("PD_TRANSCRIPT_CWD_ERROR={error}");
}
