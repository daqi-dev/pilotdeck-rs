//! Rust form of TS `createGatewayAsyncTranscriptStorageAdapter`
//! (B/src/gateway/storage/AsyncTranscriptStorageAdapter.ts:120-414): primary
//! and sidechain transcript records go through a host-owned store, with the
//! SessionRuntime commit order of B/src/session/events/SessionRuntime.ts:46-152
//! (validate → store.append → publish state). A failed append leaves sequence,
//! parent and entry ids unpublished, so the next append reuses the sequence.
//! Not wired into the host, ProjectRuntimeRegistry or gateway methods.
//!
//! Domain validation is shared with the recorder. The two read ports gate on
//! the TS reducer reads they model: `control_boundary_supported`,
//! `turn_summary_supported`, `metadata_reduce_supported`,
//! `validate_hydrated_message`, `file_artifact_supported` and
//! `compact_budget_supported` mirror hydrate (reduce only) and
//! `list_conversation_entry_supported` + `list_title_supported` mirror
//! listSessions (conversation reduce + finalize, then the title trims).
//! Every gate runs projector by projector over the whole log, because that is
//! the order TS surfaces errors in. Shapes TS throws on reject with the same
//! TypeError text; ProjectionUnavailable is left for shapes TS accepts that
//! Rust has not modelled.
use super::{
    LedgerError, TranscriptRead, WriterState, invalid,
    ledger::field,
    recorder::{RecorderClock, RecorderError, RecorderResult, RecorderUuid},
    session_domain::{DomainState, Undo},
    transcript::{
        LedgerLock, create_temporary, private_dirs, private_open, read_transcript_limited_at, uuid,
    },
};
use crate::{
    js_value,
    node_io::{descriptor_error, path_error},
    runtime::memory::js_trim_units,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashSet,
    fmt, fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

pub type StoreResult<T> = super::Result<T>;
pub type AdapterResult<T> = std::result::Result<T, AdapterError>;
/// Returns `Date.prototype.toISOString()` text for each committed entry.
pub type Clock = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Debug)]
pub enum AdapterError {
    /// TS SessionEventValidationError; code and message match TS.
    Validation { code: &'static str, message: String },
    /// TS TypeError. Only the V8 messages listed at the construction sites are modelled.
    Type(String),
    /// TS RangeError: V8's too-deep `Array.prototype.join` while building a
    /// message (real TS evidence: tests/fixtures/runtime-boundaries.json).
    Range(String),
    /// TS SessionDomainValidationError.
    Domain { code: &'static str, message: String },
    /// Recovery/list projection outside the currently ported read surface.
    ProjectionUnavailable(String),
    /// #1019 fence handoff: this writer's ledger fence moved to the session
    /// factory, so appends through it (any Arc clone) are rejected instead of
    /// forking the sequence chain. Rust-side composition constraint; TS has no
    /// fence and its `close()` is a silent no-op, not a rejection.
    Retired(String),
    /// #1457 fix-r3: the writer is closing (its session was disposed) and the
    /// entry would be dropped by the `closed` no-op. Only the "checked"
    /// metadata appends raise this, so a request that reports success can
    /// never be one whose entry never reached the log. Cleanup-hook appends
    /// keep the silent no-op the TS `close()` models.
    Closed(String),
    /// Error from the host store, propagated unchanged (TS rethrows it).
    Store(LedgerError),
}
impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { message, .. } | Self::Type(message) | Self::Range(message) => {
                f.write_str(message)
            }
            Self::Domain { message, .. } | Self::ProjectionUnavailable(message) => {
                f.write_str(message)
            }
            Self::Retired(message) | Self::Closed(message) => f.write_str(message),
            Self::Store(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for AdapterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}
fn validation(code: &'static str, message: String) -> AdapterError {
    AdapterError::Validation { code, message }
}
impl From<RecorderError> for AdapterError {
    fn from(error: RecorderError) -> Self {
        match error {
            RecorderError::Domain { code, message } => Self::Domain { code, message },
            RecorderError::Envelope { code, message } => Self::Validation { code, message },
            RecorderError::Type(message) => Self::Type(message),
            RecorderError::Range(message) => Self::Range(message),
            RecorderError::ProjectionUnavailable(message) => Self::ProjectionUnavailable(message),
            RecorderError::State(message) => Self::ProjectionUnavailable(message),
            RecorderError::Retired(message) => Self::Retired(message),
            RecorderError::Store(error) => Self::Store(error),
        }
    }
}
fn projection_unavailable(message: String) -> AdapterError {
    AdapterError::ProjectionUnavailable(message)
}

/// TS GatewayAsyncTranscriptKey. `transcript_path` is the Gateway-local
/// identifier; the store owns the durable contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptKey {
    pub project_root: PathBuf,
    pub pilot_home: PathBuf,
    pub session_id: JsString,
    pub transcript_path: PathBuf,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectKey {
    pub project_root: PathBuf,
    pub pilot_home: PathBuf,
}
/// TS GatewayAsyncTranscriptSession.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredSession {
    pub session_id: JsString,
    pub last_modified: f64,
    pub file_size: Option<f64>,
}

/// TS GatewayAsyncTranscriptStore. `append` must be atomic for a key; the
/// adapter serializes appends per writer but not across Gateway processes.
/// Optional methods return `None` when the store does not offer them, which
/// is the TS "method absent" case (the adapter then omits the capability).
pub trait TranscriptStore: Send + Sync {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()>;
    fn read(&self, key: &TranscriptKey, max_bytes: Option<u64>) -> StoreResult<TranscriptRead>;
    fn list(&self, _key: &ProjectKey) -> Option<StoreResult<Vec<StoredSession>>> {
        None
    }
    fn has(&self, _key: &TranscriptKey) -> Option<StoreResult<bool>> {
        None
    }
    fn delete(&self, _key: &TranscriptKey) -> Option<StoreResult<()>> {
        None
    }
    fn delete_session(&self, _key: &TranscriptKey) -> Option<StoreResult<()>> {
        None
    }
    fn replace(&self, _key: &TranscriptKey, _entries: &[JsValue]) -> Option<StoreResult<()>> {
        None
    }
    /// #1370 controlled stimulus: the entry this store's writes witness. The
    /// default resolves the process-wide env entry, so every implementation
    /// keeps the production default; `JsonlTranscriptStore` overrides it with
    /// the instance handle captured at construction, which is how the
    /// stimulus tests arm one store without reaching any other.
    fn stimulus(&self) -> Option<&Arc<super::stimulus::StorageStimulus>> {
        super::stimulus::StorageStimulus::global_ref()
    }
}

/// Gateway-local JSONL host store with the TS default provider's bytes:
/// append = JsonlSessionPersistence (B/src/session/persistence/
/// JsonlSessionPersistence.ts:20-65), read = readTranscript. Like TS, each
/// append is one O_APPEND write of `JSON.stringify(entry) + "\n"`, so
/// concurrent writers in other instances or processes interleave whole lines
/// instead of losing one; a failed write can leave a torn line, which
/// readTranscript then reports. Like TS `prepareTail`, the first append to a
/// path in this instance adds one LF after unterminated crash debris. No
/// `.lock` file by default (TS has none either); this instance serializes its
/// own writes. Project persistence opts into `open_locked` for one writer per
/// canonical path, sharing the ledger fence until all writer clones drop.
#[derive(Default)]
pub struct JsonlTranscriptStore {
    /// Write lock; holds the paths whose tail this instance has prepared.
    prepared: Mutex<HashSet<PathBuf>>,
    /// Only project-owned session stores opt into the shared ledger fence.
    /// The store is retained by every Arc<TranscriptWriter> clone, so the
    /// fence spans generations until #1019's holder moves it out (`take_fence`)
    /// into the factory's writer lease — which also retires that writer, so a
    /// retained clone cannot keep appending past the handoff.
    owner: Mutex<Option<LedgerLock>>,
    /// A project-owned store serves one canonical transcript; diagnostics keep
    /// the original spelling supplied when that writer acquired its fence.
    diagnostic_path: Option<PathBuf>,
    /// #1370 controlled stimulus: resolved once at construction, so arming a
    /// stimulus for this store can never be observed by another component's
    /// writes. `None` (env unset) keeps the production default inert.
    stimulus: Option<Arc<super::stimulus::StorageStimulus>>,
}
impl JsonlTranscriptStore {
    pub fn new() -> Self {
        Self::with_stimulus(super::stimulus::StorageStimulus::global())
    }
    /// #1370: construct wired to a specific controlled stimulus — the
    /// production `new` passes the process-wide env-resolved entry, while the
    /// stimulus tests inject a dedicated instance so arming can never reach
    /// another component's store.
    pub(crate) fn with_stimulus(stimulus: Option<Arc<super::stimulus::StorageStimulus>>) -> Self {
        Self {
            stimulus,
            ..Self::default()
        }
    }
    pub(crate) fn open_locked(path: &Path) -> StoreResult<(Self, PathBuf)> {
        let diagnostic_path = path.to_path_buf();
        let (path, owner) = LedgerLock::acquire(path)?;
        Ok((
            Self {
                owner: Mutex::new(Some(owner)),
                diagnostic_path: Some(diagnostic_path),
                ..Self::with_stimulus(super::stimulus::StorageStimulus::global())
            },
            path,
        ))
    }
    /// Move this store's ledger fence out (#1019). The caller owns the fence
    /// from here on, so its release can be tied to the writer drain instead
    /// of this store's Arc lifetime.
    pub(crate) fn take_fence(&self) -> Option<LedgerLock> {
        self.owner.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
    fn lock(&self, path: &Path) -> StoreResult<MutexGuard<'_, HashSet<PathBuf>>> {
        let guard = self.prepared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            private_dirs(parent)?;
        }
        Ok(guard)
    }
    /// TS prepareTail: `open(path, "a+", 0o600)`, then LF after a non-LF last byte.
    fn prepare_tail(path: &Path) -> StoreResult<()> {
        let mut file = private_open()
            .read(true)
            .append(true)
            .create(true)
            .open(path)
            .map_err(|e| path_error(e, "open", path))?;
        let size = file
            .metadata()
            .map_err(|e| descriptor_error(e, "fstat"))?
            .len();
        if size > 0 {
            let mut last = [0u8; 1];
            file.seek(SeekFrom::Start(size - 1))
                .map_err(|e| descriptor_error(e, "lseek"))?;
            file.read_exact(&mut last)
                .map_err(|e| descriptor_error(e, "read"))?;
            if last[0] != b'\n' {
                file.write_all(b"\n")
                    .map_err(|e| descriptor_error(e, "write"))?;
            }
        }
        Ok(())
    }
}
fn is_str(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|t| t.equals_str(text))
}
fn json_line(entry: &JsValue) -> StoreResult<String> {
    let mut line = stringify_json(entry)?
        .ok_or_else(|| invalid("Session event must be JSON serializable."))?;
    line.push('\n');
    Ok(line)
}
impl TranscriptStore for JsonlTranscriptStore {
    fn stimulus(&self) -> Option<&Arc<super::stimulus::StorageStimulus>> {
        self.stimulus.as_ref()
    }
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        let line = json_line(entry)?;
        let path = &key.transcript_path;
        let mut prepared = self.lock(path)?;
        if !prepared.contains(path) {
            Self::prepare_tail(path)?;
            prepared.insert(path.clone());
        }
        // appendFile(path, line, { mode: 0o600, flush }): flush only for compact boundaries.
        let boundary = field(entry, "boundary");
        let control = is_str(field(entry, "type"), "control_boundary");
        // JsonlSessionPersistence.ts:24-25 reads entry.boundary.kind after prepareTail:
        // a missing or null boundary throws TypeError there, so nothing is appended.
        if control && matches!(boundary, JsValue::Undefined | JsValue::Null) {
            let shown = if matches!(boundary, JsValue::Null) {
                "null"
            } else {
                "undefined"
            };
            return Err(invalid(format!(
                "Cannot read properties of {shown} (reading 'kind')"
            )));
        }
        let flush = control
            && is_str(field(boundary, "kind"), "compact")
            && is_str(field(boundary, "subtype"), "compact_boundary");
        // #1370 controlled stimulus: the real writer's underlying append, with
        // the caller's commit lock (the TS writeTail) held. Inert unless this
        // store was constructed with the controlled entry armed; the bounded
        // barrier holds the writeTail, and the single EIO fails the append
        // before any bytes.
        let stimulus = self.stimulus.clone();
        let blocked = match &stimulus {
            Some(stimulus) => {
                stimulus.before_write(
                    super::stimulus::Site::Writer,
                    &super::stimulus::writer_entry_key(entry),
                )? == super::stimulus::Applied::Blocked
            }
            None => false,
        };
        let mut file = private_open()
            .append(true)
            .create(true)
            .open(path)
            .map_err(|e| path_error(e, "open", path))?;
        file.write_all(line.as_bytes())
            .map_err(|e| descriptor_error(e, "write"))?;
        if flush {
            file.sync_all().map_err(|e| descriptor_error(e, "fsync"))?;
        }
        if let Some(stimulus) = &stimulus
            && blocked
        {
            stimulus.completed(
                super::stimulus::Site::Writer,
                &super::stimulus::writer_entry_key(entry),
            );
        }
        Ok(())
    }
    fn read(&self, key: &TranscriptKey, max_bytes: Option<u64>) -> StoreResult<TranscriptRead> {
        read_transcript_limited_at(
            &key.transcript_path,
            self.diagnostic_path
                .as_deref()
                .unwrap_or(&key.transcript_path),
            max_bytes,
        )
    }
    fn has(&self, key: &TranscriptKey) -> Option<StoreResult<bool>> {
        Some(Ok(fs::metadata(&key.transcript_path).is_ok()))
    }
    fn delete(&self, key: &TranscriptKey) -> Option<StoreResult<()>> {
        Some(match fs::remove_file(&key.transcript_path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                Err(path_error(e, "unlink", &key.transcript_path).into())
            }
            _ => Ok(()),
        })
    }
    fn replace(&self, key: &TranscriptKey, entries: &[JsValue]) -> Option<StoreResult<()>> {
        let lines = entries
            .iter()
            .map(json_line)
            .collect::<StoreResult<String>>();
        // The TS store contract calls replace atomic; the fixture's writeFile
        // truncates first. A sibling replace gives the same bytes without a
        // window where the transcript is empty or half written.
        Some(lines.and_then(|lines| {
            let path = &key.transcript_path;
            let _guard = self.lock(path)?;
            let (mut file, cleanup) = create_temporary(path)?;
            file.write_all(lines.as_bytes())
                .map_err(|e| descriptor_error(e, "write"))?;
            file.sync_all().map_err(|e| descriptor_error(e, "fsync"))?;
            // The sibling temp name is a Rust atomicity detail; the diagnostic
            // names the transcript the caller asked to replace (TS writeFile).
            fs::rename(&cleanup.0, path).map_err(|e| path_error(e, "rename", path))?;
            Ok(())
        }))
    }
}

/// TS AsyncTranscriptStorageAdapter options. `project_chat_dir` is the TS
/// `getPilotProjectChatDir` layout (or the host `getProjectChatDir`); the
/// project-id resolution itself stays with the caller.
pub struct AsyncTranscriptAdapter {
    store: Arc<dyn TranscriptStore>,
    project_chat_dir: Arc<dyn Fn(&ProjectKey) -> PathBuf + Send + Sync>,
}
pub struct SessionStorageInput {
    pub project_root: PathBuf,
    pub pilot_home: PathBuf,
    pub session_id: JsString,
    pub now: Option<Clock>,
}
impl AsyncTranscriptAdapter {
    pub fn new(
        store: Arc<dyn TranscriptStore>,
        project_chat_dir: Arc<dyn Fn(&ProjectKey) -> PathBuf + Send + Sync>,
    ) -> Self {
        Self {
            store,
            project_chat_dir,
        }
    }
    pub fn project_chat_dir(&self, key: &ProjectKey) -> PathBuf {
        (self.project_chat_dir)(key)
    }
    /// ProjectSessionStorage.ts:401-417 paths; transcript goes to the store.
    pub fn create_session_storage(
        &self,
        input: SessionStorageInput,
    ) -> AdapterResult<SessionStorage> {
        let project = ProjectKey {
            project_root: input.project_root,
            pilot_home: input.pilot_home,
        };
        let chat_dir = resolve(&self.project_chat_dir(&project), &[])?;
        let safe_id = sanitize_session_id(&input.session_id);
        let safe_id = safe_id.as_str();
        let transcript_path = resolve(&chat_dir, &[format!("{safe_id}.jsonl").as_str()])?;
        let subagents_dir = resolve(&chat_dir, &[safe_id, "subagents"])?;
        let mut checkpoint = transcript_path.clone().into_os_string();
        checkpoint.push(".projections.json");
        let base = Arc::new(KeyBase {
            project,
            session_id: input.session_id,
            store: self.store.clone(),
            now: input
                .now
                .unwrap_or_else(|| Arc::new(|| super::transcript::timestamp().unwrap_or_default())),
        });
        Ok(SessionStorage {
            tool_results_dir: resolve(
                &base.project.project_root,
                &[".pilotdeck", "tool-results", safe_id],
            )?,
            file_history_dir: resolve(&chat_dir, &[safe_id, "file-history"])?,
            projection_checkpoint_path: checkpoint.into(),
            transcript: TranscriptWriter::new(
                base.clone(),
                transcript_path.clone(),
                SubagentLayout::Dir(subagents_dir.clone()),
            ),
            chat_dir,
            transcript_path,
            subagents_dir,
            base,
        })
    }
    /// TS listSessions (AsyncTranscriptStorageAdapter.ts:392-411); `None` when
    /// the store has no `list`. Reads every record before projecting, like the
    /// TS Promise.all; the first failure in record order is returned.
    pub fn list_sessions(
        &self,
        input: &ProjectKey,
        limit: Option<f64>,
        offset: Option<f64>,
    ) -> Option<AdapterResult<Vec<JsValue>>> {
        let records = match self.store.list(input)? {
            Ok(records) => records,
            Err(error) => return Some(Err(AdapterError::Store(error))),
        };
        let reads: Vec<_> = records
            .iter()
            .map(|record| {
                self.create_session_storage(SessionStorageInput {
                    project_root: input.project_root.clone(),
                    pilot_home: input.pilot_home.clone(),
                    session_id: record.session_id.clone(),
                    now: None,
                })
                .and_then(|storage| storage.read_transcript(None))
            })
            .collect();
        Some((|| -> AdapterResult<Vec<JsValue>> {
            let mut sessions = Vec::new();
            for (record, read) in records.iter().zip(reads) {
                if let Some(info) = project_session_info(record, &read?)? {
                    sessions.push((record.last_modified, info));
                }
            }
            if sessions.iter().any(|(modified, _)| modified.is_nan()) {
                return Err(projection_unavailable(
                    "Session listing with a NaN lastModified is not implemented (A7.5).".into(),
                ));
            }
            sessions.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            // Math.max(0, NaN) is NaN, unlike f64::max.
            let offset = offset.unwrap_or(0.0);
            let offset = if offset.is_nan() {
                offset
            } else {
                offset.max(0.0)
            };
            let limit = limit.unwrap_or(records.len() as f64);
            let len = sessions.len();
            let start = slice_index(offset, len);
            let end = if limit == 0.0 {
                len
            } else {
                slice_index(offset + limit, len)
            };
            Ok(sessions
                .into_iter()
                .skip(start)
                .take(end.saturating_sub(start))
                .map(|(_, info)| info)
                .collect())
        })())
    }
}

struct KeyBase {
    project: ProjectKey,
    session_id: JsString,
    store: Arc<dyn TranscriptStore>,
    now: Clock,
}
impl KeyBase {
    /// TS keyForPath: every key carries the owning session id, sidechains too.
    fn key(&self, transcript_path: &Path) -> TranscriptKey {
        TranscriptKey {
            project_root: self.project.project_root.clone(),
            pilot_home: self.project.pilot_home.clone(),
            session_id: self.session_id.clone(),
            transcript_path: transcript_path.to_path_buf(),
        }
    }
}

/// The adapter's AgentProjectSessionStorage view (`externalTranscriptStore: true`).
pub struct SessionStorage {
    pub chat_dir: PathBuf,
    pub transcript_path: PathBuf,
    pub projection_checkpoint_path: PathBuf,
    pub tool_results_dir: PathBuf,
    pub file_history_dir: PathBuf,
    pub subagents_dir: PathBuf,
    pub transcript: TranscriptWriter,
    base: Arc<KeyBase>,
}
impl SessionStorage {
    pub const EXTERNAL_TRANSCRIPT_STORE: bool = true;
    pub fn subagent_transcript_path(&self, subagent_id: &JsString) -> PathBuf {
        self.transcript.sidechain_path(subagent_id)
    }
    /// ProjectSessionStorage.ts:337-348: persistence.load (store.read without
    /// options) then SessionRuntime.restore, which validates the whole log
    /// before any state changes. Malformed-line diagnostics do not fail the
    /// restore; a size-limited read cannot seed a writable chain and is rejected.
    pub fn restore(&self) -> AdapterResult<TranscriptRead> {
        // Serialize the read and state installation with every producer.
        let inner = self.transcript.lock();
        let read = self.read_transcript(None)?;
        // A truncated read cannot seed a writable session's sequence chain.
        read.ensure_writable().map_err(AdapterError::Store)?;
        self.transcript.restore_entries(&read.entries, inner)?;
        Ok(read)
    }
    pub fn read_transcript(&self, max_bytes: Option<u64>) -> AdapterResult<TranscriptRead> {
        self.read_transcript_at_path(&self.transcript_path, max_bytes)
    }
    pub fn read_transcript_at_path(
        &self,
        transcript_path: &Path,
        max_bytes: Option<u64>,
    ) -> AdapterResult<TranscriptRead> {
        self.base
            .store
            .read(&self.base.key(transcript_path), max_bytes)
            .map_err(AdapterError::Store)
    }
    pub fn transcript_exists(&self) -> Option<AdapterResult<bool>> {
        let key = self.base.key(&self.transcript_path);
        Some(self.base.store.has(&key)?.map_err(AdapterError::Store))
    }
    pub fn delete_transcript(&self) -> Option<AdapterResult<()>> {
        let key = self.base.key(&self.transcript_path);
        Some(self.base.store.delete(&key)?.map_err(AdapterError::Store))
    }
    pub fn delete_session_transcripts(&self) -> Option<AdapterResult<()>> {
        let key = self.base.key(&self.transcript_path);
        Some(
            self.base
                .store
                .delete_session(&key)?
                .map_err(AdapterError::Store),
        )
    }
    /// Store replacement only; like TS, the writer's sequence is not reset.
    pub fn replace_transcript(&self, entries: &[JsValue]) -> Option<AdapterResult<()>> {
        let key = self.base.key(&self.transcript_path);
        Some(
            self.base
                .store
                .replace(&key, entries)?
                .map_err(AdapterError::Store),
        )
    }
}

enum SubagentLayout {
    /// Parent session: `<subagentsDir>/<sanitized id>.jsonl`.
    Dir(PathBuf),
    /// Sidechain writer: TS defaultSubagentPath, id not sanitized.
    Default,
}
struct WriterInner {
    sequence: u64,
    last_entry_id: Option<JsString>,
    entry_ids: HashSet<JsString>,
    recorder_domain: DomainState,
}
/// TS JsonlTranscriptWriter + SessionRuntime with an external append sink.
/// The mutex is the TS writeTail/commitTail: appends commit one at a time in
/// lock order, and the host store is called while it is held.
pub struct TranscriptWriter {
    base: Arc<KeyBase>,
    path: PathBuf,
    layout: SubagentLayout,
    inner: Mutex<WriterInner>,
    closed: AtomicBool,
    /// #1019 fence handoff: set when this writer's ledger fence moved to the
    /// session factory. Every Arc clone shares the flag, so a stale holder's
    /// append is rejected at write time instead of forking the sequence chain
    /// under the factory's replacement writer.
    retired: AtomicBool,
    #[cfg(test)]
    pub(crate) test_sources: Mutex<Option<(RecorderClock, RecorderUuid)>>,
}
pub struct SubagentHandle {
    pub subagent_id: JsString,
    pub writer: TranscriptWriter,
    pub transcript_path: PathBuf,
}
impl TranscriptWriter {
    fn new(base: Arc<KeyBase>, path: PathBuf, layout: SubagentLayout) -> Self {
        Self {
            base,
            path,
            layout,
            #[cfg(test)]
            test_sources: Mutex::new(None),
            closed: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            inner: Mutex::new(WriterInner {
                sequence: 0,
                last_entry_id: None,
                entry_ids: HashSet::new(),
                recorder_domain: DomainState::default(),
            }),
        }
    }
    /// Standalone recorder convenience constructor; integrations inject their
    /// existing session writer instead of creating another writer for the key.
    pub(super) fn for_store(store: Arc<dyn TranscriptStore>, key: TranscriptKey) -> Self {
        let base = Arc::new(KeyBase {
            project: ProjectKey {
                project_root: key.project_root,
                pilot_home: key.pilot_home,
            },
            session_id: key.session_id,
            store,
            now: Arc::new(|| super::transcript::timestamp().unwrap_or_default()),
        });
        Self::new(base, key.transcript_path, SubagentLayout::Default)
    }
    /// SessionRuntime.restore's domain half (`validateSessionDomainLog`):
    /// fold the whole log into a fresh state before anything is installed; the
    /// first invalid entry fails the restore. One shared fold for both restore
    /// paths (`restore_entries` and `restore_recorder_entries`) — the two Rust
    /// halves of the same TS call must not drift apart again (#297 P2#3).
    fn fold_domain(entries: &[JsValue]) -> RecorderResult<DomainState> {
        let mut domain = DomainState::default();
        for entry in entries {
            domain.next(entry)?;
        }
        Ok(domain)
    }
    /// #297: `true` when the caller may re-seed its own projection — the
    /// snapshot is not stale. `false` only when the snapshot is strictly older
    /// than what this writer already committed, in which case the caller must
    /// keep its live projection or the two halves disagree about which turns
    /// are live (review: `Recorder::restore` used to clear its turns anyway).
    pub(super) fn restore_recorder_entries(&self, entries: &[JsValue]) -> RecorderResult<bool> {
        let mut inner = self.lock();
        let entries = entries
            .iter()
            .map(materialize)
            .collect::<AdapterResult<Vec<_>>>()?;
        let (state, ids) = validate_event_log(&entries)?;
        let domain = Self::fold_domain(&entries)?;
        // #297 a caller's snapshot can be older than what this shared writer has
        // already committed (a producer appended between its read and this
        // restore). Rewinding sequence/last_entry_id/entry_ids here makes the
        // next append write a sequence the persisted log already contains, and
        // rewinding the domain replays later events against an earlier state.
        // Install only a strictly further-committed snapshot; an equal one is
        // what this writer already holds (a differing last_entry_id at the same
        // sequence is a foreign chain and must not fork the parent link). The
        // entry ids merge either way, so a stale id stays a duplicate.
        let installed = state.sequence > inner.sequence;
        // #297 fix-r4: only a *stale* snapshot is refused. An equal one installs
        // nothing, but the caller must still re-seed its projection: a reopen is
        // a fresh TS recorder, so pending compactions and the active turn set are
        // dropped (`restore_recorder_entries` returning `installed` alone made
        // that a silent no-op and left `commitDeferred` closing a bracket the
        // fresh recorder never opened — the frozen `restore` parity scenario in
        // `tests/compaction.rs` went red).
        let accepted = installed || state.sequence == inner.sequence;
        if installed {
            inner.sequence = state.sequence;
            inner.last_entry_id = state.last_entry_id;
            inner.recorder_domain = domain;
        }
        inner.entry_ids.extend(ids);
        Ok(accepted)
    }
    pub(super) fn append_recorder_event(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: JsObject,
        sources: Option<&(RecorderClock, RecorderUuid)>,
    ) -> RecorderResult<()> {
        self.append_with(
            session_id,
            turn_id,
            draft,
            || match sources {
                Some((clock, uuid)) => Ok((clock()?, uuid()?)),
                None => self.sources().map_err(RecorderError::from),
            },
            DomainState::advance,
        )
    }
    fn lock(&self) -> MutexGuard<'_, WriterInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn snapshot_state(&self) -> WriterState {
        let inner = self.lock();
        WriterState {
            sequence: inner.sequence,
            last_entry_id: inner.last_entry_id.clone(),
        }
    }
    /// SessionRuntime.restoreState via validateSessionEventStoreState. Entry ids
    /// already committed are kept, as in TS.
    /// #297 review (sibling, simplify): this entry point rewinds
    /// sequence/last_entry_id/domain unconditionally, unlike
    /// [`Self::restore_recorder_entries`]. That is deliberate and stays: it is
    /// the TS `restoreState` twin (`tests/fixtures/transcript-adapter`
    /// `restore-state` oracle pins the rewind) and has no production caller —
    /// only adapter tests and the session-factory negative-control injection
    /// reach it. Do not "fix" it without an oracle change and a ledger entry.
    pub fn restore_state(&self, max_sequence: f64, last_entry_id: &JsValue) -> AdapterResult<()> {
        if !is_safe_integer(max_sequence) || max_sequence < 0.0 {
            return Err(validation(
                "invalid_sequence",
                format!(
                    "Session event store sequence must be a non-negative safe integer: {}.",
                    js_value::number_string(max_sequence)
                ),
            ));
        }
        let last = match last_entry_id {
            JsValue::Null => None,
            value => Some(non_empty_string(value).cloned().ok_or_else(|| {
                validation(
                    "invalid_entry_id",
                    "Session event store lastEntryId must be null or a non-empty string.".into(),
                )
            })?),
        };
        let mut inner = self.lock();
        inner.sequence = max_sequence as u64;
        inner.last_entry_id = last;
        inner.recorder_domain = DomainState::default();
        Ok(())
    }
    /// #297 review (simplify): no stale-snapshot guard here, on purpose — the
    /// only caller (`SessionStorage::restore`) reads the log and installs it
    /// while holding the same `WriterInner` lock, so no append can land in
    /// between and the snapshot cannot be stale. Adding a guard would change
    /// nothing observable; the sibling `restore_recorder_entries` needs one
    /// because its caller reads outside this writer's lock.
    fn restore_entries(
        &self,
        entries: &[JsValue],
        mut inner: MutexGuard<'_, WriterInner>,
    ) -> AdapterResult<()> {
        // validateSessionEventLog runs over the whole log before domain checks.
        let (state, ids) = validate_event_log(entries)?;
        // Then validateSessionDomainLog; then storage.restore hydrates projections.
        let domain = Self::fold_domain(entries)?;
        // SessionRuntime.restore installs validated state before hydrate; a
        // projection error must still leave subsequent appends on this chain.
        inner.sequence = state.sequence;
        inner.last_entry_id = state.last_entry_id;
        inner.entry_ids = ids;
        inner.recorder_domain = domain;
        // Hydration runs every registered reducer over the whole log, reducer by
        // reducer, in registration order — not entry by entry: a malformed entry
        // late in the log wins over an earlier one from a later reducer.
        // `BuiltinSessionProjections.ts` registers the agent-transcript cell
        // first and WebHistory after it; inside that cell the order is
        // conversation → turnSummary → metadata → compactBoundary
        // (`AgentTranscriptProjections.ts:436-440`). So: conversation's compact
        // boundary reduce, turn summary, metadata, then WebHistory's reducers in
        // their own order — fileArtifact (reads `message.content.some`) before
        // tokenUsage (`compactMetadata.postTokens`, `WebHistoryProjections.ts:266-270`).
        // Codex #1985 r2 measured the two double-fault combinations that pin the
        // agent-transcript half of this order:
        //   metadata:null + boundary without postTokens -> `reading 'title'`
        //   replacementMessages:42 + usage:null        -> `messages.map is not a function`
        // GLM #1985 r2 asked for the WebHistory half; the golden rows
        // `metadata-before-posttok` and `message-before-posttok` pin it.
        for entry in entries {
            if is_str(field(entry, "type"), "control_boundary") {
                control_boundary_supported(entry)?;
            }
        }
        for entry in entries {
            turn_summary_supported(entry)?;
        }
        for entry in entries {
            metadata_reduce_supported(entry)?;
        }
        // WebHistory's fileArtifact reducer (`WebHistoryProjections.ts:75-88`)
        // is registered before its tokenUsage reducer
        // (`WebHistoryProjections.ts:266-270`) and walks the log once. Per entry
        // it reads `message.content.some` (via messageContainsToolResult) on
        // durable messages and `entry.artifacts.length` on `file_artifacts`
        // entries, so the first malformed entry of either kind wins over a
        // missing `compactMetadata` (Codex #1985 r3 measured both orders).
        for entry in entries {
            if is_durable_message_entry(entry) {
                validate_hydrated_message(field(entry, "message"))?;
            }
            file_artifact_supported(entry)?;
        }
        for entry in entries {
            if is_str(field(entry, "type"), "control_boundary") {
                compact_budget_supported(entry)?;
            }
        }
        Ok(())
    }
    /// Fence new appends synchronously, before awaiting the in-flight writer.
    pub fn begin_close(&self) {
        // #996: writer-close-does-not-fence keeps the writer unfenced
        // (live-session/schemata.py). Off — the original fence.
        self.closed.store(
            !crate::mutants::on("writer-close-does-not-fence"),
            Ordering::SeqCst,
        );
    }
    /// #1019 fence handoff: retire this writer when its ledger fence moves to
    /// the session factory (`SessionRecording::into_storage`). Setting the
    /// shared flag invalidates every Arc clone at once; acquiring the commit
    /// lock once afterwards drains an in-flight append before the handoff
    /// observes the log, so the factory's replacement writer restores a
    /// quiescent chain. Unlike `close`, later appends are a typed rejection.
    pub(crate) fn retire(&self) {
        self.retired.store(true, Ordering::SeqCst);
        drop(self.lock());
    }
    fn check_retired(&self) -> Result<(), AdapterError> {
        if self.retired.load(Ordering::SeqCst) {
            return Err(AdapterError::Retired(
                "#1019 this session writer handed its ledger fence to the session factory and can no longer append"
                    .into(),
            ));
        }
        Ok(())
    }
    /// JsonlTranscriptWriter.close: later appends resolve without writing,
    /// and close waits for any append that already acquired the commit lock.
    pub fn close(&self) {
        self.begin_close();
        drop(self.lock());
    }
    pub fn record_session_event(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: JsObject,
    ) -> AdapterResult<()> {
        self.append(session_id, turn_id, draft)
    }
    pub fn record_accepted_input(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        messages: Vec<JsValue>,
        metadata: Option<JsObject>,
    ) -> AdapterResult<()> {
        let mut draft = draft("accepted_input");
        draft.insert("messages".into(), JsValue::Array(messages));
        if let Some(metadata) = metadata.filter(|m| !m.is_empty()) {
            draft.insert("metadata".into(), JsValue::Object(metadata));
        }
        self.append(session_id, turn_id, draft)
    }
    pub fn record_session_metadata(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        metadata: JsValue,
    ) -> AdapterResult<()> {
        self.append_session_metadata(session_id, turn_id, metadata, false)
    }
    /// #1457 fix-r3: the request-level variant. A close racing this append is
    /// an error, not a success with no entry on disk.
    pub fn record_session_metadata_checked(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        metadata: JsValue,
    ) -> AdapterResult<()> {
        self.append_session_metadata(session_id, turn_id, metadata, true)
    }
    fn append_session_metadata(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        metadata: JsValue,
        reject_closed: bool,
    ) -> AdapterResult<()> {
        let mut draft = draft("session_metadata");
        draft.insert("metadata".into(), metadata);
        self.append_with_policy(
            session_id,
            turn_id,
            draft,
            reject_closed,
            || self.sources(),
            Self::adapter_domain,
        )
    }
    pub fn record_turn_result(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        result: JsValue,
    ) -> AdapterResult<()> {
        let mut draft = draft("turn_result");
        draft.insert("result".into(), result);
        self.append(session_id, turn_id, draft)
    }
    pub fn record_agent_status_message(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        status: &JsValue,
    ) -> AdapterResult<()> {
        self.append_agent_status(session_id, turn_id, status, false)
    }
    /// #1457 fix-r3: `record_agent_status_message` for a request path, where a
    /// close racing the append must not be reported as `recorded: true`.
    pub fn record_agent_status_message_checked(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        status: &JsValue,
    ) -> AdapterResult<()> {
        self.append_agent_status(session_id, turn_id, status, true)
    }
    fn append_agent_status(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        status: &JsValue,
        reject_closed: bool,
    ) -> AdapterResult<()> {
        let mut draft = draft("agent_status_message");
        for key in ["event", "kind", "text"] {
            draft.insert(key.into(), field(status, key).clone());
        }
        // `detail && Object.keys(detail).length > 0`.
        let detail = field(status, "detail");
        if match detail {
            JsValue::Object(o) => !o.is_empty(),
            JsValue::Array(a) => !a.is_empty(),
            JsValue::String(s) => !s.is_empty(),
            _ => false,
        } {
            draft.insert("detail".into(), detail.clone());
        }
        self.append_with_policy(
            session_id,
            turn_id,
            draft,
            reject_closed,
            || self.sources(),
            Self::adapter_domain,
        )
    }
    /// TS recordEntry → SessionRuntime.appendRecorded: the caller's envelope is
    /// kept and validated against the current state.
    pub fn record_entry(&self, entry: &JsValue) -> AdapterResult<()> {
        self.check_retired()?;
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        // #1370 stimulus entry witness, same position as append_with above.
        super::stimulus::writer_stimulus_queued_entry(self.base.store.stimulus(), entry)
            .map_err(AdapterError::Store)?;
        let mut inner = self.lock();
        self.check_retired()?;
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let entry = materialize(entry)?;
        self.commit(&mut inner, entry)
    }
    fn append(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: JsObject,
    ) -> AdapterResult<()> {
        self.append_with(
            session_id,
            turn_id,
            draft,
            || self.sources(),
            Self::adapter_domain,
        )
    }
    fn sources(&self) -> AdapterResult<(String, String)> {
        #[cfg(test)]
        if let Some((clock, uuid)) = self.test_sources.lock().unwrap().as_ref() {
            return Ok((clock()?, uuid()?));
        }
        let now = (self.base.now)();
        Ok((now, uuid().map_err(|e| AdapterError::Store(e.into()))?))
    }
    fn adapter_domain(
        domain: &mut DomainState,
        entry: &JsValue,
        undo: &mut Vec<Undo>,
    ) -> AdapterResult<()> {
        domain.advance(entry, undo).map_err(AdapterError::from)
    }
    fn append_with<E: From<AdapterError>>(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: JsObject,
        sources: impl FnOnce() -> Result<(String, String), E>,
        validate: impl FnOnce(&mut DomainState, &JsValue, &mut Vec<Undo>) -> Result<(), E>,
    ) -> Result<(), E> {
        self.append_with_policy(session_id, turn_id, draft, false, sources, validate)
    }
    /// `reject_closed` (#1457 fix-r3): a request-level append must not report
    /// success for an entry the closed writer drops. The check runs under the
    /// commit lock, so "closed before or during this append" is observed
    /// rather than raced.
    fn append_with_policy<E: From<AdapterError>>(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: JsObject,
        reject_closed: bool,
        sources: impl FnOnce() -> Result<(String, String), E>,
        validate: impl FnOnce(&mut DomainState, &JsValue, &mut Vec<Undo>) -> Result<(), E>,
    ) -> Result<(), E> {
        self.check_retired()?;
        if self.closed.load(Ordering::SeqCst) {
            return if reject_closed {
                Err(E::from(AdapterError::Closed(
                    "#1457 the session writer closed before this entry; it was not persisted"
                        .into(),
                )))
            } else {
                Ok(())
            };
        }
        // #1370 stimulus entry witness: this append queues behind the commit
        // lock (the TS writeTail) — record what arrived while a barrier holds
        // the underlying append, before blocking on the lock below.
        if let Err(error) =
            super::stimulus::writer_stimulus_queued_draft(self.base.store.stimulus(), &draft)
        {
            return Err(E::from(AdapterError::Store(error)));
        }
        let mut inner = self.lock();
        self.check_retired()?;
        if self.closed.load(Ordering::SeqCst) {
            return if reject_closed {
                Err(E::from(AdapterError::Closed(
                    "#1457 the session writer closed while this entry was queued; it was not persisted"
                        .into(),
                )))
            } else {
                Ok(())
            };
        }
        // JsValue has a Drop impl, so the object is filled in place, not moved out.
        let mut entry = materialize(&JsValue::Object(draft))?;
        let JsValue::Object(fields) = &mut entry else {
            return Err(AdapterError::Type("Session event must be an object.".into()).into());
        };
        let (created_at, entry_id) = sources()?;
        fields.insert("sessionId".into(), JsValue::String(session_id.clone()));
        fields.insert("turnId".into(), JsValue::String(turn_id.clone()));
        fields.insert(
            "sequence".into(),
            JsValue::Number((inner.sequence + 1) as f64),
        );
        fields.insert("createdAt".into(), created_at.as_str().into());
        fields.insert("entryId".into(), entry_id.as_str().into());
        fields.insert(
            "parentEntryId".into(),
            inner
                .last_entry_id
                .clone()
                .map_or(JsValue::Null, JsValue::String),
        );
        let entry = materialize(&entry)?;
        self.commit_with(&mut inner, entry, validate)
    }
    /// SessionRuntime.commit: nothing is published until the store accepted it.
    /// Projection reducers are not consulted: in TS they subscribe after the
    /// propagating persistence binding, so their failures cannot undo a commit.
    fn commit(&self, inner: &mut WriterInner, entry: JsValue) -> AdapterResult<()> {
        self.commit_with(inner, entry, Self::adapter_domain)
    }
    fn commit_with<E: From<AdapterError>>(
        &self,
        inner: &mut WriterInner,
        entry: JsValue,
        validate: impl FnOnce(&mut DomainState, &JsValue, &mut Vec<Undo>) -> Result<(), E>,
    ) -> Result<(), E> {
        validate_envelope(&entry)?;
        let seq = field(&entry, "sequence").as_number().unwrap();
        if seq <= inner.sequence as f64 {
            return Err(validation(
                "sequence_not_increasing",
                format!(
                    "Recorded session event sequence {} must be greater than current sequence {}.",
                    js_value::number_string(seq),
                    inner.sequence
                ),
            )
            .into());
        }
        let id = present(&entry, "entryId")
            .and_then(JsValue::as_string)
            .cloned();
        if let Some(id) = &id
            && inner.entry_ids.contains(id)
        {
            return Err(validation(
                "duplicate_entry_id",
                format!("Session event entryId must be unique: {}.", lossy(id)),
            )
            .into());
        }
        // #297 P2#2: TS SessionRuntime.commit validates into a new state,
        // publishes, and only then assigns this.domainState — a publish that
        // throws leaves the runtime on the old state. The in-place advance
        // holds the same boundary with the undo journal: the store failure
        // (before or after its write) rolls the writer's domain back, so the
        // next append re-validates against the state the log actually holds.
        let mut undo = Vec::new();
        if let Err(error) = validate(&mut inner.recorder_domain, &entry, &mut undo) {
            inner.recorder_domain.rollback(&mut undo);
            return Err(error);
        }
        if let Err(error) = self.base.store.append(&self.base.key(&self.path), &entry) {
            inner.recorder_domain.rollback(&mut undo);
            return Err(E::from(AdapterError::Store(error)));
        }
        drop(undo);
        inner.sequence = seq as u64;
        if let Some(id) = id {
            inner.last_entry_id = Some(id.clone());
            inner.entry_ids.insert(id);
        }
        Ok(())
    }
    fn sidechain_path(&self, subagent_id: &JsString) -> PathBuf {
        match &self.layout {
            // Only create_session_storage constructs Dir, after resolving it
            // to an absolute path. No cwd lookup is needed for these children.
            SubagentLayout::Dir(dir) => normalize(
                dir,
                &[format!("{}.jsonl", sanitize_session_id(subagent_id)).as_str()],
            ),
            SubagentLayout::Default => {
                let name = self
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let stem = match name.len().checked_sub(6) {
                    Some(at)
                        if name.is_char_boundary(at)
                            && name[at..].eq_ignore_ascii_case(".jsonl") =>
                    {
                        &name[..at]
                    }
                    _ => name.as_str(),
                };
                // TS defaultSubagentPath: join(dirname(parentPath), stem,
                // "subagents", `${id}.jsonl`). path.join is lexical — no cwd
                // lookup — so an unresolvable leading `..` survives and a
                // bare file name stays relative (its empty dirname must not
                // become a root slash).
                let parent = match self.path.parent() {
                    Some(parent) => parent.to_string_lossy().into_owned(),
                    None if self.path.is_absolute() => "/".to_owned(),
                    None => ".".to_owned(),
                };
                let subagent = format!("{}.jsonl", lossy(subagent_id));
                join_path(&[&parent, stem, "subagents", &subagent])
            }
        }
    }
    /// TS forSubagent: an independent writer with its own sequence (starting at
    /// 0 every call), appending through the same store under the parent's key.
    pub fn for_subagent(&self, subagent_id: &JsString) -> SubagentHandle {
        let path = self.sidechain_path(subagent_id);
        SubagentHandle {
            subagent_id: subagent_id.clone(),
            writer: TranscriptWriter::new(self.base.clone(), path.clone(), SubagentLayout::Default),
            transcript_path: path,
        }
    }
    pub fn relative_subagent_path(&self, subagent_id: &JsString) -> String {
        relative(
            self.path.parent().unwrap_or(Path::new("/")),
            &self.sidechain_path(subagent_id),
        )
    }
}

fn draft(kind: &str) -> JsObject {
    let mut draft = JsObject::new();
    draft.insert("type".into(), kind.into());
    draft
}
/// SessionRuntime materializeJsonValue: a JSON round trip.
fn materialize(value: &JsValue) -> AdapterResult<JsValue> {
    let text = stringify_json(value)
        .map_err(|e| AdapterError::Type(e.to_string()))?
        .ok_or_else(|| AdapterError::Type("Session event must be JSON serializable.".into()))?;
    parse_json(&text).map_err(|e| AdapterError::Type(e.to_string()))
}
fn present<'a>(entry: &'a JsValue, key: &str) -> Option<&'a JsValue> {
    entry.get(key).filter(|v| !v.is_undefined())
}
fn non_empty_string(value: &JsValue) -> Option<&JsString> {
    value
        .as_string()
        .filter(|s| !js_trim_units(s.units()).is_empty())
}
/// SessionEventLogValidation.ts:26-56 validateSessionEventLog; also restores
/// the JsonlLedgerStore writer state.
pub(super) fn validate_event_log(
    entries: &[JsValue],
) -> AdapterResult<(WriterState, HashSet<JsString>)> {
    let mut sequence = 0.0;
    let mut last = None;
    let mut ids = HashSet::new();
    for entry in entries {
        validate_envelope(entry)?;
        let seq = field(entry, "sequence").as_number().unwrap();
        if seq <= sequence {
            return Err(validation(
                "sequence_not_increasing",
                format!(
                    "Session event sequence must increase strictly: received {} after {}.",
                    js_value::number_string(seq),
                    js_value::number_string(sequence)
                ),
            ));
        }
        sequence = seq;
        if let Some(id) = present(entry, "entryId").and_then(JsValue::as_string) {
            if !ids.insert(id.clone()) {
                return Err(validation(
                    "duplicate_entry_id",
                    format!("Session event entryId must be unique: {}.", lossy(id)),
                ));
            }
            last = Some(id.clone());
        }
    }
    let state = WriterState {
        sequence: sequence as u64,
        last_entry_id: last,
    };
    Ok((state, ids))
}
/// SessionEventLogValidation.ts:93-122.
fn validate_envelope(entry: &JsValue) -> AdapterResult<()> {
    let sequence = field(entry, "sequence");
    if !sequence
        .as_number()
        .is_some_and(|n| is_safe_integer(n) && n > 0.0)
    {
        return Err(validation(
            "invalid_sequence",
            format!(
                "Session event sequence must be a positive safe integer: {}.",
                js_to_string(sequence)?
            ),
        ));
    }
    let id = present(entry, "entryId");
    if id.is_some_and(|id| non_empty_string(id).is_none()) {
        return Err(validation(
            "invalid_entry_id",
            "Session event entryId must be a non-empty string when present.".into(),
        ));
    }
    if present(entry, "parentEntryId")
        .is_some_and(|p| !matches!(p, JsValue::Null) && non_empty_string(p).is_none())
    {
        return Err(validation(
            "invalid_parent_entry_id",
            "Session event parentEntryId must be null or a non-empty string when present.".into(),
        ));
    }
    if let Some(id) = id.and_then(JsValue::as_string)
        && present(entry, "parentEntryId").and_then(JsValue::as_string) == Some(id)
    {
        return Err(validation(
            "self_parent",
            format!(
                "Session event {} cannot reference itself as parent.",
                lossy(id)
            ),
        ));
    }
    Ok(())
}
pub(super) const DOMAIN_TYPES: &[&str] = &[
    "agent_turn_enqueued",
    "agent_turn_discarded",
    "turn_started",
    "step_started",
    "step_completed",
    "context_snapshot",
    "agent_instructions",
    "model_request",
    "model_stream_event",
    "tool_call",
    "tool_result",
    "compaction_started",
    "compaction_completed",
    "compaction_failed",
    "question_started",
    "question_completed",
    "question_failed",
    "permission_started",
    "permission_completed",
    "permission_failed",
    "inbox_mutation",
    "plan_todo_plan_changed",
    "plan_todo_written",
    "plan_todo_progressed",
    "goal_changed",
];
/// Non-null object. Arrays are their own `JsValue` arm, so this is also the
/// WebHistory `isRecord` predicate (WebHistoryProjections.ts:408-410:
/// `typeof value === "object" && value !== null && !Array.isArray(value)`).
fn plain_object(value: &JsValue) -> bool {
    value.as_object().is_some()
}

/// WebHistoryProjections.ts:79,321-324: messageContainsToolResult. This is a
/// JS `.some` call, not canonical-message validation: primitive blocks are
/// allowed and a matching block stops further reads. Returns whether a
/// tool-result block was found; `validate_hydrated_message` is the `Ok(())`
/// projection of the same reads the hydrate gate needs.
pub(crate) fn message_contains_tool_result(message: &JsValue) -> AdapterResult<bool> {
    if nullish(message) {
        return Err(cannot_read(message, "content"));
    }
    let content = field(message, "content");
    if nullish(content) {
        return Err(cannot_read(content, "some"));
    }
    let JsValue::Array(blocks) = content else {
        return Err(AdapterError::Type(
            "message.content.some is not a function".into(),
        ));
    };
    for block in blocks {
        if nullish(block) {
            return Err(cannot_read(block, "type"));
        }
        if is_str(field(block, "type"), "tool_result")
            || is_str(field(block, "type"), "tool_result_reference")
        {
            return Ok(true);
        }
    }
    Ok(false)
}
fn validate_hydrated_message(message: &JsValue) -> AdapterResult<()> {
    message_contains_tool_result(message).map(|_| ())
}
/// WebHistoryProjections.ts:313-318: the three transcript entry types whose
/// `message` the fileArtifact reducer reads on hydrate.
fn is_durable_message_entry(entry: &JsValue) -> bool {
    matches!(
        lossy_value(field(entry, "type")).as_str(),
        "assistant_message" | "tool_result_message" | "durable_message"
    )
}
/// WebHistoryProjections.ts:84: the same fileArtifact reducer reads
/// `entry.artifacts.length` on every `file_artifacts` entry, so a nullish
/// `artifacts` throws before the later reducers (tokenUsage included) run.
/// `.length` on any other JSON value is `undefined`, not a throw.
fn file_artifact_supported(entry: &JsValue) -> AdapterResult<()> {
    if !is_str(field(entry, "type"), "file_artifacts") {
        return Ok(());
    }
    let artifacts = field(entry, "artifacts");
    if nullish(artifacts) {
        return Err(cannot_read(artifacts, "length"));
    }
    Ok(())
}
fn message_supported(message: &JsValue) -> bool {
    plain_object(message)
        && matches!(field(message, "content"), JsValue::Array(blocks) if blocks.iter().all(|block| {
            plain_object(block)
                && (!field(block, "type").as_string().is_some_and(|t| t.equals_str("text"))
                    || field(block, "text").as_string().is_some())
        }))
}
fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}
// TS restore (`ProjectSessionStorage.restore` → `SessionProjectionDriver.hydrate`)
// runs every registered reducer's `reduce` without catching and never runs
// `finalize`. The gates below reproduce only the reads that throw, in reducer
// order, so every shape TS restores must restore here and every shape TS
// rejects must reject with the same TypeError text (fixture rows
// `restore-shape-*` and `restore-order-*` in tests/fixtures/transcript-adapter).
/// Single-entry dispatcher for one log entry; the ports themselves run each
/// projector over the whole log instead (see `restore_entries` and
/// `project_session_info`), which is what decides the error TS surfaces.
#[cfg(test)]
fn restore_entry_supported(entry: &JsValue) -> AdapterResult<()> {
    match lossy_value(field(entry, "type")).as_str() {
        "turn_result" => turn_summary_supported(entry),
        "control_boundary" => {
            control_boundary_supported(entry)?;
            compact_budget_supported(entry)
        }
        "session_metadata" => metadata_reduce_supported(entry),
        "assistant_message" | "tool_result_message" | "durable_message" => {
            validate_hydrated_message(field(entry, "message"))
        }
        "file_artifacts" => file_artifact_supported(entry),
        _ => Ok(()),
    }
}
/// turnSummary reducer, shared by both ports: `mergeUsage(state.usage,
/// entry.result.usage)` runs over `entry.result` first and reads the new
/// usage's fields, then the permissionDenials spread accepts any iterable (a
/// string spreads into code points, an object throws). `errors` is never read
/// here — only the conversation finalize reads it, and only on the list port.
fn turn_summary_supported(entry: &JsValue) -> AdapterResult<()> {
    if !is_str(field(entry, "type"), "turn_result") {
        return Ok(());
    }
    let result = projection_result(entry)?;
    let usage = field(result, "usage");
    if nullish(usage) {
        return Err(cannot_read(usage, "inputTokens"));
    }
    if !js_iterable(field(result, "permissionDenials")) {
        return Err(AdapterError::Type(
            "entry.result.permissionDenials is not iterable".into(),
        ));
    }
    Ok(())
}
/// metadata reducer: `mergeMetadata` reads `second.title` on every metadata
/// entry, so a nullish `metadata` throws before the merged value is trimmed.
fn metadata_reduce_supported(entry: &JsValue) -> AdapterResult<()> {
    if !is_str(field(entry, "type"), "session_metadata") {
        return Ok(());
    }
    let metadata = field(entry, "metadata");
    if nullish(metadata) {
        return Err(cannot_read(metadata, "title"));
    }
    Ok(())
}
/// TS listSessions (`projectSessionInfo` → `replayTranscriptEntries` →
/// `projectAgentTranscriptEntries`) runs every agent-transcript reducer's
/// `reduce` **and** `finalize`, then reads `metadata.title?.trim()` /
/// `metadata.aiTitle?.trim()`. Web-history reducers do not run here, so
/// `compactMetadata` is never read on this port.
///
/// `before_boundary` is the conversation finalize's `beforeBoundary` flag: the
/// compact boundary discards every history entry before it, so `cloneTurnResult`
/// never runs for those turn results. Message entries are still read there (the
/// `metadata?.compactReplacement` probe runs before the flag), so only the
/// `turn_result` arm is skipped.
fn list_conversation_entry_supported(entry: &JsValue, before_boundary: bool) -> AdapterResult<()> {
    let kind = lossy_value(field(entry, "type"));
    let ok = match kind.as_str() {
        "accepted_input" => {
            matches!(field(entry, "messages"), JsValue::Array(m) if m.iter().all(message_supported))
        }
        "assistant_message" | "tool_result_message" | "durable_message" => {
            message_supported(field(entry, "message"))
        }
        "turn_result" => {
            // Conversation finalize (cloneTurnResult) reads the result:
            // `{...result.usage}` never throws, then permissionDenials.map and
            // errors?.map. The turnSummary reduce that reads usage runs later.
            if before_boundary {
                return Ok(());
            }
            let result = projection_result(entry)?;
            let denials = field(result, "permissionDenials");
            if !matches!(denials, JsValue::Array(_)) {
                return Err(if nullish(denials) {
                    cannot_read(denials, "map")
                } else {
                    AdapterError::Type("result.permissionDenials.map is not a function".into())
                });
            }
            let errors = field(result, "errors");
            if !nullish(errors) && !matches!(errors, JsValue::Array(_)) {
                return Err(AdapterError::Type(
                    "result.errors?.map is not a function".into(),
                ));
            }
            true
        }
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(projection_unavailable(format!(
            "Session projection of this {kind} entry shape is not implemented (A7.5)."
        )))
    }
}
/// `projectSessionInfo`'s summary line, evaluated on the **merged** metadata
/// after every projection has run: `metadata.title?.trim()` first, then
/// `metadata.aiTitle?.trim()` only when the trimmed title is falsy. Optional
/// chaining short-circuits on `null` as well as `undefined`, so a `null` title
/// or `null` aiTitle is simply "no text"; only a present non-string value
/// reaches `.trim` and throws.
fn list_title_supported(title: &Option<JsValue>, ai_title: &Option<JsValue>) -> AdapterResult<()> {
    let blank = match title {
        None => true,
        Some(value) if nullish(value) => true,
        Some(JsValue::String(text)) => js_trim_units(text.units()).is_empty(),
        Some(_) => {
            return Err(AdapterError::Type(
                "metadata.title?.trim is not a function".into(),
            ));
        }
    };
    if blank && matches!(ai_title, Some(value) if !nullish(value) && value.as_string().is_none()) {
        return Err(AdapterError::Type(
            "metadata.aiTitle?.trim is not a function".into(),
        ));
    }
    Ok(())
}
/// Conversation reducer: the index of the last compact boundary whose
/// `readCompactSnapshot(entry) ?? replacementMessages` is truthy. `finalize`
/// skips every entry before it, so a malformed turn result that the compact
/// boundary discarded never reaches `cloneTurnResult`.
fn last_compact_boundary_index(entries: &[JsValue]) -> AdapterResult<isize> {
    let mut last: isize = -1;
    for (index, entry) in entries.iter().enumerate() {
        if !is_str(field(entry, "type"), "control_boundary") {
            continue;
        }
        control_boundary_supported(entry)?;
        let boundary = field(entry, "boundary");
        if !is_str(field(boundary, "kind"), "compact")
            || !is_str(field(boundary, "subtype"), "compact_boundary")
        {
            continue;
        }
        let replacement = match compact_snapshot_messages(boundary)? {
            Some(messages) => messages,
            None => field(boundary, "replacementMessages"),
        };
        if js_truthy(replacement) {
            last = index as isize;
        }
    }
    Ok(last)
}
/// JS truthiness for the values a JSON transcript can carry.
fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(flag) => *flag,
        JsValue::Number(number) => *number != 0.0 && !number.is_nan(),
        JsValue::String(text) => !text.units().is_empty(),
        _ => true,
    }
}
/// A JS iterable in the JSON value model: arrays and strings.
fn js_iterable(value: &JsValue) -> bool {
    matches!(value, JsValue::Array(_)) || value.as_string().is_some()
}
/// `entry.result` as read by both ports; a nullish result throws on `.usage`
/// before any shape check.
fn projection_result(entry: &JsValue) -> AdapterResult<&JsValue> {
    let result = field(entry, "result");
    if nullish(result) {
        return Err(cannot_read(result, "usage"));
    }
    Ok(result)
}
/// The JS TypeError message for a property read on null or undefined.
fn cannot_read(value: &JsValue, property: &str) -> AdapterError {
    AdapterError::Type(format!(
        "Cannot read properties of {} (reading '{property}')",
        if matches!(value, JsValue::Null) {
            "null"
        } else {
            "undefined"
        }
    ))
}
/// control_boundary reads of the default registry in registration order: the
/// conversation reducer (AgentTranscriptProjections.ts:105-135) first, then —
/// on the restore port only — web-history token usage, which reads
/// `compactMetadata` (WebHistoryProjections.ts:379-381;
/// `compact_budget_supported` covers that pass). Shapes the reducers ignore pass on both
/// ports; the checks below reproduce only what throws. Live appends go through
/// `drive`, which catches reducer errors instead.
fn control_boundary_supported(entry: &JsValue) -> AdapterResult<()> {
    let boundary = field(entry, "boundary");
    // Conversation reducer: `entry.boundary.kind` runs before any shape check.
    if nullish(boundary) {
        return Err(cannot_read(boundary, "kind"));
    }
    if boundary.as_object().is_none() {
        // Primitives and arrays: `.kind` reads undefined and every reducer
        // ignores the entry.
        return Ok(());
    }
    if !is_str(field(boundary, "kind"), "compact") {
        // resume/manual and unknown kinds never reach the compact logic.
        return Ok(());
    }
    if !boundary_has_compact_subtype(boundary) {
        // Microcompact and unknown subtypes never reach the compact logic.
        return Ok(());
    }
    // Conversation reducer: `readCompactSnapshot(entry) ?? replacementMessages`
    // followed by cloneMessages — a truthy non-array replacement, a null
    // replacement message, or a tool_result block with a non-array content
    // throws. A valid snapshot short-circuits the replacementMessages read.
    if compact_snapshot_messages(boundary)?.is_none()
        && let Some(error) = replacement_clone_error(field(boundary, "replacementMessages"))
    {
        return Err(error);
    }
    Ok(())
}
/// WebHistory's `projectCompactBudget` runs in a **later** pass than the
/// conversation reducer above and than the turn-summary, metadata and
/// fileArtifact reducers (registration order: conversation, turnSummary,
/// metadata, compactBoundary, subagentReferences, fileHistory, WebHistory's
/// fileArtifact → agentStatus → turnError → tokenUsage → incompleteTurn —
/// `BuiltinSessionProjections.ts` plus `AgentTranscriptProjections.ts:436-440`
/// and `WebHistoryProjections.ts:266-270`). It reads
/// `positiveNumber(compactMetadata.postTokens)` for the same subtype, so a null
/// `compactMetadata` throws only after every pass above has scanned the whole
/// log. `listSessions` never runs WebHistory, so the list port does not call
/// this.
fn compact_budget_supported(entry: &JsValue) -> AdapterResult<()> {
    let boundary = field(entry, "boundary");
    if boundary.as_object().is_none()
        || !is_str(field(boundary, "kind"), "compact")
        || !boundary_has_compact_subtype(boundary)
    {
        return Ok(());
    }
    let metadata = field(boundary, "compactMetadata");
    if nullish(metadata) {
        return Err(cannot_read(metadata, "postTokens"));
    }
    Ok(())
}
fn boundary_has_compact_subtype(boundary: &JsValue) -> bool {
    boundary
        .as_object()
        .is_some_and(|object| object.contains_key("subtype"))
        && is_str(field(boundary, "subtype"), "compact_boundary")
}
/// readCompactSnapshot (CompactSnapshot.ts:5-27) restricted to a boundary
/// already known to be `kind: "compact", subtype: "compact_boundary"`:
/// Some(messages) only for a version-1 snapshot with non-empty, fully valid
/// messages. The `String(block.type)` coercion inside the tool_result check
/// can itself throw, exactly as in TS.
fn compact_snapshot_messages(boundary: &JsValue) -> AdapterResult<Option<&JsValue>> {
    let snapshot = field(boundary, "snapshot");
    if snapshot.as_object().is_none() || field(snapshot, "version").as_number() != Some(1.0) {
        return Ok(None);
    }
    let JsValue::Array(messages) = field(snapshot, "messages") else {
        return Ok(None);
    };
    if messages.is_empty() || !snapshot_messages_valid(messages)? {
        return Ok(None);
    }
    Ok(Some(field(snapshot, "messages")))
}
fn snapshot_messages_valid(messages: &[JsValue]) -> AdapterResult<bool> {
    for message in messages {
        if message.as_object().is_none()
            || !(is_str(field(message, "role"), "user")
                || is_str(field(message, "role"), "assistant"))
            || !(field(message, "metadata").is_undefined()
                || field(message, "metadata").as_object().is_some())
        {
            return Ok(false);
        }
        let JsValue::Array(blocks) = field(message, "content") else {
            return Ok(false);
        };
        for block in blocks {
            if !snapshot_block_valid(block)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
/// isContentBlock (CompactSnapshot.ts:16-47).
fn snapshot_block_valid(block: &JsValue) -> AdapterResult<bool> {
    if block.as_object().is_none() {
        return Ok(false);
    }
    let text = |key: &str| field(block, key).as_string().is_some();
    Ok(
        match field(block, "type")
            .as_string()
            .and_then(|s| s.to_utf8().ok())
            .as_deref()
        {
            Some("text" | "thinking") => text("text"),
            Some("image" | "audio") => {
                (is_str(field(block, "source"), "base64") || is_str(field(block, "source"), "url"))
                    && text("data")
                    && text("mimeType")
            }
            Some("pdf") => {
                is_str(field(block, "source"), "base64")
                    && text("data")
                    && is_str(field(block, "mimeType"), "application/pdf")
                    && field(block, "bytes").as_number().is_some()
            }
            Some("tool_call") => text("id") && text("name"),
            Some("tool_result") => {
                if !text("toolCallId") {
                    return Ok(false);
                }
                let JsValue::Array(content) = field(block, "content") else {
                    return Ok(false);
                };
                for inner in content {
                    if inner.as_object().is_none() {
                        return Ok(false);
                    }
                    checked_string(field(inner, "type"))?;
                    if !["text", "image", "pdf"]
                        .iter()
                        .any(|t| is_str(field(inner, "type"), t))
                        || !snapshot_block_valid(inner)?
                    {
                        return Ok(false);
                    }
                }
                true
            }
            Some("tool_result_reference" | "media_reference") => {
                if !(text("path")
                    && field(block, "originalBytes").as_number().is_some()
                    && text("preview")
                    && field(block, "hasMore").as_bool().is_some())
                {
                    return Ok(false);
                }
                if is_str(field(block, "type"), "tool_result_reference") {
                    text("toolCallId")
                } else if text("mimeType") {
                    checked_string(field(block, "mediaType"))?;
                    ["image", "pdf", "audio"]
                        .iter()
                        .any(|t| is_str(field(block, "mediaType"), t))
                } else {
                    false
                }
            }
            _ => false,
        },
    )
}
/// The `String(...)` coercion inside isContentBlock throws for an object
/// carrying a `toString` property (same convention as replay.rs and js_value
/// `NotPrimitive`), and throws `RangeError` once the array nesting passes the
/// shared 2000-container bound (V8's own limit is higher and stack-dependent;
/// that band is the human-approved A6-JSVALUE-DEPTH-001 divergence); a plain
/// object just stringifies and fails the membership check, which the caller
/// reports as an invalid block. The walk must be the bounded one (#1550 P0):
/// the recursive truthiness helper aborts the process on a deeply nested
/// transcript line, while V8 raises a catchable `RangeError` that these
/// callers surface as `AdapterError::Range`.
fn checked_string(value: &JsValue) -> AdapterResult<()> {
    match js_value::string_coercible_checked(value) {
        Ok(_) => Ok(()),
        Err(error) => Err(match error {
            js_value::ToStringError::NotPrimitive => {
                AdapterError::Type(js_value::ToStringError::NOT_PRIMITIVE.into())
            }
            js_value::ToStringError::StackOverflow => {
                AdapterError::Range(js_value::StackOverflow::MESSAGE.into())
            }
        }),
    }
}
/// JS truthiness: the `if (!replacement)` guard runs before cloneMessages.
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
/// cloneMessages(replacement) (model/protocol/clone.ts:50 with
/// cloneContentBlock :21-48): `.map` on a truthy non-array replacement throws
/// before any state changes; a nullish replacement message, a null block, or
/// a tool_result block with a non-array content throws while cloning.
fn replacement_clone_error(replacement: &JsValue) -> Option<AdapterError> {
    if !truthy(replacement) {
        return None;
    }
    let JsValue::Array(messages) = replacement else {
        return Some(AdapterError::Type("messages.map is not a function".into()));
    };
    for message in messages {
        if nullish(message) {
            return Some(cannot_read(message, "content"));
        }
        if let JsValue::Array(blocks) = field(message, "content") {
            for block in blocks {
                if nullish(block) {
                    return Some(cannot_read(block, "type"));
                }
                if is_str(field(block, "type"), "tool_result") {
                    let content = field(block, "content");
                    if nullish(content) {
                        return Some(cannot_read(content, "map"));
                    }
                    if !matches!(content, JsValue::Array(_)) {
                        return Some(AdapterError::Type(
                            "tr.content.map is not a function".into(),
                        ));
                    }
                }
            }
        }
    }
    None
}

/// TS projectSessionInfo (AsyncTranscriptStorageAdapter.ts:444-478).
fn project_session_info(
    record: &StoredSession,
    transcript: &TranscriptRead,
) -> AdapterResult<Option<JsValue>> {
    if transcript.entries.is_empty() || transcript.diagnostics.iter().any(|d| d.severity == "error")
    {
        return Ok(None);
    }
    // projectAgentTranscriptEntries runs one projection at a time over the whole
    // log, so the error that surfaces is decided by projection order, not by
    // entry order: conversation reduce → conversation finalize → turnSummary →
    // metadata → the trims below.
    let last_boundary = last_compact_boundary_index(&transcript.entries)?;
    for (index, entry) in transcript.entries.iter().enumerate() {
        let before_boundary = last_boundary != -1 && (index as isize) < last_boundary;
        list_conversation_entry_supported(entry, before_boundary)?;
    }
    for entry in &transcript.entries {
        turn_summary_supported(entry)?;
    }
    for entry in &transcript.entries {
        metadata_reduce_supported(entry)?;
    }
    // mergeMetadata: title keeps the previous value when the new one is nullish;
    // other keys follow object spread (a present key overrides, null included).
    let mut title: Option<JsValue> = None;
    let mut spread: [(&str, Option<JsValue>); 4] = [
        ("aiTitle", None),
        ("tag", None),
        ("parentSessionId", None),
        ("forkedFromTurnId", None),
    ];
    let mut prompts: Vec<JsString> = Vec::new();
    for entry in &transcript.entries {
        let kind = field(entry, "type");
        if kind
            .as_string()
            .is_some_and(|k| k.equals_str("session_metadata"))
        {
            let metadata = field(entry, "metadata");
            if !nullish(field(metadata, "title")) {
                title = Some(field(metadata, "title").clone());
            }
            for (key, value) in &mut spread {
                if let Some(v) = metadata.get(key) {
                    *value = Some(v.clone());
                }
            }
        } else if kind
            .as_string()
            .is_some_and(|k| k.equals_str("accepted_input"))
            && let JsValue::Array(messages) = field(entry, "messages")
        {
            for message in messages {
                if let JsValue::Array(blocks) = field(message, "content") {
                    for block in blocks {
                        if field(block, "type")
                            .as_string()
                            .is_some_and(|t| t.equals_str("text"))
                            && let Some(text) = field(block, "text").as_string()
                        {
                            let trimmed = js_trim_units(text.units());
                            if !trimmed.is_empty() {
                                prompts.push(JsString::from_utf16(trimmed.to_vec()));
                            }
                        }
                    }
                }
            }
        }
    }
    let trimmed = |value: &Option<JsValue>| {
        value
            .as_ref()
            .and_then(JsValue::as_string)
            .map(|s| JsString::from_utf16(js_trim_units(s.units()).to_vec()))
            .filter(|s| !s.is_empty())
    };
    let ai_title = spread[0].1.clone();
    // The two trims read the merged metadata, after every projection has run.
    list_title_supported(&title, &ai_title)?;
    let Some(summary) = trimmed(&title)
        .or_else(|| trimmed(&ai_title))
        .or_else(|| prompts.last().cloned())
        .or_else(|| prompts.first().cloned())
    else {
        return Ok(None);
    };
    let created_at = match field(&transcript.entries[0], "createdAt").as_string() {
        Some(text) => pilotdeck_transport::system_clock::parse_node22_date(text)
            .map_err(|e| projection_unavailable(lossy(&e)))?
            .unwrap_or(f64::NAN),
        None => f64::NAN,
    };
    let undefined = JsValue::Undefined;
    let mut info = JsObject::new();
    info.insert(
        "sessionId".into(),
        JsValue::String(record.session_id.clone()),
    );
    info.insert("summary".into(), JsValue::String(summary));
    info.insert("lastModified".into(), JsValue::Number(record.last_modified));
    info.insert(
        "fileSize".into(),
        record.file_size.map_or(JsValue::Undefined, JsValue::Number),
    );
    info.insert("customTitle".into(), title.unwrap_or(JsValue::Undefined));
    info.insert("aiTitle".into(), ai_title.unwrap_or(JsValue::Undefined));
    info.insert(
        "firstPrompt".into(),
        prompts
            .first()
            .cloned()
            .map_or(JsValue::Undefined, JsValue::String),
    );
    info.insert(
        "tag".into(),
        spread[1].1.clone().unwrap_or(undefined.clone()),
    );
    info.insert("createdAt".into(), JsValue::Number(created_at));
    info.insert(
        "parentSessionId".into(),
        spread[2].1.clone().unwrap_or(undefined.clone()),
    );
    info.insert(
        "forkedFromTurnId".into(),
        spread[3].1.clone().unwrap_or(undefined),
    );
    Ok(Some(JsValue::Object(info)))
}

/// TS `SubagentReferenceProjectionResult` (AgentTranscriptProjections.ts:72-77):
/// the `agent-transcript.subagent-references` cell. `started`/`completed` keep
/// every matching entry in log order; the two `*_by_id` indexes are the JS
/// `Map`s, materialised in insertion order because a `Map` is not a JSON value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubagentReferences {
    /// `subagent_started` entries in log order, duplicates included.
    pub started: Vec<JsValue>,
    /// `subagent_completed` entries in log order, duplicates included.
    pub completed: Vec<JsValue>,
    /// `startedById`: the **first** entry per `subagentId`, keys in first-seen
    /// order (`AgentTranscriptProjections.ts:392-395`).
    pub started_by_id: Vec<(JsValue, JsValue)>,
    /// `completedById`: the **last** entry per `subagentId`, keys in first-seen
    /// order (`AgentTranscriptProjections.ts:400-403`; `Map.set` on an existing
    /// key updates the value and keeps the original position).
    pub completed_by_id: Vec<(JsValue, JsValue)>,
}
impl SubagentReferences {
    /// `startedById.get(subagentId)` — the sidechain lookup `readSessionMessages`
    /// uses (readSessionMessages.ts:290).
    pub fn started_entry(&self, subagent_id: &JsValue) -> Option<&JsValue> {
        self.started_by_id
            .iter()
            .find(|(key, _)| same_value_zero(key, subagent_id))
            .map(|(_, entry)| entry)
    }
    /// `completedById.get(subagentId)`.
    pub fn completed_entry(&self, subagent_id: &JsValue) -> Option<&JsValue> {
        self.completed_by_id
            .iter()
            .find(|(key, _)| same_value_zero(key, subagent_id))
            .map(|(_, entry)| entry)
    }
}
/// ECMAScript `SameValueZero`, restricted to what a JSON-parsed transcript can
/// express. Numbers compare by value (so `NaN` is one key), strings/booleans/
/// `null`/`undefined` compare by value, and a container key never equals
/// another container: the TS `Map` compares object keys by *reference*, and two
/// entries of a JSON-parsed log can never share one. Container keys are also
/// unreachable from the typed log (`subagentId: string`).
fn same_value_zero(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Number(left), JsValue::Number(right)) => {
            left == right || (left.is_nan() && right.is_nan())
        }
        (JsValue::Object(_) | JsValue::Array(_), _)
        | (_, JsValue::Object(_) | JsValue::Array(_)) => false,
        (left, right) => left == right,
    }
}
/// TS `projectSubagentReferences` (AgentTranscriptProjections.ts:449-456) over
/// the `agent-transcript.subagent-references` reducer (:380-408).
///
/// The reducer reads only `entry.type` and `entry.subagentId` and validates
/// nothing else: missing/`null` ids, non-string ids and truthy `errored` fields
/// all project as-is. Shapes it ignores (`subagent_started ` with a trailing
/// space, primitives, any other entry type) are skipped, not rejected — so both
/// admission doors, `restore_entry_supported` and
/// `list_conversation_entry_supported`, keep letting these two types through
/// (`_ => Ok(())` / `_ => true`). The one throw is the V8 property read every
/// reducer shares: a
/// `null`/`undefined` log element. `validate_event_log` rejects that entry
/// before any projection runs, so it is unreachable through restore/list and is
/// modelled here only so a direct caller cannot silently differ from TS.
pub fn project_subagent_references(entries: &[JsValue]) -> AdapterResult<SubagentReferences> {
    let mut references = SubagentReferences::default();
    for entry in entries {
        if matches!(entry, JsValue::Null | JsValue::Undefined) {
            return Err(cannot_read(entry, "type"));
        }
        let kind = field(entry, "type");
        let started = is_str(kind, "subagent_started");
        if !started && !is_str(kind, "subagent_completed") {
            continue;
        }
        let subagent_id = field(entry, "subagentId").clone();
        if started {
            let first = !references
                .started_by_id
                .iter()
                .any(|(key, _)| same_value_zero(key, &subagent_id));
            if first {
                references
                    .started_by_id
                    .push((subagent_id.clone(), entry.clone()));
            } else if crate::mutants::on("subagent-started-index-last") {
                // #2042 negative control: the index keeps the last entry per id.
                if let Some(slot) = references
                    .started_by_id
                    .iter_mut()
                    .find(|(key, _)| same_value_zero(key, &subagent_id))
                {
                    slot.1 = entry.clone();
                }
            }
            if crate::mutants::on("subagent-drop-started") {
                // #2042 negative control: the row list loses every entry.
            } else if crate::mutants::on("subagent-started-first-only") && !first {
                // #2042 negative control: only the first row per id survives.
            } else {
                references.started.push(entry.clone());
            }
        } else {
            let repeat = references
                .completed_by_id
                .iter()
                .any(|(key, _)| same_value_zero(key, &subagent_id));
            if repeat {
                // `Map.set` updates in place: the key keeps its first position.
                if !crate::mutants::on("subagent-completed-index-first") {
                    // #2042 negative control: keep the first entry instead.
                    if let Some(slot) = references
                        .completed_by_id
                        .iter_mut()
                        .find(|(key, _)| same_value_zero(key, &subagent_id))
                    {
                        slot.1 = entry.clone();
                    }
                }
            } else {
                references
                    .completed_by_id
                    .push((subagent_id, entry.clone()));
            }
            if !(crate::mutants::on("subagent-completed-deduped") && repeat) {
                references.completed.push(entry.clone());
            }
        }
    }
    Ok(references)
}

/// TS `WebHistoryProjectionResult` (WebHistoryProjections.ts:48-54): the five
/// `web-history.*` cells `projectWebHistory` (:279-311) exposes to the host.
/// The two Set-typed states are projection-internal, so every field holds the
/// finalized value the driver's `snapshot` returns.
#[derive(Clone, Debug, PartialEq)]
pub struct WebHistoryView {
    /// `fileArtifacts`: finalize rows `{...entry, artifacts: [...artifacts]}`.
    pub file_artifacts: Vec<JsValue>,
    /// `agentStatuses`: the raw reduce state (this cell has no finalize).
    pub agent_statuses: Vec<JsValue>,
    /// `turnErrors`: finalize-filtered `turn_result` error rows.
    pub turn_errors: Vec<JsValue>,
    /// `tokenUsage`: `{latestContextBudget?, latestCompactBudget?,
    /// latestTurnUsage?}` (no finalize).
    pub token_usage: JsValue,
    /// `incompleteTurnIds`: the finalize-filtered durable-message turn ids.
    pub incomplete_turn_ids: Vec<JsValue>,
}
impl Default for WebHistoryView {
    fn default() -> Self {
        Self {
            file_artifacts: Vec::new(),
            agent_statuses: Vec::new(),
            turn_errors: Vec::new(),
            token_usage: JsValue::Undefined,
            incomplete_turn_ids: Vec::new(),
        }
    }
}
/// TS `value.length` as the ECMAScript property read sees it: arrays and
/// strings carry the property, an object carries its own `length` key and every
/// other value reads `undefined`.
fn length_property(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Number(items.len() as f64),
        JsValue::String(text) => JsValue::Number(text.len_utf16() as f64),
        JsValue::Object(_) => field(value, "length").clone(),
        _ => JsValue::Undefined,
    }
}
/// `entry.artifacts.length > 0` (WebHistoryProjections.ts:85 and the finalize
/// re-check :95): the `.length` read throws on nullish, and `> 0` is the
/// ECMAScript relational comparison — `undefined` gives NaN (false) while a
/// string `'0x10'` coerces to 16.
fn length_positive(value: &JsValue) -> AdapterResult<bool> {
    if nullish(value) {
        return Err(cannot_read(value, "length"));
    }
    js_value::to_number(&length_property(value))
        .map(|number| number > 0.0)
        .map_err(|_| AdapterError::Range(js_value::StackOverflow::MESSAGE.into()))
}
/// TS positiveNumber (:404-406): a finite number greater than zero.
fn positive_number(value: &JsValue) -> Option<f64> {
    value
        .as_number()
        .filter(|number| number.is_finite() && *number > 0.0)
}
/// TS hasPositiveUsage (:395-402); the caller already ran the `!usage` falsy
/// guard, and no field read here can throw on a non-nullish value.
fn has_positive_usage(usage: &JsValue) -> bool {
    [
        "inputTokens",
        "outputTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
        "totalTokens",
    ]
    .iter()
    .any(|key| positive_number(field(usage, key)).is_some())
}
/// TS projectContextBudget (WebHistoryProjections.ts:327-365): an
/// `agent_status_message` `context_budget` event whose detail record carries
/// usable `used`/`total` numbers. Optional fields spread in only when their
/// `positiveNumber` read is defined.
fn project_context_budget(entry: &JsValue, index: &JsValue) -> AdapterResult<Option<JsObject>> {
    if !is_str(field(entry, "type"), "agent_status_message")
        || !is_str(field(entry, "event"), "context_budget")
    {
        return Ok(None);
    }
    let detail = field(entry, "detail");
    if !plain_object(detail) {
        return Ok(None);
    }
    let display_used = positive_number(field(detail, "displayUsed"));
    let Some(used) = display_used.or_else(|| positive_number(field(detail, "used"))) else {
        return Ok(None);
    };
    let Some(total) = positive_number(field(detail, "total")) else {
        return Ok(None);
    };
    let effective_total = positive_number(field(detail, "effectiveTotal")).unwrap_or(total);
    let mut usage = JsObject::new();
    usage.insert("used".into(), JsValue::Number(used));
    if let Some(display_used) = display_used {
        usage.insert("displayUsed".into(), JsValue::Number(display_used));
    }
    if let Some(budget_used) = positive_number(field(detail, "budgetUsed")) {
        usage.insert("budgetUsed".into(), JsValue::Number(budget_used));
    }
    usage.insert("total".into(), JsValue::Number(total));
    usage.insert("effectiveTotal".into(), JsValue::Number(effective_total));
    usage.insert(
        "reservedOutputTokens".into(),
        JsValue::Number(positive_number(field(detail, "reservedOutputTokens")).unwrap_or(0.0)),
    );
    if let Some(state) = field(detail, "state").as_string() {
        usage.insert("state".into(), JsValue::String(state.clone()));
    }
    if let Some(ratio) = field(detail, "ratio").as_number()
        && ratio.is_finite()
    {
        usage.insert("ratio".into(), JsValue::Number(ratio));
    }
    usage.insert("source".into(), JsValue::String("history".into()));
    usage.insert("exact".into(), JsValue::Bool(true));
    let mut budget = JsObject::new();
    budget.insert("index".into(), index.clone());
    budget.insert("usage".into(), JsValue::Object(usage));
    Ok(Some(budget))
}
/// TS projectCompactBudget (:367-393): a `control_boundary` compact boundary
/// whose `compactMetadata.postTokens` is a positive number. `boundary.kind` is
/// read before any shape check, so a nullish boundary throws there; `in` only
/// runs on objects because a primitive boundary reads `.kind` as `undefined`.
fn project_compact_budget(entry: &JsValue, index: &JsValue) -> AdapterResult<Option<JsObject>> {
    let boundary = field(entry, "boundary");
    if !is_str(field(entry, "type"), "control_boundary") {
        return Ok(None);
    }
    if nullish(boundary) {
        return Err(cannot_read(boundary, "kind"));
    }
    if !is_str(field(boundary, "kind"), "compact") {
        return Ok(None);
    }
    if !(boundary
        .as_object()
        .is_some_and(|object| object.contains_key("subtype"))
        && is_str(field(boundary, "subtype"), "compact_boundary"))
    {
        return Ok(None);
    }
    let metadata = field(boundary, "compactMetadata");
    if nullish(metadata) {
        return Err(cannot_read(metadata, "postTokens"));
    }
    let Some(post_tokens) = positive_number(field(metadata, "postTokens")) else {
        return Ok(None);
    };
    let mut budget = JsObject::new();
    budget.insert("index".into(), index.clone());
    budget.insert("postTokens".into(), JsValue::Number(post_tokens));
    if let Some(pre_tokens) = positive_number(field(metadata, "preTokens")) {
        budget.insert("preTokens".into(), JsValue::Number(pre_tokens));
    }
    if let Some(summarized) = positive_number(field(metadata, "messagesSummarized")) {
        budget.insert("messagesSummarized".into(), JsValue::Number(summarized));
    }
    Ok(Some(budget))
}
/// TS `projectWebHistory` (WebHistoryProjections.ts:279-311) over the web-only
/// registry (:266-277). The driver builds each cell by scanning the whole log
/// before any finalize runs (SessionProjectionDriver.ts:44-52,186-218), so the
/// read order below is pass by pass: the constructor's `lastSequence` first,
/// then fileArtifact, agentStatuses, turnErrors, tokenUsage and incompleteTurn
/// reduce, then the finalize filters. Every read that throws is modelled with
/// its V8 TypeError text (fixture rows `webHistory` in
/// tests/fixtures/session-factory; real Node v22.22.0 evidence) and every
/// shape the reducers ignore projects as-is.
pub fn project_web_history(entries: &[JsValue]) -> AdapterResult<WebHistoryView> {
    // SessionProjectionDriver.ts:47-48 with lastSequence (:232-234): the
    // constructor reads `entry.sequence` through `Math.max` before any
    // reducer, so a nullish element reports 'sequence', not 'type'.
    for entry in entries {
        if nullish(entry) {
            return Err(cannot_read(entry, "sequence"));
        }
    }
    let mut view = WebHistoryView::default();
    // #2044 negative control, one switch shared by both fileArtifact reads:
    // the tool-result turn is forgotten, so the finalize filter applies to it.
    let tool_turns_ignored = crate::mutants::on("web-history-tool-turn-ignored");
    // fileArtifact reduce (:78-89) over the whole log: the tool-result turn
    // set first, then the `file_artifacts` rows with a positive `.length`.
    let mut turns_with_tool_results: Vec<JsValue> = Vec::new();
    for entry in entries {
        if is_durable_message_entry(entry) && message_contains_tool_result(field(entry, "message"))?
        {
            let turn_id = field(entry, "turnId").clone();
            if !tool_turns_ignored
                && !turns_with_tool_results
                    .iter()
                    .any(|seen| same_value_zero(seen, &turn_id))
            {
                turns_with_tool_results.push(turn_id);
            }
        } else if is_str(field(entry, "type"), "file_artifacts")
            && length_positive(field(entry, "artifacts"))?
        {
            view.file_artifacts.push(entry.clone());
        }
    }
    // agentStatuses reduce (:124-128) reads `entry.type` only.
    for entry in entries {
        if is_str(field(entry, "type"), "agent_status_message") {
            view.agent_statuses.push(entry.clone());
        }
    }
    // turnErrors reduce (:147-164): the visible-failure set first, then the
    // error `turn_result` rows. `entry.detail?.visible !== false` reads a
    // nullish detail as `undefined !== false` (true), and a `turn_result`
    // reads `result.type` — a nullish result throws there.
    let mut visible_failure_status_turn_ids: Vec<JsValue> = Vec::new();
    for entry in entries {
        if is_str(field(entry, "type"), "agent_status_message")
            && is_str(field(entry, "kind"), "error")
            && field(field(entry, "detail"), "visible") != &JsValue::Bool(false)
        {
            let turn_id = field(entry, "turnId").clone();
            if !visible_failure_status_turn_ids
                .iter()
                .any(|seen| same_value_zero(seen, &turn_id))
            {
                visible_failure_status_turn_ids.push(turn_id);
            }
        }
        if is_str(field(entry, "type"), "turn_result") {
            let result = field(entry, "result");
            if nullish(result) {
                return Err(cannot_read(result, "type"));
            }
            if is_str(field(result, "type"), "error") {
                view.turn_errors.push(entry.clone());
            }
        }
    }
    // tokenUsage reduce (:200-215): contextBudget, then compactBudget, then
    // turn usage; `index` is the log position. hasPositiveUsage runs its
    // falsy `!usage` guard first, so a nullish usage never reaches the field
    // reads (:395-396).
    let mut latest_context_budget: Option<JsObject> = None;
    let mut latest_compact_budget: Option<JsObject> = None;
    let mut latest_turn_usage: Option<JsObject> = None;
    for (index, entry) in entries.iter().enumerate() {
        let index = JsValue::Number(index as f64);
        if let Some(budget) = project_context_budget(entry, &index)? {
            latest_context_budget = Some(budget);
            continue;
        }
        if let Some(budget) = project_compact_budget(entry, &index)? {
            latest_compact_budget = Some(budget);
            continue;
        }
        if is_str(field(entry, "type"), "turn_result") {
            let usage = field(field(entry, "result"), "usage");
            if js_truthy(usage)
                && has_positive_usage(usage)
                && let Some(object) = usage.as_object()
            {
                latest_turn_usage = Some(object.clone());
            }
        }
    }
    // fileArtifact finalize (:90-97): the first cell `snapshot` views. A turn
    // with a tool result keeps its artifacts verbatim; every other row filters
    // `workspace_diff` out, so `artifacts.filter` must exist — the only shapes
    // that reduced in past `.length > 0` without being an array are strings
    // and objects, and both report the missing method.
    let mut file_rows = Vec::new();
    for entry in &view.file_artifacts {
        let artifacts = field(entry, "artifacts");
        let turn_id = field(entry, "turnId");
        let tool_result_turn = !tool_turns_ignored
            && turns_with_tool_results
                .iter()
                .any(|seen| same_value_zero(seen, turn_id));
        let artifacts = if tool_result_turn || crate::mutants::on("web-history-workspace-diff-kept")
        {
            // #2044 negative control (second arm): the filter never runs.
            artifacts.clone()
        } else {
            let kept = match artifacts {
                JsValue::Array(items) => {
                    let mut kept = Vec::with_capacity(items.len());
                    for artifact in items {
                        if nullish(artifact) {
                            return Err(cannot_read(artifact, "source"));
                        }
                        if !is_str(field(artifact, "source"), "workspace_diff") {
                            kept.push(artifact.clone());
                        }
                    }
                    kept
                }
                _ if crate::mutants::on("web-history-finalize-nonarray-empty") => {
                    // #2044 negative control: the broken artifacts row is
                    // dropped instead of the filter call being reported.
                    Vec::new()
                }
                _ => {
                    return Err(AdapterError::Type(
                        "entry.artifacts.filter is not a function".into(),
                    ));
                }
            };
            JsValue::Array(kept)
        };
        // `artifacts.length > 0 ? [...] : []`: a filtered-out row disappears.
        if !length_positive(&artifacts)? {
            continue;
        }
        // `[...artifacts]`: an array copies its elements and a string
        // iterates code points (the shared `js_value::code_point_chunks`
        // walk, same as permission denials and the host stream events); any
        // other shape is not iterable.
        let spread = match &artifacts {
            JsValue::Array(items) => JsValue::Array(items.clone()),
            JsValue::String(text) => JsValue::Array(
                js_value::code_point_chunks(text.units())
                    .into_iter()
                    .map(|chunk| JsValue::String(JsString::from_utf16(chunk.to_vec())))
                    .collect(),
            ),
            _ => return Err(AdapterError::Type("artifacts is not iterable".into())),
        };
        // `{...entry, artifacts}`: only objects match the `file_artifacts`
        // type read, so the spread target always exists.
        let Some(mut row) = entry.as_object().cloned() else {
            continue;
        };
        row.insert("artifacts".into(), spread);
        file_rows.push(JsValue::Object(row));
    }
    view.file_artifacts = file_rows;
    // turnErrors finalize (:166-168): drop rows whose turn has a visible
    // failure status.
    if !crate::mutants::on("web-history-visible-error-kept") {
        // #2044 negative control: visible failures stay in the rows.
        view.turn_errors.retain(|entry| {
            !visible_failure_status_turn_ids
                .iter()
                .any(|seen| same_value_zero(seen, field(entry, "turnId")))
        });
    }
    // tokenUsage state: `{}` with the latest of each budget the log carried.
    let mut token_usage = JsObject::new();
    if !crate::mutants::on("web-history-context-budget-dropped")
        && let Some(budget) = latest_context_budget
    {
        // #2044 negative control: the context budget never lands.
        token_usage.insert("latestContextBudget".into(), JsValue::Object(budget));
    }
    if !crate::mutants::on("web-history-compact-budget-dropped")
        && let Some(budget) = latest_compact_budget
    {
        // #2044 negative control: the compact budget never lands.
        token_usage.insert("latestCompactBudget".into(), JsValue::Object(budget));
    }
    if !crate::mutants::on("web-history-turn-usage-dropped")
        && let Some(usage) = latest_turn_usage
    {
        // #2044 negative control: the turn usage never lands.
        token_usage.insert("latestTurnUsage".into(), JsValue::Object(usage));
    }
    view.token_usage = JsValue::Object(token_usage);
    // incompleteTurns reduce (:226-240) and finalize (:241-243): both read
    // only `entry.type` and `entry.turnId`.
    let mut durable_message_turn_ids: Vec<JsValue> = Vec::new();
    let mut completed_turn_ids: Vec<JsValue> = Vec::new();
    for entry in entries {
        if is_str(field(entry, "type"), "turn_result") {
            let turn_id = field(entry, "turnId").clone();
            if !completed_turn_ids
                .iter()
                .any(|seen| same_value_zero(seen, &turn_id))
            {
                completed_turn_ids.push(turn_id);
            }
        }
        if is_durable_message_entry(entry) {
            let turn_id = field(entry, "turnId").clone();
            if !durable_message_turn_ids
                .iter()
                .any(|seen| same_value_zero(seen, &turn_id))
            {
                durable_message_turn_ids.push(turn_id);
            }
        }
    }
    view.incomplete_turn_ids = if crate::mutants::on("web-history-incomplete-turn-kept") {
        // #2044 negative control: completed turns stay in the list.
        durable_message_turn_ids
    } else {
        durable_message_turn_ids
            .into_iter()
            .filter(|turn_id| {
                !completed_turn_ids
                    .iter()
                    .any(|seen| same_value_zero(seen, turn_id))
            })
            .collect()
    };
    Ok(view)
}

/// ProjectSessionStorage.ts:239-245 on POSIX: runs of `/` or `\` become `-`,
/// leading/trailing `-` are trimmed, empty becomes "session".
pub fn sanitize_session_id(session_id: &JsString) -> String {
    let mut units = Vec::with_capacity(session_id.len_utf16());
    let mut separator = false;
    for &unit in session_id.units() {
        if unit == b'/' as u16 || unit == b'\\' as u16 {
            if !separator {
                units.push(b'-' as u16);
            }
            separator = true;
        } else {
            units.push(unit);
            separator = false;
        }
    }
    let start = units
        .iter()
        .position(|u| *u != b'-' as u16)
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| *u != b'-' as u16)
        .map_or(start, |i| i + 1);
    if start >= end {
        "session".into()
    } else {
        String::from_utf16_lossy(&units[start..end])
    }
}
/// Node path.resolve/join for absolute bases: lexical, no symlink resolution.
fn resolve(base: &Path, parts: &[&str]) -> AdapterResult<PathBuf> {
    let path = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| {
                let (_, message) = crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None);
                AdapterError::Store(io::Error::new(error.kind(), message).into())
            })?
            .join(base)
    };
    Ok(normalize(&path, parts))
}
/// Node path.join over the segments: purely lexical, never reads cwd. An
/// unresolvable leading `..` survives (normalize would eat it), and a bare
/// file name keeps the joined path relative instead of inventing a root
/// slash.
fn join_path(segments: &[&str]) -> PathBuf {
    let mut joined = String::new();
    for segment in segments.iter().filter(|segment| !segment.is_empty()) {
        if !joined.is_empty() {
            joined.push('/');
        }
        joined.push_str(segment);
    }
    let mut output = PathBuf::new();
    for component in Path::new(&joined).components() {
        match component {
            Component::ParentDir => match output.components().next_back() {
                Some(Component::Normal(_)) => {
                    output.pop();
                }
                _ if joined.starts_with('/') => {}
                _ => output.push(".."),
            },
            Component::CurDir => {}
            component => output.push(component.as_os_str()),
        }
    }
    output
}
fn normalize(base: &Path, parts: &[&str]) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in parts.iter().filter(|p| !p.is_empty()) {
        path.push(part);
    }
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                output.pop();
            }
            Component::CurDir => {}
            component => output.push(component.as_os_str()),
        }
    }
    output
}
/// Node path.relative between two resolved absolute paths.
fn relative(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".into(); from.len() - common];
    parts.extend(
        to[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}
/// Array.prototype.slice index (ToIntegerOrInfinity, negative from the end).
pub(crate) fn slice_index(value: f64, len: usize) -> usize {
    let value = if value.is_nan() { 0.0 } else { value.trunc() };
    if value < 0.0 {
        (len as f64 + value).max(0.0) as usize
    } else {
        value.min(len as f64) as usize
    }
}
fn is_safe_integer(n: f64) -> bool {
    n.is_finite() && n.fract() == 0.0 && n.abs() <= MAX_SAFE_INTEGER
}
/// JS String(value) for JSON values.
fn js_to_string(value: &JsValue) -> AdapterResult<String> {
    js_value::display(value)
        .map_err(|_| AdapterError::Range(js_value::StackOverflow::MESSAGE.into()))
}
fn lossy(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
fn lossy_value(value: &JsValue) -> String {
    value.as_string().map(lossy).unwrap_or_default()
}
#[cfg(test)]
mod trim_tests {
    use super::*;

    /// #1244: the storage transcript path must trim with the full JS
    /// whitespace set. The 25 units are the `String.prototype.trim` set
    /// measured by scanning every UTF-16 code unit on Node v22.22.0
    /// (#1244 delivery evidence `node-probe/whitespace-probe.log`); the
    /// pre-merge private `js_trim_units` in this module had no test and
    /// could drift from the shared `runtime::memory` set unnoticed.
    const TRIM_SPACE_UNITS: [u16; 25] = [
        0x0009, 0x000A, 0x000B, 0x000C, 0x000D, 0x0020, 0x00A0, 0x1680, 0x2000, 0x2001, 0x2002,
        0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200A, 0x2028, 0x2029, 0x202F,
        0x205F, 0x3000, 0xFEFF,
    ];

    /// #1244: exhaustive pin of [`non_empty_string`] over the whole UTF-16
    /// code-unit range: a one-unit string is empty after trimming exactly
    /// when the unit is in the measured TS trim set. This routes through the
    /// shared `runtime::memory::js_trim_units`, so a dropped unit, a moved
    /// range bound or a locally rewritten predicate (Rust whitespace,
    /// ASCII-only) all drift.
    #[test]
    fn non_empty_string_pins_the_js_trim_whitespace_set() {
        let drifted: Vec<String> = (0u16..=u16::MAX)
            .filter(|&unit| {
                let value = JsValue::String(JsString::from_utf16(vec![unit]));
                non_empty_string(&value).is_some() == TRIM_SPACE_UNITS.contains(&unit)
            })
            .map(|unit| format!("U+{unit:04X}"))
            .collect();
        assert!(
            drifted.is_empty(),
            "#1244 non_empty_string drifted from the TS trim whitespace set: {drifted:?}"
        );
        // Named non-whitespace counterexamples (issue #1244): U+180E and
        // U+200B are White_Space-adjacent but not JS trim whitespace.
        for &unit in &[0x180Eu16, 0x200Bu16] {
            let value = JsValue::String(JsString::from_utf16(vec![unit]));
            assert!(
                non_empty_string(&value).is_some(),
                "#1244 U+{unit:04X} is not JS whitespace and must stay a non-empty string"
            );
        }
    }
}

#[cfg(test)]
mod restore_tests {
    use super::*;
    use std::{
        sync::{Weak, mpsc},
        thread,
        time::Duration,
    };

    struct ReadGateStore {
        entries: Mutex<Vec<JsValue>>,
        owner: Mutex<Weak<SessionStorage>>,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl TranscriptStore for ReadGateStore {
        fn append(&self, _key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
            self.entries.lock().unwrap().push(entry.clone());
            Ok(())
        }
        fn read(&self, _key: &TranscriptKey, _max: Option<u64>) -> StoreResult<TranscriptRead> {
            let owner = self.owner.lock().unwrap().upgrade().unwrap();
            // Deterministic regression assertion: the read itself must be
            // inside the commit lock, independent of thread scheduling.
            assert!(matches!(
                owner.transcript.inner.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            let entries = self.entries.lock().unwrap().clone();
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            Ok(TranscriptRead {
                entries,
                diagnostics: vec![],
            })
        }
    }

    #[test]
    fn restore_serializes_read_and_state_installation_with_append() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let store = Arc::new(ReadGateStore {
            entries: Mutex::new(vec![]),
            owner: Mutex::new(Weak::new()),
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let adapter =
            AsyncTranscriptAdapter::new(store.clone(), Arc::new(|_| "/pdrs-fixture/chats".into()));
        let storage = Arc::new(
            adapter
                .create_session_storage(SessionStorageInput {
                    project_root: "/pdrs-fixture/project".into(),
                    pilot_home: "/pdrs-fixture/home".into(),
                    session_id: "s1".into(),
                    now: Some(Arc::new(|| "2026-10-18T00:00:00.000Z".into())),
                })
                .unwrap(),
        );
        *store.owner.lock().unwrap() = Arc::downgrade(&storage);
        storage
            .transcript
            .record_session_event(&"s1".into(), &"t".into(), draft("turn_started"))
            .unwrap();
        thread::scope(|scope| {
            let restore = scope.spawn(|| storage.restore());
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let (started_tx, started_rx) = mpsc::channel();
            let storage = &storage;
            let append = scope.spawn(move || {
                started_tx.send(()).unwrap();
                let mut step = draft("step_started");
                step.insert("step".into(), JsValue::Number(1.0));
                storage
                    .transcript
                    .record_session_event(&"s1".into(), &"t".into(), step)
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            release_tx.send(()).unwrap();
            assert_eq!(restore.join().unwrap().unwrap().entries.len(), 1);
            append.join().unwrap().unwrap();
        });
        assert_eq!(storage.transcript.snapshot_state().sequence, 2);
        let entries = store.entries.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(field(&entries[1], "sequence"), &JsValue::Number(2.0));
        assert_eq!(
            field(&entries[1], "parentEntryId"),
            field(&entries[0], "entryId")
        );
        drop(entries);
        let mut step = draft("step_started");
        step.insert("step".into(), JsValue::Number(2.0));
        assert!(matches!(
            storage
                .transcript
                .record_session_event(&"s1".into(), &"t".into(), step),
            Err(AdapterError::Domain {
                code: "step_already_open",
                ..
            })
        ));
    }
}
#[cfg(test)]
mod coercion_depth_tests {
    use super::*;
    use crate::metadata::object;

    /// A transcript line is untrusted input. TS `String(block.type)` inside
    /// isContentBlock throws a RangeError once array nesting exhausts the V8
    /// stack; the port must report that catchable error instead of recursing
    /// until the Rust stack aborts the process. JSON parsing has no nesting
    /// bound (`codec::parse` is iterative), so a few hundred kilobytes of
    /// `[` reach this check from disk. The shared bound is
    /// `js_value::MAX_ARRAY_DEPTH` (2000), so the test pins both sides of it
    /// rather than one pathological depth.
    #[test]
    fn deep_snapshot_block_type_is_a_range_error_not_a_stack_overflow() {
        let entry = |depth: usize| {
            entry_with(object([
                ("type", JsValue::from("tool_result")),
                ("toolCallId", JsValue::from("c1")),
                (
                    "content",
                    JsValue::Array(vec![object([("type", nested(depth))])]),
                ),
            ]))
        };
        match restore_entry_supported(&entry(js_value::MAX_ARRAY_DEPTH + 1)) {
            Err(AdapterError::Range(message)) => {
                assert_eq!(message, js_value::StackOverflow::MESSAGE);
            }
            other => panic!("expected a RangeError, got {other:?}"),
        }
        // At the bound the coercion still succeeds, so the block is merely an
        // unknown type and the snapshot degrades like any other invalid one.
        assert!(restore_entry_supported(&entry(js_value::MAX_ARRAY_DEPTH)).is_ok());
    }
    /// `isContentBlock` also runs `String(value.mediaType)` on
    /// `media_reference` blocks (`CompactSnapshot.ts:44-50`); pin the same two
    /// sides of the shared bound for that call site.
    #[test]
    fn deep_snapshot_media_type_is_a_range_error_not_a_stack_overflow() {
        let entry = |depth: usize| {
            entry_with(object([
                ("type", JsValue::from("media_reference")),
                ("path", JsValue::from("media/p.png")),
                ("originalBytes", JsValue::Number(1.0)),
                ("preview", JsValue::from("preview")),
                ("hasMore", JsValue::Bool(false)),
                ("mimeType", JsValue::from("image/png")),
                ("mediaType", nested(depth)),
            ]))
        };
        match restore_entry_supported(&entry(js_value::MAX_ARRAY_DEPTH + 1)) {
            Err(AdapterError::Range(message)) => {
                assert_eq!(message, js_value::StackOverflow::MESSAGE);
            }
            other => panic!("expected a RangeError, got {other:?}"),
        }
        assert!(restore_entry_supported(&entry(js_value::MAX_ARRAY_DEPTH)).is_ok());
    }
    fn nested(depth: usize) -> JsValue {
        let mut deep = JsValue::from("text");
        for _ in 0..depth {
            deep = JsValue::Array(vec![deep]);
        }
        deep
    }
    /// A compact boundary whose single snapshot message holds `block`.
    fn entry_with(block: JsValue) -> JsValue {
        object([
            ("type", JsValue::from("control_boundary")),
            (
                "boundary",
                object([
                    ("kind", JsValue::from("compact")),
                    ("subtype", JsValue::from("compact_boundary")),
                    (
                        "compactMetadata",
                        object([("postTokens", JsValue::Number(1.0))]),
                    ),
                    (
                        "snapshot",
                        object([
                            ("version", JsValue::Number(1.0)),
                            (
                                "messages",
                                JsValue::Array(vec![object([
                                    ("role", JsValue::from("user")),
                                    ("content", JsValue::Array(vec![block])),
                                ])]),
                            ),
                        ]),
                    ),
                ]),
            ),
        ])
    }
}
