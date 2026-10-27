//! `EdgeClawMemoryService` assembly and synchronous facade (A6.6-edgeclaw-18).
//!
//! Ports `service.ts:600-672,737-774,971-1150` of the fixed TS baseline:
//!
//! * constructor path resolution (`workspaceDir`, `dataDir`, `dbPath`,
//!   `memoryDir`, default `~/.edgeclaw/memory` root), the
//!   `pipeline_state.workspaceDir` write and the extractor / indexer /
//!   retriever wiring (`:617-653`);
//! * `close`, settings, `overview` / `snapshot` (`:655-672,737-739`);
//! * `captureTurn` (message normalization + `captureL0Session`, `:741-774`);
//! * the read facade and `act` (`:971-1150`).
//!
//! The asynchronous orchestration (`flush`, `dream`, `rollbackLastDream`,
//! `retrieve*`, `runDueScheduledMaintenance`) belongs to A6.6-edgeclaw-19 and
//! lives in `service/orchestration.rs`; this file deliberately does not
//! duplicate it.

use super::{
    file_memory::{EdgeClawFileStores, FileMemoryClock, FileMemoryError, absolute},
    heartbeat::indexer::{
        CaptureL0SessionInput, HeartbeatError, HeartbeatIndexer, HeartbeatLogger, HeartbeatOptions,
    },
    llm::{
        config::{LlmOptions, MergedIndexingSettings, build_llm_config, merge_indexing_settings},
        transport::{LlmError, LlmLogger, LlmMemoryExtractor, LlmRuntime},
    },
    messages::{CaptureStrategy, MemoryMessage, NormalizeOptions, normalize_messages},
    retriever::{RandomToken, ReasoningRetriever, RetrieverClock},
    storage::{IndexingSettings, StorageError},
    text::{self, hash_text, normalize_case_trace_context_preview},
};
use crate::runtime::model::http::{HyperModelTransport, ModelHttpTransport};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// `LoggerLike` (`service.ts:59-63`). Every level is optional in TS; the
/// default implementation keeps each one a no-op.
pub trait ServiceLogger: Send + Sync {
    fn info(&self, _message: &JsString) {}
    fn warn(&self, _message: &JsString) {}
    fn error(&self, _message: &JsString) {}
}
impl ServiceLogger for () {}

/// One optional logger reaches the extractor (warn only) and the indexer
/// (info/warn), matching the three seams that share `options.logger`.
struct LoggerAdapter(Arc<dyn ServiceLogger>);
impl LlmLogger for LoggerAdapter {
    fn warn(&self, message: &JsString) {
        self.0.warn(message);
    }
}
impl HeartbeatLogger for LoggerAdapter {
    fn info(&self, message: &JsString) {
        self.0.info(message);
    }
    fn warn(&self, message: &JsString) {
        self.0.warn(message);
    }
}

/// Injected host seams. Production callers use [`Default::default`]; fixtures
/// pass a frozen clock, a scripted random token and a loopback transport.
#[derive(Default)]
pub struct ServiceDependencies {
    /// `os.homedir()` override; also selects the general workspace root.
    pub home: Option<PathBuf>,
    /// `nowIso()` for SQLite rows and trace timestamps.
    pub clock: Option<FileMemoryClock>,
    /// `Date.now()` for the retriever cache (A6.6-edgeclaw-19 consumes it).
    pub now_ms: Option<RetrieverClock>,
    /// `Math.random().toString(36).slice(2, 10)`.
    pub random_token: Option<RandomToken>,
    /// Injected HTTP transport; defaults to the production hyper client.
    pub transport: Option<Arc<dyn ModelHttpTransport>>,
    pub runtime: Option<Arc<dyn LlmRuntime>>,
    pub logger: Option<Arc<dyn ServiceLogger>>,
    /// Pre-resolved `buildLlmConfig` value; defaults to the real env/home read.
    pub llm_config: Option<JsValue>,
}

/// `EdgeClawMemoryServiceOptions` (`service.ts:66-82`).
pub struct ServiceOptions {
    pub workspace_dir: PathBuf,
    pub root_dir: Option<PathBuf>,
    pub db_path: Option<PathBuf>,
    pub memory_dir: Option<PathBuf>,
    pub capture_strategy: CaptureStrategy,
    pub include_assistant: Option<bool>,
    pub max_message_chars: Option<f64>,
    pub heartbeat_batch_size: Option<usize>,
    pub default_indexing_settings: Option<JsValue>,
    pub source: Option<String>,
    pub llm: Option<LlmOptions>,
}
impl ServiceOptions {
    pub fn new(workspace_dir: impl Into<PathBuf>) -> Self {
        Self {
            workspace_dir: workspace_dir.into(),
            root_dir: None,
            db_path: None,
            memory_dir: None,
            capture_strategy: CaptureStrategy::LastTurn,
            include_assistant: None,
            max_message_chars: None,
            heartbeat_batch_size: None,
            default_indexing_settings: None,
            source: None,
            llm: None,
        }
    }
}

/// `CaptureTurnResult` (`service.ts:84-88`).
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureTurnResult {
    pub captured: bool,
    pub normalized_messages: Vec<MemoryMessage>,
    pub session_key: String,
}
impl CaptureTurnResult {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("captured".into(), JsValue::Bool(self.captured));
        object.insert(
            "normalizedMessages".into(),
            JsValue::Array(
                self.normalized_messages
                    .iter()
                    .map(MemoryMessage::to_value)
                    .collect(),
            ),
        );
        object.insert(
            "sessionKey".into(),
            JsValue::String(self.session_key.as_str().into()),
        );
        JsValue::Object(object)
    }
}

/// `ClearMemoryScope` (`types.ts`), default `current_project`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClearMemoryScope {
    #[default]
    CurrentProject,
    AllMemory,
}

/// Any facade failure with the JS error shape the host would observe.
#[derive(Debug)]
pub enum ServiceError {
    Storage(StorageError),
    File(FileMemoryError),
    Heartbeat(HeartbeatError),
    Llm(LlmError),
}
impl ServiceError {
    pub fn js_name(&self) -> &'static str {
        match self {
            Self::File(error) => error.js_name(),
            Self::Storage(StorageError::Closed) => "Error",
            Self::Storage(_) | Self::Heartbeat(_) | Self::Llm(_) => "Error",
        }
    }
    pub fn js_message(&self) -> JsString {
        match self {
            Self::File(error) => error.js_message(),
            Self::Storage(error) => JsString::from(error.to_string()),
            Self::Heartbeat(error) => match error {
                HeartbeatError::Storage(StorageError::Closed) => {
                    JsString::from("database is not open")
                }
                HeartbeatError::File(error) => error.js_message(),
                other => JsString::from(other.to_string()),
            },
            Self::Llm(error) => error.message.clone(),
        }
    }
}
impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "{error}"),
            Self::File(error) => write!(f, "{error}"),
            Self::Heartbeat(error) => write!(f, "{error}"),
            Self::Llm(error) => write!(f, "{}", String::from_utf16_lossy(error.message.units())),
        }
    }
}
impl std::error::Error for ServiceError {}
impl From<StorageError> for ServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
impl From<FileMemoryError> for ServiceError {
    fn from(error: FileMemoryError) -> Self {
        Self::File(error)
    }
}
impl From<HeartbeatError> for ServiceError {
    fn from(error: HeartbeatError) -> Self {
        Self::Heartbeat(error)
    }
}
impl From<LlmError> for ServiceError {
    fn from(error: LlmError) -> Self {
        Self::Llm(error)
    }
}
pub type Result<T> = std::result::Result<T, ServiceError>;

/// `EdgeClawMemoryService` (`service.ts:600-1150`).
pub struct EdgeClawMemoryService {
    pub workspace_dir: PathBuf,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub memory_dir: PathBuf,
    pub default_indexing_settings: IndexingSettings,
    pub repository: Arc<EdgeClawFileStores>,
    pub extractor: Arc<LlmMemoryExtractor>,
    indexer: Mutex<HeartbeatIndexer>,
    retriever: Arc<ReasoningRetriever>,
    capture_strategy: CaptureStrategy,
    include_assistant: bool,
    max_message_chars: f64,
    source: String,
    clock: FileMemoryClock,
    random_token: RandomToken,
}

impl EdgeClawMemoryService {
    pub fn new(options: ServiceOptions) -> Result<Self> {
        Self::with_dependencies(options, ServiceDependencies::default())
    }

    /// [`Self::new`] with the host seams injected. The constructor still runs
    /// the same path resolution and assembly sequence as TS.
    pub fn with_dependencies(
        options: ServiceOptions,
        dependencies: ServiceDependencies,
    ) -> Result<Self> {
        let ServiceDependencies {
            home,
            clock,
            now_ms,
            random_token,
            transport,
            runtime,
            logger,
            llm_config,
        } = dependencies;
        let workspace_dir = absolute(&options.workspace_dir)?;
        let home = match home {
            Some(home) => home,
            None => super::storage::accessors::homedir()?,
        };
        let root_dir = match options.root_dir.as_deref() {
            Some(root) => absolute(root)?,
            None => absolute(&home.join(".edgeclaw").join("memory"))?,
        };
        let data_dir = resolve_configured_data_dir(
            options.db_path.as_deref(),
            options.memory_dir.as_deref(),
            &workspace_dir,
            &root_dir,
        )?;
        let db_path = match options.db_path.as_deref() {
            Some(path) => absolute(path)?,
            None => absolute(&data_dir.join("control.sqlite"))?,
        };
        let memory_dir = match options.memory_dir.as_deref() {
            Some(path) => absolute(path)?,
            None => absolute(&data_dir.join("memory"))?,
        };
        let default_indexing_settings = storage_settings(merge_indexing_settings(
            options.default_indexing_settings.as_ref(),
        ));
        let clock = clock.unwrap_or_else(|| Arc::new(text::now_iso));
        let now_ms: RetrieverClock = now_ms.unwrap_or_else(|| {
            Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(i64::MAX as u128) as i64
            })
        });
        let random_token: RandomToken =
            random_token.unwrap_or_else(|| Arc::new(super::retriever::default_random_token));
        let general_workspace = absolute(&home.join(".pilotdeck"))?;

        let repository = Arc::new(EdgeClawFileStores::with_workspace_clock(
            &db_path,
            &memory_dir,
            root_dir.join("global"),
            &workspace_dir,
            &general_workspace,
            clock.clone(),
        )?);
        repository.storage.set_pipeline_state(
            "workspaceDir",
            &JsValue::String(workspace_dir.to_string_lossy().into_owned().into()),
        )?;

        let config = match llm_config {
            Some(config) => config,
            None => build_llm_config(options.llm.as_ref()),
        };
        let mut extractor = LlmMemoryExtractor::new(
            config,
            transport.unwrap_or_else(|| Arc::new(HyperModelTransport::new())),
            runtime.unwrap_or_else(|| Arc::new(())),
        );
        if let Some(logger) = &logger {
            extractor = extractor.with_logger(Arc::new(LoggerAdapter(logger.clone())));
        }
        let extractor = Arc::new(extractor);
        let source = options.source.clone().unwrap_or_else(|| "edgeclaw".into());
        let mut heartbeat_options = HeartbeatOptions::new(default_indexing_settings.clone());
        heartbeat_options.batch_size = Some(options.heartbeat_batch_size.unwrap_or(30));
        heartbeat_options.source = Some(source.clone());
        heartbeat_options.logger = logger
            .as_ref()
            .map(|logger| Arc::new(LoggerAdapter(logger.clone())) as Arc<dyn HeartbeatLogger>);
        heartbeat_options.clock = Some(clock.clone());
        let indexer =
            HeartbeatIndexer::new(repository.clone(), extractor.clone(), heartbeat_options);
        let retriever = ReasoningRetriever::with_dependencies(
            repository.clone(),
            extractor.clone(),
            now_ms,
            random_token.clone(),
        );

        Ok(Self {
            workspace_dir,
            data_dir,
            db_path,
            memory_dir,
            default_indexing_settings,
            repository,
            extractor,
            indexer: Mutex::new(indexer),
            retriever: Arc::new(retriever),
            capture_strategy: options.capture_strategy,
            include_assistant: options.include_assistant.unwrap_or(true),
            max_message_chars: options.max_message_chars.unwrap_or(6000.0),
            source,
            clock,
            random_token,
        })
    }

    /// `close` (`service.ts:655-657`). Closing a closed repository raises
    /// `database is not open`, exactly like `DatabaseSync.close()`.
    pub fn close(&self) -> Result<()> {
        self.repository.close()?;
        Ok(())
    }

    pub fn get_settings(&self) -> Result<IndexingSettings> {
        Ok(self
            .repository
            .storage
            .get_indexing_settings(&self.default_indexing_settings)?)
    }

    /// `saveSettings` (`service.ts:663-668`): persist, refresh the indexer's
    /// settings, then clear the retriever's transient recall cache.
    pub fn save_settings(&self, partial: &JsValue) -> Result<IndexingSettings> {
        let settings = self
            .repository
            .storage
            .save_indexing_settings(partial, &self.default_indexing_settings)?;
        self.indexer
            .lock()
            .expect("edgeclaw service indexer mutex poisoned")
            .set_settings(settings.clone());
        self.retriever.reset_transient_state();
        Ok(settings)
    }

    pub fn overview(&self) -> Result<JsValue> {
        Ok(self.repository.get_overview()?)
    }

    pub fn snapshot(&self, limit: Option<&JsValue>) -> Result<JsValue> {
        Ok(self.repository.get_ui_snapshot(limit)?)
    }

    /// `captureTurn` (`service.ts:741-774`).
    pub fn capture_turn(
        &self,
        raw_messages: &[JsValue],
        input: CaptureTurnInput,
    ) -> Result<CaptureTurnResult> {
        let normalized_messages = normalize_messages(
            raw_messages,
            &NormalizeOptions {
                include_assistant: self.include_assistant,
                max_message_chars: self.max_message_chars,
                capture_strategy: self.capture_strategy,
            },
        );
        if normalized_messages.is_empty() {
            return Ok(CaptureTurnResult {
                captured: false,
                normalized_messages,
                session_key: input.session_key,
            });
        }
        self.indexer
            .lock()
            .expect("edgeclaw service indexer mutex poisoned")
            .capture_l0_session(CaptureL0SessionInput {
                session_key: input.session_key.clone(),
                timestamp: input.timestamp,
                messages: normalized_messages.clone(),
                source: input.source.or_else(|| Some(self.source.clone())),
            })?;
        Ok(CaptureTurnResult {
            captured: true,
            normalized_messages,
            session_key: input.session_key,
        })
    }

    pub fn list(&self, options: &JsObject) -> Result<JsValue> {
        Ok(self.repository.list_memory_entries(options)?)
    }

    pub fn get(&self, ids: &JsValue, max_lines: Option<f64>) -> Result<JsValue> {
        Ok(self
            .repository
            .get_memory_records_by_ids(ids, max_lines.unwrap_or(80.0))?)
    }

    pub fn get_user_summary(&self) -> Result<JsValue> {
        Ok(self.repository.get_user_summary()?)
    }

    pub fn get_project_meta(&self) -> Result<JsValue> {
        Ok(self.repository.get_project_meta()?)
    }

    pub fn get_workspace_mode(&self) -> &'static str {
        if self.repository.get_workspace_mode() == "general" {
            "general"
        } else {
            "single"
        }
    }

    pub fn list_readable_project_catalog(&self) -> Result<JsValue> {
        Ok(self.repository.list_readable_project_catalog()?)
    }

    pub fn get_readable_project(&self, logical_project_id: &str) -> Result<JsValue> {
        Ok(self.repository.get_readable_project(logical_project_id)?)
    }

    pub fn list_readable_project_entries(
        &self,
        logical_project_id: &str,
        options: &JsObject,
    ) -> Result<JsValue> {
        Ok(self
            .repository
            .list_readable_project_entries(logical_project_id, options)?)
    }

    pub fn update_project_meta(&self, input: &JsObject) -> Result<JsValue> {
        Ok(self.repository.edit_project_meta(input)?)
    }

    pub fn get_snapshot_version(&self) -> Result<String> {
        Ok(self.repository.get_snapshot_version()?)
    }

    /// `listCaseTraces` (`service.ts:1035-1037`): every record's raw recall
    /// preview is rendered before it leaves the facade.
    pub fn list_case_traces(&self, limit: Option<usize>) -> Result<Vec<JsValue>> {
        Ok(self
            .repository
            .storage
            .list_recent_case_traces(limit.unwrap_or(30))?
            .into_iter()
            .map(normalize_trace_preview)
            .collect())
    }

    /// `saveCaseTrace` (`service.ts:1039-1053`).
    pub fn save_case_trace(&self, record: &JsObject) -> Result<()> {
        let undefined = JsValue::Undefined;
        let field = |key: &str| record.get(key).unwrap_or(&undefined).clone();
        let started_at = match record.get("startedAt").and_then(JsValue::as_string) {
            Some(value) if !value.is_empty() => value.clone(),
            _ => JsString::from((self.clock)()),
        };
        let case_id = match record.get("caseId").and_then(JsValue::as_string) {
            Some(value) if !crate::runtime::memory::js_trim_units(value.units()).is_empty() => {
                JsString::from_utf16(crate::runtime::memory::js_trim_units(value.units()).to_vec())
            }
            _ => {
                let seed = format!(
                    "{}:{}:{}:{}",
                    template_text(record.get("sessionKey").unwrap_or(&undefined)),
                    template_text(record.get("query").unwrap_or(&undefined)),
                    started_at.to_utf8().unwrap_or_default(),
                    (self.random_token)().to_utf8().unwrap_or_default(),
                );
                JsString::from(format!("case_trace_{}", hash_text(&JsString::from(seed))))
            }
        };
        // Object-literal insertion order from `service.ts:1042-1052`; undefined
        // fields are dropped by `JSON.stringify`, so the serialized state and
        // the TS-created SQLite rows stay byte-identical.
        let mut next = JsObject::new();
        next.insert("caseId".into(), JsValue::String(case_id));
        next.insert("sessionKey".into(), field("sessionKey"));
        next.insert("query".into(), field("query"));
        next.insert("startedAt".into(), JsValue::String(started_at));
        next.insert("finishedAt".into(), field("finishedAt"));
        next.insert("status".into(), field("status"));
        next.insert("retrieval".into(), field("retrieval"));
        next.insert("toolEvents".into(), field("toolEvents"));
        next.insert("assistantReply".into(), field("assistantReply"));
        self.repository
            .storage
            .save_case_trace(&JsValue::Object(next), 30)?;
        Ok(())
    }

    pub fn get_case_trace(&self, case_id: &str) -> Result<Option<JsValue>> {
        Ok(self
            .repository
            .storage
            .get_case_trace(case_id)?
            .map(normalize_trace_preview))
    }

    pub fn list_index_traces(&self, limit: Option<usize>) -> Result<Vec<JsValue>> {
        Ok(self
            .repository
            .storage
            .list_recent_index_traces(limit.unwrap_or(30))?)
    }

    pub fn get_index_trace(&self, index_trace_id: &str) -> Result<Option<JsValue>> {
        Ok(self.repository.storage.get_index_trace(index_trace_id)?)
    }

    pub fn list_dream_traces(&self, limit: Option<usize>) -> Result<Vec<JsValue>> {
        Ok(self
            .repository
            .storage
            .list_recent_dream_traces(limit.unwrap_or(30))?)
    }

    pub fn get_dream_trace(&self, dream_trace_id: &str) -> Result<Option<JsValue>> {
        Ok(self.repository.storage.get_dream_trace(dream_trace_id)?)
    }

    pub fn export_bundle(&self) -> Result<JsValue> {
        Ok(self.repository.export_memory_bundle()?)
    }

    /// `importBundle` (`service.ts:1085-1090`): refresh indexer settings from
    /// the imported state and clear the retriever cache.
    pub fn import_bundle(&self, bundle: &JsValue) -> Result<JsValue> {
        let result = self.repository.import_memory_bundle(bundle)?;
        let settings = self.get_settings()?;
        self.indexer
            .lock()
            .expect("edgeclaw service indexer mutex poisoned")
            .set_settings(settings);
        self.retriever.reset_transient_state();
        Ok(result)
    }

    /// `clear` (`service.ts:1092-1099`): any scope other than `all_memory`
    /// clears the current workspace, and the retriever cache is dropped.
    pub fn clear(&self, scope: ClearMemoryScope) -> Result<JsValue> {
        let result = match scope {
            ClearMemoryScope::AllMemory => self.repository.clear_all_memory_data()?,
            ClearMemoryScope::CurrentProject => {
                self.repository.clear_current_workspace_memory_data()?
            }
        };
        self.retriever.reset_transient_state();
        Ok(result)
    }

    pub fn clear_session(&self, session_key: &str) -> Result<JsValue> {
        let result = self.repository.clear_session_memory_data(session_key)?;
        self.retriever.reset_transient_state();
        Ok(result)
    }

    /// `act` (`service.ts:1099-1150`).
    pub fn act(&self, input: &JsObject) -> Result<JsValue> {
        let mut messages: Vec<String> = vec![];
        let mut mutated_ids = JsValue::Array(vec![]);
        let mut deleted_project_ids = JsValue::Array(vec![]);
        let action_value = input.get("action").cloned().unwrap_or(JsValue::Undefined);
        let action = action_value.as_string();
        match action.and_then(|value| value.to_utf8().ok()).as_deref() {
            Some("edit_project_meta") => {
                // TS constructs a three-field object instead of forwarding the
                // whole action request (`service.ts:1102-1108`).
                let mut meta_input = JsObject::new();
                for key in ["projectName", "description", "status"] {
                    if let Some(value) = input.get(key) {
                        meta_input.insert(key.into(), value.clone());
                    }
                }
                let meta = self.repository.edit_project_meta(&meta_input)?;
                let relative = meta
                    .get("relativePath")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default();
                mutated_ids = JsValue::Array(vec![JsValue::String(relative)]);
                let name = meta
                    .get("projectName")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default();
                messages.push(format!(
                    "Updated current project metadata for {}.",
                    template_text(&JsValue::String(name))
                ));
            }
            Some("edit_entry") => {
                // `service.ts:1110-1118`: `{id, name, description, fields?}`.
                let mut entry_input = JsObject::new();
                for key in ["id", "name", "description"] {
                    if let Some(value) = input.get(key) {
                        entry_input.insert(key.into(), value.clone());
                    }
                }
                if let Some(fields) = input.get("fields") {
                    entry_input.insert("fields".into(), fields.clone());
                }
                let record = self.repository.edit_memory_entry(&entry_input)?;
                let relative = record
                    .get("relativePath")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default();
                mutated_ids = JsValue::Array(vec![JsValue::String(relative)]);
                let name = record
                    .get("name")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default();
                messages.push(format!(
                    "Updated memory entry {}.",
                    template_text(&JsValue::String(name))
                ));
            }
            Some("delete_entries") => {
                let result = self
                    .repository
                    .delete_memory_entries(input.get("ids").unwrap_or(&JsValue::Undefined))?;
                mutated_ids = result.get("mutatedIds").cloned().unwrap_or(mutated_ids);
                deleted_project_ids = result
                    .get("deletedProjectIds")
                    .cloned()
                    .unwrap_or(deleted_project_ids);
                let count = array_len(&mutated_ids);
                messages.push(format!(
                    "Deleted {count} memory file{}.",
                    if count == 1 { "" } else { "s" }
                ));
                let deleted = array_len(&deleted_project_ids);
                if deleted > 0 {
                    messages.push(format!(
                        "Removed {deleted} empty project{}.",
                        if deleted == 1 { "" } else { "s" }
                    ));
                }
            }
            Some("deprecate_entries") => {
                let result = self
                    .repository
                    .deprecate_memory_entries(input.get("ids").unwrap_or(&JsValue::Undefined))?;
                mutated_ids = result.get("mutatedIds").cloned().unwrap_or(mutated_ids);
                deleted_project_ids = result
                    .get("deletedProjectIds")
                    .cloned()
                    .unwrap_or(deleted_project_ids);
                let count = array_len(&mutated_ids);
                messages.push(format!(
                    "Deprecated {count} memory file{}.",
                    if count == 1 { "" } else { "s" }
                ));
            }
            Some("restore_entries") => {
                let result = self
                    .repository
                    .restore_memory_entries(input.get("ids").unwrap_or(&JsValue::Undefined))?;
                mutated_ids = result.get("mutatedIds").cloned().unwrap_or(mutated_ids);
                deleted_project_ids = result
                    .get("deletedProjectIds")
                    .cloned()
                    .unwrap_or(deleted_project_ids);
                let count = array_len(&mutated_ids);
                messages.push(format!(
                    "Restored {count} memory file{}.",
                    if count == 1 { "" } else { "s" }
                ));
            }
            _ => {}
        }
        let updated_overview = self.overview()?;
        self.retriever.reset_transient_state();
        let mut object = JsObject::new();
        object.insert("ok".into(), JsValue::Bool(true));
        object.insert("action".into(), action_value);
        object.insert("updatedOverview".into(), updated_overview);
        object.insert("mutatedIds".into(), mutated_ids);
        object.insert("deletedProjectIds".into(), deleted_project_ids);
        object.insert(
            "messages".into(),
            JsValue::Array(
                messages
                    .into_iter()
                    .map(|m| JsValue::String(m.into()))
                    .collect(),
            ),
        );
        Ok(JsValue::Object(object))
    }
}

/// `captureTurn(rawMessages, { sessionKey, timestamp?, source? })`.
pub struct CaptureTurnInput {
    pub session_key: String,
    pub timestamp: Option<String>,
    pub source: Option<String>,
}

fn resolve_configured_data_dir(
    db_path: Option<&Path>,
    memory_dir: Option<&Path>,
    workspace_dir: &Path,
    root_dir: &Path,
) -> Result<PathBuf> {
    if let Some(path) = db_path {
        return resolve_parent(path);
    }
    if let Some(path) = memory_dir {
        return resolve_parent(path);
    }
    let seed = JsString::from(workspace_dir.to_string_lossy().into_owned());
    Ok(root_dir.join("workspaces").join(hash_text(&seed)))
}

fn resolve_parent(path: &Path) -> Result<PathBuf> {
    let resolved = absolute(path)?;
    Ok(resolved
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/")))
}

/// `mergeIndexingSettings` returns JS `Math.floor`/`Math.max` results, which
/// may be non-finite. [`IndexingSettings`] stores `i64`, so `+Infinity`
/// saturates to `i64::MAX`, `-Infinity` becomes `0`, and `NaN` becomes `0`.
/// This is the storage-layer number-model difference registered in RESULT.md;
/// finite values (including `Math.floor` and the non-negative clamp) match TS.
fn storage_settings(merged: MergedIndexingSettings) -> IndexingSettings {
    IndexingSettings {
        reasoning_mode: merged.reasoning_mode.to_utf8().unwrap_or_default(),
        auto_index_interval_minutes: interval(merged.auto_index_interval_minutes),
        auto_dream_interval_minutes: interval(merged.auto_dream_interval_minutes),
    }
}

fn interval(value: f64) -> i64 {
    if value.is_nan() {
        return 0;
    }
    if value == f64::INFINITY {
        return i64::MAX;
    }
    if value <= 0.0 {
        return 0;
    }
    value.min(i64::MAX as f64) as i64
}

fn normalize_trace_preview(record: JsValue) -> JsValue {
    match record.as_object() {
        Some(object) => JsValue::Object(normalize_case_trace_context_preview(object)),
        None => record,
    }
}

fn array_len(value: &JsValue) -> usize {
    match value {
        JsValue::Array(items) => items.len(),
        _ => 0,
    }
}

/// JS template-literal `String(value)` for the case-trace seed. The record is
/// typed as strings in TS; this keeps `undefined`/`null`, numbers, booleans and
/// objects on the same `ToPrimitive` path if a caller omits a typed field.
fn template_text(value: &JsValue) -> String {
    match crate::js_value::to_string_units_checked(value) {
        Ok(units) => String::from_utf16_lossy(&units),
        Err(error) => error.message().to_string(),
    }
}
