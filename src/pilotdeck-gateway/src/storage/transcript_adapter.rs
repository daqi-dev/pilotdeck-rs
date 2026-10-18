//! Rust form of TS `createGatewayAsyncTranscriptStorageAdapter`
//! (B/src/gateway/storage/AsyncTranscriptStorageAdapter.ts:120-414): primary
//! and sidechain transcript records go through a host-owned store, with the
//! SessionRuntime commit order of B/src/session/events/SessionRuntime.ts:46-152
//! (validate → store.append → publish state). A failed append leaves sequence,
//! parent and entry ids unpublished, so the next append reuses the sequence.
//! Not wired into the host, ProjectRuntimeRegistry or gateway methods.
//!
//! Not ported here (fail closed, never silently accepted): SessionDomainValidation
//! state for turn/step/inbox entries, the session projection reducers beyond
//! the shapes checked by `projection_supported`, the replacement transaction,
//! fork-copy, file-history and tool-result artifact capabilities (pr4).
use super::{
    LedgerError, TranscriptRead, WriterState, invalid,
    ledger::field,
    transcript::{after_debris, create_temporary, private_dirs, read_transcript_limited, uuid},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashSet,
    fmt, fs,
    io::{self, Write},
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
    /// A TS module this adapter does not port would decide this input.
    NodeHostRequired(String),
    /// Error from the host store, propagated unchanged (TS rethrows it).
    Store(LedgerError),
}
impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { message, .. } | Self::Type(message) => f.write_str(message),
            Self::NodeHostRequired(message) => f.write_str(message),
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
fn node_only(message: String) -> AdapterError {
    AdapterError::NodeHostRequired(message)
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
/// append = JsonlSessionPersistence (debris kept, LF added, one JSON line),
/// read = readTranscript. Appends are atomic sibling replaces instead of
/// appendFile, so a failed append never leaves a partial line. No `.lock`
/// file (TS has none either): one store instance per Gateway process owns
/// its files; it serializes its own writes only.
#[derive(Default)]
pub struct JsonlTranscriptStore {
    writes: Mutex<()>,
}
impl JsonlTranscriptStore {
    pub fn new() -> Self {
        Self::default()
    }
    fn write_all(&self, path: &Path, bytes: impl FnOnce(Vec<u8>) -> Vec<u8>) -> StoreResult<()> {
        let _guard = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            private_dirs(parent)?;
        }
        let current = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        let next = bytes(current);
        let (mut file, cleanup) = create_temporary(path)?;
        file.write_all(&next)?;
        file.sync_all()?;
        fs::rename(&cleanup.0, path)?;
        Ok(())
    }
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
        self.write_all(&key.transcript_path, |current| {
            after_debris(&current, line.as_bytes())
        })
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
        Some(lines.and_then(|lines| self.write_all(&key.transcript_path, |_| lines.into_bytes())))
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
                return Err(node_only(
                    "Session listing with a NaN lastModified requires the Node host sort.".into(),
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
    /// before any state changes. Diagnostics do not fail the restore.
    pub fn restore(&self) -> AdapterResult<TranscriptRead> {
        let read = self.read_transcript(None)?;
        self.transcript.restore_entries(&read.entries)?;
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
            }),
        }
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
                    js_number(max_sequence)
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
        Ok(())
    }
    fn restore_entries(&self, entries: &[JsValue]) -> AdapterResult<()> {
        let mut sequence = 0.0;
        let mut last = None;
        let mut ids = HashSet::new();
        // validateSessionEventLog runs over the whole log before domain checks.
        for entry in entries {
            validate_envelope(entry)?;
            let seq = field(entry, "sequence").as_number().unwrap();
            if seq <= sequence {
                return Err(validation(
                    "sequence_not_increasing",
                    format!(
                        "Session event sequence must increase strictly: received {} after {}.",
                        js_number(seq),
                        js_number(sequence)
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
        // Then validateSessionDomainLog; then storage.restore hydrates projections.
        for entry in entries {
            domain_supported(entry)?;
        }
        for entry in entries {
            projection_supported(entry)?;
        }
        let mut inner = self.lock();
        inner.sequence = sequence as u64;
        inner.last_entry_id = last;
        inner.entry_ids = ids;
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
        let mut inner = self.lock();
        if inner.closed {
            return Ok(());
        }
        // JsValue has a Drop impl, so the object is filled in place, not moved out.
        let mut entry = materialize(&JsValue::Object(draft))?;
        let JsValue::Object(fields) = &mut entry else {
            return Err(AdapterError::Type(
                "Session event must be an object.".into(),
            ));
        };
        let created_at = (self.base.now)();
        let entry_id: JsString = uuid().map_err(|e| AdapterError::Store(e.into()))?.into();
        fields.insert("sessionId".into(), JsValue::String(session_id.clone()));
        fields.insert("turnId".into(), JsValue::String(turn_id.clone()));
        fields.insert(
            "sequence".into(),
            JsValue::Number((inner.sequence + 1) as f64),
        );
        fields.insert("createdAt".into(), created_at.as_str().into());
        fields.insert("entryId".into(), JsValue::String(entry_id));
        fields.insert(
            "parentEntryId".into(),
            inner
                .last_entry_id
                .clone()
                .map_or(JsValue::Null, JsValue::String),
        );
        let entry = materialize(&entry)?;
        self.commit(&mut inner, entry)
    }
    /// SessionRuntime.commit: nothing is published until the store accepted it.
    /// Projection reducers are not consulted: in TS they subscribe after the
    /// propagating persistence binding, so their failures cannot undo a commit.
    fn commit(&self, inner: &mut WriterInner, entry: JsValue) -> AdapterResult<()> {
        validate_envelope(&entry)?;
        let seq = field(&entry, "sequence").as_number().unwrap();
        if seq <= inner.sequence as f64 {
            return Err(validation(
                "sequence_not_increasing",
                format!(
                    "Recorded session event sequence {} must be greater than current sequence {}.",
                    js_number(seq),
                    inner.sequence
                ),
            ));
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
            ));
        }
        domain_supported(&entry)?;
        self.base
            .store
            .append(&self.base.key(&self.path), &entry)
            .map_err(AdapterError::Store)?;
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
                js_to_string(sequence)
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
const DOMAIN_TYPES: &[&str] = &[
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
/// SessionDomainValidation.ts:84-112 for a state that never saw turn_started:
/// turn-scoped entries are rejected (not ported), legacy turn_result passes,
/// and durable messages only dereference `message.metadata`.
fn domain_supported(entry: &JsValue) -> AdapterResult<()> {
    let kind = lossy_value(field(entry, "type"));
    if DOMAIN_TYPES.contains(&kind.as_str()) {
        return Err(node_only(format!(
            "Session domain validation for {kind} entries requires the Node host."
        )));
    }
    if matches!(
        kind.as_str(),
        "durable_message" | "assistant_message" | "tool_result_message"
    ) {
        match field(entry, "message") {
            JsValue::Undefined => {
                return Err(AdapterError::Type(
                    "Cannot read properties of undefined (reading 'metadata')".into(),
                ));
            }
            JsValue::Null => {
                return Err(AdapterError::Type(
                    "Cannot read properties of null (reading 'metadata')".into(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}
fn plain_object(value: &JsValue) -> bool {
    value.as_object().is_some()
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
        Err(node_only(format!(
            "Session projection of this {kind} entry shape requires the Node host."
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
                if let Some(v) = metadata.get(*key) {
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
            .map_err(|e| node_only(lossy(&e)))?
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
/// JS String(number).
fn js_number(n: f64) -> String {
    if n == 0.0 {
        "0".into()
    } else if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        (if n > 0.0 { "Infinity" } else { "-Infinity" }).into()
    } else {
        stringify_json(&JsValue::Number(n))
            .ok()
            .flatten()
            .unwrap_or_default()
    }
}
/// JS String(value) for JSON values.
fn js_to_string(value: &JsValue) -> String {
    match value {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Number(n) => js_number(*n),
        JsValue::String(s) => lossy(s),
        JsValue::Array(items) => items
            .iter()
            .map(|item| {
                if nullish(item) {
                    String::new()
                } else {
                    js_to_string(item)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        JsValue::Object(_) => "[object Object]".into(),
    }
}
fn lossy(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
fn lossy_value(value: &JsValue) -> String {
    value.as_string().map(lossy).unwrap_or_default()
}
/// String.prototype.trim on UTF-16 units (all JS whitespace is in the BMP).
fn js_trim_units(units: &[u16]) -> &[u16] {
    let space = |u: &u16| matches!(*u, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff);
    let start = units.iter().position(|u| !space(u)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !space(u))
        .map_or(start, |i| i + 1);
    &units[start..end]
}
