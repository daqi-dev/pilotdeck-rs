//! Rust form of TS `createGatewayAsyncTranscriptStorageAdapter`
//! (B/src/gateway/storage/AsyncTranscriptStorageAdapter.ts:120-414): primary
//! and sidechain transcript records go through a host-owned store, with the
//! SessionRuntime commit order of B/src/session/events/SessionRuntime.ts:46-152
//! (validate → store.append → publish state). A failed append leaves sequence,
//! parent and entry ids unpublished, so the next append reuses the sequence.
//! Not wired into the host, ProjectRuntimeRegistry or gateway methods.
//!
//! Domain validation is shared with the recorder. Full recovery projections
//! remain A7.5; unsupported reducer shapes return ProjectionUnavailable.
use super::{
    LedgerError, TranscriptRead, WriterState, invalid,
    ledger::field,
    recorder::{RecorderClock, RecorderError, RecorderResult, RecorderUuid},
    session_domain::DomainState,
    transcript::{
        LedgerLock, create_temporary, private_dirs, private_open, read_transcript_limited, uuid,
    },
};
use crate::js_value;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashSet,
    fmt, fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
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
    /// The store is retained by every Arc<TranscriptWriter> clone.
    _owner: Option<LedgerLock>,
}
impl JsonlTranscriptStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub(crate) fn open_locked(path: &Path) -> StoreResult<(Self, PathBuf)> {
        let (path, owner) = LedgerLock::acquire(path)?;
        Ok((
            Self {
                _owner: Some(owner),
                ..Self::default()
            },
            path,
        ))
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
            .open(path)?;
        let size = file.metadata()?.len();
        if size > 0 {
            let mut last = [0u8; 1];
            file.seek(SeekFrom::Start(size - 1))?;
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
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
        let mut file = private_open().append(true).create(true).open(path)?;
        file.write_all(line.as_bytes())?;
        if flush {
            file.sync_all()?;
        }
        Ok(())
    }
    fn read(&self, key: &TranscriptKey, max_bytes: Option<u64>) -> StoreResult<TranscriptRead> {
        read_transcript_limited(&key.transcript_path, max_bytes)
    }
    fn has(&self, key: &TranscriptKey) -> Option<StoreResult<bool>> {
        Some(Ok(fs::metadata(&key.transcript_path).is_ok()))
    }
    fn delete(&self, key: &TranscriptKey) -> Option<StoreResult<()>> {
        Some(match fs::remove_file(&key.transcript_path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
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
            file.write_all(lines.as_bytes())?;
            file.sync_all()?;
            fs::rename(&cleanup.0, path)?;
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
    pub fn create_session_storage(&self, input: SessionStorageInput) -> SessionStorage {
        let project = ProjectKey {
            project_root: input.project_root,
            pilot_home: input.pilot_home,
        };
        let chat_dir = resolve(&self.project_chat_dir(&project), &[]);
        let safe_id = sanitize_session_id(&input.session_id);
        let safe_id = safe_id.as_str();
        let transcript_path = resolve(&chat_dir, &[format!("{safe_id}.jsonl").as_str()]);
        let subagents_dir = resolve(&chat_dir, &[safe_id, "subagents"]);
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
        SessionStorage {
            tool_results_dir: resolve(
                &base.project.project_root,
                &[".pilotdeck", "tool-results", safe_id],
            ),
            file_history_dir: resolve(&chat_dir, &[safe_id, "file-history"]),
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
        }
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
                .read_transcript(None)
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
    closed: bool,
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
            inner: Mutex::new(WriterInner {
                sequence: 0,
                last_entry_id: None,
                entry_ids: HashSet::new(),
                closed: false,
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
    pub(super) fn restore_recorder_entries(&self, entries: &[JsValue]) -> RecorderResult<()> {
        let mut inner = self.lock();
        let entries = entries
            .iter()
            .map(materialize)
            .collect::<AdapterResult<Vec<_>>>()?;
        let (state, ids) = validate_event_log(&entries)?;
        let mut domain = DomainState::default();
        for entry in &entries {
            domain = domain.next(entry)?;
        }
        inner.sequence = state.sequence;
        inner.last_entry_id = state.last_entry_id;
        inner.entry_ids = ids;
        inner.recorder_domain = domain;
        Ok(())
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
            DomainState::next,
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
    fn restore_entries(
        &self,
        entries: &[JsValue],
        mut inner: MutexGuard<'_, WriterInner>,
    ) -> AdapterResult<()> {
        // validateSessionEventLog runs over the whole log before domain checks.
        let (state, ids) = validate_event_log(entries)?;
        // Then validateSessionDomainLog; then storage.restore hydrates projections.
        let mut domain = DomainState::default();
        for entry in entries {
            domain = domain.next(entry)?;
        }
        // SessionRuntime.restore installs validated state before hydrate; a
        // projection error must still leave subsequent appends on this chain.
        inner.sequence = state.sequence;
        inner.last_entry_id = state.last_entry_id;
        inner.entry_ids = ids;
        inner.recorder_domain = domain;
        // Hydration runs every registered reducer. Agent transcript reducers
        // run over the whole log before WebHistory's file-artifact reducer;
        // accepted_input.messages waits for conversation finalization.
        for entry in entries {
            if matches!(
                lossy_value(field(entry, "type")).as_str(),
                "turn_result" | "control_boundary" | "session_metadata"
            ) {
                projection_supported(entry)?;
            }
        }
        // WebHistory reads message.content.some even for incomplete turns.
        for entry in entries {
            if matches!(
                lossy_value(field(entry, "type")).as_str(),
                "assistant_message" | "tool_result_message" | "durable_message"
            ) {
                validate_hydrated_message(field(entry, "message"))?;
            }
        }
        Ok(())
    }
    /// JsonlTranscriptWriter.close: later appends resolve without writing.
    pub fn close(&self) {
        self.lock().closed = true;
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
        let mut draft = draft("session_metadata");
        draft.insert("metadata".into(), metadata);
        self.append(session_id, turn_id, draft)
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
        self.append(session_id, turn_id, draft)
    }
    /// TS recordEntry → SessionRuntime.appendRecorded: the caller's envelope is
    /// kept and validated against the current state.
    pub fn record_entry(&self, entry: &JsValue) -> AdapterResult<()> {
        let mut inner = self.lock();
        if inner.closed {
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
        let now = (self.base.now)();
        Ok((now, uuid().map_err(|e| AdapterError::Store(e.into()))?))
    }
    fn adapter_domain(domain: &DomainState, entry: &JsValue) -> AdapterResult<DomainState> {
        domain.next(entry).map_err(AdapterError::from)
    }
    fn append_with<E: From<AdapterError>>(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
        draft: JsObject,
        sources: impl FnOnce() -> Result<(String, String), E>,
        validate: impl FnOnce(&DomainState, &JsValue) -> Result<DomainState, E>,
    ) -> Result<(), E> {
        let mut inner = self.lock();
        if inner.closed {
            return Ok(());
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
        validate: impl FnOnce(&DomainState, &JsValue) -> Result<DomainState, E>,
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
        let domain = validate(&inner.recorder_domain, &entry)?;
        self.base
            .store
            .append(&self.base.key(&self.path), &entry)
            .map_err(AdapterError::Store)?;
        inner.recorder_domain = domain;
        inner.sequence = seq as u64;
        if let Some(id) = id {
            inner.last_entry_id = Some(id.clone());
            inner.entry_ids.insert(id);
        }
        Ok(())
    }
    fn sidechain_path(&self, subagent_id: &JsString) -> PathBuf {
        match &self.layout {
            SubagentLayout::Dir(dir) => resolve(
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
                // path.join, not resolve: an absolute-looking id stays nested.
                let parent = self.path.parent().unwrap_or(Path::new("/"));
                resolve(
                    Path::new(&format!(
                        "{}/{stem}/subagents/{}.jsonl",
                        parent.display(),
                        lossy(subagent_id)
                    )),
                    &[],
                )
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
fn plain_object(value: &JsValue) -> bool {
    value.as_object().is_some()
}
/// WebHistoryProjections.ts:79,321-324: messageContainsToolResult during
/// hydrate. This is a JS `.some` call, not canonical-message validation:
/// primitive blocks are allowed and a matching block stops further reads.
fn validate_hydrated_message(message: &JsValue) -> AdapterResult<()> {
    let property_error = |value: &JsValue, property: &str| {
        AdapterError::Type(format!(
            "Cannot read properties of {} (reading '{property}')",
            if matches!(value, JsValue::Null) {
                "null"
            } else {
                "undefined"
            }
        ))
    };
    if nullish(message) {
        return Err(property_error(message, "content"));
    }
    let content = field(message, "content");
    if nullish(content) {
        return Err(property_error(content, "some"));
    }
    let JsValue::Array(blocks) = content else {
        return Err(AdapterError::Type(
            "message.content.some is not a function".into(),
        ));
    };
    for block in blocks {
        if nullish(block) {
            return Err(property_error(block, "type"));
        }
        if is_str(field(block, "type"), "tool_result")
            || is_str(field(block, "type"), "tool_result_reference")
        {
            break;
        }
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
/// Entry shapes whose TS projection reducers (AgentTranscriptProjections.ts)
/// are modelled; anything else may throw in TS and is rejected here.
fn projection_supported(entry: &JsValue) -> AdapterResult<()> {
    let kind = lossy_value(field(entry, "type"));
    let ok = match kind.as_str() {
        "accepted_input" => {
            matches!(field(entry, "messages"), JsValue::Array(m) if m.iter().all(message_supported))
        }
        "assistant_message" | "tool_result_message" | "durable_message" => {
            message_supported(field(entry, "message"))
        }
        "turn_result" => {
            let result = field(entry, "result");
            if matches!(result, JsValue::Null | JsValue::Undefined) {
                return Err(AdapterError::Type(format!(
                    "Cannot read properties of {} (reading 'usage')",
                    if matches!(result, JsValue::Null) {
                        "null"
                    } else {
                        "undefined"
                    }
                )));
            }
            plain_object(result)
                && plain_object(field(result, "usage"))
                && matches!(field(result, "permissionDenials"), JsValue::Array(_))
                && (nullish(field(result, "errors"))
                    || matches!(field(result, "errors"), JsValue::Array(_)))
        }
        "control_boundary" => {
            let boundary = field(entry, "boundary");
            plain_object(boundary)
                && !field(boundary, "kind")
                    .as_string()
                    .is_some_and(|k| k.equals_str("compact"))
        }
        "session_metadata" => {
            let metadata = field(entry, "metadata");
            plain_object(metadata)
                && ["title", "aiTitle"].iter().all(|key| {
                    let value = field(metadata, key);
                    nullish(value) || value.as_string().is_some()
                })
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

/// TS projectSessionInfo (AsyncTranscriptStorageAdapter.ts:444-478).
fn project_session_info(
    record: &StoredSession,
    transcript: &TranscriptRead,
) -> AdapterResult<Option<JsValue>> {
    if transcript.entries.is_empty() || transcript.diagnostics.iter().any(|d| d.severity == "error")
    {
        return Ok(None);
    }
    for entry in &transcript.entries {
        projection_supported(entry)?;
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
fn resolve(base: &Path, parts: &[&str]) -> PathBuf {
    let mut path = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(base)
    };
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
fn slice_index(value: f64, len: usize) -> usize {
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
/// String.prototype.trim on UTF-16 units (all JS whitespace is in the BMP).
pub(super) fn js_trim_units(units: &[u16]) -> &[u16] {
    let space = |u: &u16| matches!(*u, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff);
    let start = units.iter().position(|u| !space(u)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !space(u))
        .map_or(start, |i| i + 1);
    &units[start..end]
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
        let storage = Arc::new(adapter.create_session_storage(SessionStorageInput {
            project_root: "/pdrs-fixture/project".into(),
            pilot_home: "/pdrs-fixture/home".into(),
            session_id: "s1".into(),
            now: Some(Arc::new(|| "2026-10-18T00:00:00.000Z".into())),
        }));
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
