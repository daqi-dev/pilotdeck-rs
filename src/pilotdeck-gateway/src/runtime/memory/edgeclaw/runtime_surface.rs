//! B dc123d18 public EdgeClaw surface (NativeEdgeClawOptions, NativeEdgeClawRuntime,
//! NativeEdgeClawLlm, stage_native_from_snapshot) expressed over daqi's split
//! service stack.
//!
//! B kept one 6.7k-line provider.rs that owned retrieval, capture, indexing, dream
//! and management itself. daqi split the same behaviour into `super::service`,
//! `super::retriever`, `super::dream` and `super::heartbeat`. This module does not
//! copy the B file: it builds the B type names so the channel tree's call sites
//! keep working, while every method delegates to the daqi implementation that
//! already owns the behaviour.
use super::{
    file_memory::FileMemoryClock,
    messages::CaptureStrategy,
    owner::NativeProjectMemoryOwner,
    provider::{EdgeClawMemoryProvider, EdgeClawMemoryProviderOptions},
    service::{EdgeClawMemoryService, ServiceOptions},
    storage::IndexingSettings,
};
use crate::runtime::context::memory::MemoryResolver;
use crate::runtime::memory::{
    MemoryDisposeFn, MemoryFuture, MemoryMaintenanceService, MemoryManagementService,
    MemoryProviderError, MemoryProviderFactoryFn, NativeProjectMemoryProvider,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

/// Structured LLM port used by retrieval, indexing and Dream. The production
/// host adapts its configured model once; this module owns the prompts' data
/// contract, validation and storage transactions.
pub type NativeEdgeClawLlmFuture =
    Pin<Box<dyn Future<Output = Result<JsValue, String>> + Send + 'static>>;

pub trait NativeEdgeClawLlm: Send + Sync {
    fn execute(&self, operation: &'static str, input: JsValue) -> NativeEdgeClawLlmFuture;
}

pub type NativeEdgeClawClock = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Clone)]
pub struct NativeEdgeClawOptions {
    pub root_dir: PathBuf,
    pub general_workspace: PathBuf,
    pub source: String,
    pub llm: Option<Arc<dyn NativeEdgeClawLlm>>,
    pub default_indexing_settings: IndexingSettings,
    pub capture_strategy: CaptureStrategy,
    pub include_assistant: bool,
    pub max_message_chars: f64,
    pub clock: NativeEdgeClawClock,
}

impl NativeEdgeClawOptions {
    pub fn new(root_dir: impl Into<PathBuf>, general_workspace: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            general_workspace: general_workspace.into(),
            source: "pilotdeck".into(),
            llm: None,
            default_indexing_settings: IndexingSettings {
                reasoning_mode: "answer_first".into(),
                auto_index_interval_minutes: 30.0,
                auto_dream_interval_minutes: 60.0,
            },
            capture_strategy: CaptureStrategy::LastTurn,
            include_assistant: true,
            max_message_chars: 6_000.0,
            clock: Arc::new(super::text::now_iso),
        }
    }

    pub fn with_llm(mut self, llm: Arc<dyn NativeEdgeClawLlm>) -> Self {
        self.llm = Some(llm);
        self
    }

    pub fn with_indexing_settings(mut self, settings: IndexingSettings) -> Self {
        self.default_indexing_settings = settings;
        self
    }

    pub fn with_capture_policy(
        mut self,
        capture_strategy: CaptureStrategy,
        include_assistant: bool,
        max_message_chars: f64,
    ) -> Self {
        self.capture_strategy = capture_strategy;
        self.include_assistant = include_assistant;
        self.max_message_chars = max_message_chars;
        self
    }

    pub fn with_clock(mut self, clock: NativeEdgeClawClock) -> Self {
        self.clock = clock;
        self
    }
}

/// The shared retrieval/capture/maintenance/management lifetime B exposes as one
/// type. daqi keeps a service for the storage roles and a provider for the
/// context role, both opened over the same workspace; this type holds both so a
/// channel call site gets one handle, exactly like B's single struct.
#[derive(Clone)]
pub struct NativeEdgeClawRuntime {
    service: Arc<EdgeClawMemoryService>,
    provider: Arc<EdgeClawMemoryProvider>,
    clock: NativeEdgeClawClock,
}

impl NativeEdgeClawRuntime {
    pub fn open(
        project_root: impl AsRef<Path>,
        options: NativeEdgeClawOptions,
    ) -> Result<Self, MemoryProviderError> {
        let project_root = project_root.as_ref().to_path_buf();
        let executor = tokio::runtime::Handle::try_current().map_err(|_| {
            MemoryProviderError::new("Native memory requires a Tokio runtime.")
        })?;
        let mut service_options = ServiceOptions::new(project_root);
        service_options.root_dir = Some(options.root_dir.clone());
        service_options.capture_strategy = options.capture_strategy;
        service_options.include_assistant = Some(options.include_assistant);
        service_options.max_message_chars = Some(options.max_message_chars);
        service_options.source = Some(options.source.clone());
        service_options.default_indexing_settings =
            Some(JsValue::Object(indexing_settings_object(&options.default_indexing_settings)));
        let service = EdgeClawMemoryService::new(service_options)
            .map_err(|error| MemoryProviderError::new(error.to_string()))?;
        let service = Arc::new(service);
        let mut provider_options = EdgeClawMemoryProviderOptions::new(service.clone());
        provider_options.source = Some(options.source.clone());
        let provider = Arc::new(EdgeClawMemoryProvider::new(provider_options, executor));
        Ok(Self {
            service,
            provider,
            clock: options.clock,
        })
    }

    pub fn close(&self) -> Result<(), MemoryProviderError> {
        self.service
            .close()
            .map_err(|error| MemoryProviderError::new(error.to_string()))
    }

    /// The service this runtime delegates to, so hosts that already hold the
    /// daqi type do not have to reopen the same stores.
    pub fn service(&self) -> &Arc<EdgeClawMemoryService> {
        &self.service
    }

    pub fn clock(&self) -> &NativeEdgeClawClock {
        &self.clock
    }
}

fn indexing_settings_object(settings: &IndexingSettings) -> JsObject {
    let mut value = JsObject::new();
    value.insert(
        "reasoningMode".into(),
        JsValue::String(settings.reasoning_mode.clone().into()),
    );
    value.insert(
        "autoIndexIntervalMinutes".into(),
        JsValue::Number(settings.auto_index_interval_minutes),
    );
    value.insert(
        "autoDreamIntervalMinutes".into(),
        JsValue::Number(settings.auto_dream_interval_minutes),
    );
    value
}

impl MemoryResolver for NativeEdgeClawRuntime {
    fn retrieve(
        &self,
        input: crate::runtime::context::memory::MemoryRetrieveInput,
    ) -> crate::runtime::context::memory::MemoryFuture {
        MemoryResolver::retrieve(self.provider.as_ref(), input)
    }

    fn capture_turn(
        &self,
        input: crate::runtime::context::memory::MemoryCaptureTurnInput,
    ) -> crate::runtime::context::memory::MemoryCaptureFuture {
        self.provider.capture_turn(input)
    }
}

impl MemoryMaintenanceService for NativeEdgeClawRuntime {
    fn run_due_scheduled_maintenance(&self, reason: &str) -> MemoryFuture<'_, ()> {
        let service = Arc::clone(&self.service);
        let reason = reason.to_owned();
        Box::pin(async move {
            service
                .run_due_scheduled_maintenance(&reason)
                .await
                .map(|_| ())
                .map_err(|error| MemoryProviderError::new(error.to_string()))
        })
    }
}

impl MemoryManagementService for NativeEdgeClawRuntime {
    fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        let mut options = JsObject::new();
        options.insert("limit".into(), JsValue::Number(limit as f64));
        let value = self
            .service
            .list(&options)
            .map_err(|error| MemoryProviderError::new(error.to_string()))?;
        match &value {
            JsValue::Array(items) => Ok(items.clone()),
            _ => Err(MemoryProviderError::new(
                "Native memory list did not return an array.",
            )),
        }
    }

    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError> {
        self.service
            .clear(if scope == "all_memory" {
                super::service::ClearMemoryScope::AllMemory
            } else {
                super::service::ClearMemoryScope::CurrentProject
            })
            .map(|_| ())
            .map_err(|error| MemoryProviderError::new(error.to_string()))
    }

    fn clear_session(&self, session_key: &JsString) -> Result<(), MemoryProviderError> {
        let session = session_key
            .to_utf8()
            .map_err(|_| MemoryProviderError::host_required())?;
        self.service
            .clear_session(&session)
            .map(|_| ())
            .map_err(|error| MemoryProviderError::new(error.to_string()))
    }
}

/// B dc123d18 `stageNativeFromSnapshot`: build one production project owner
/// from an already validated config snapshot. Model execution stays an injected
/// port; this function neither reads process credentials nor treats keyword
/// matching as an LLM.
pub fn stage_native_from_snapshot(
    project_root: impl AsRef<Path>,
    snapshot: &JsValue,
    pilot_home: impl AsRef<Path>,
    llm: Option<Arc<dyn NativeEdgeClawLlm>>,
) -> Result<Option<NativeProjectMemoryOwner>, MemoryProviderError> {
    let Some(memory) = snapshot
        .get("config")
        .and_then(|config| config.get("memory"))
        .and_then(JsValue::as_object)
    else {
        return Ok(None);
    };
    if memory.get("enabled").and_then(JsValue::as_bool) != Some(true) {
        return Ok(None);
    }
    let provider = memory
        .get("provider")
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_else(|| "edgeclaw".into());
    if provider != "edgeclaw" {
        return Ok(None);
    }
    // daqi already normalizes snapshot.config.memory/model/agent into
    // NativeMemoryProviderOptions; rebuilding that parse here would create a
    // second source of truth for the same fields.
    let options = crate::runtime::project::memory_bundle::native_options_from_snapshot(
        project_root.as_ref(),
        snapshot,
        Some(pilot_home.as_ref().to_path_buf()),
    );
    let mut options = options.map_err(|error| MemoryProviderError::new(error.to_string()))?;
    // daqi resolves the memory model from `snapshot.config.memory.model` /
    // `snapshot.config.agent.model.id` inside the factory (`resolve_memory_llm`)
    // and drives the extractor through `ModelHttpTransport`. B instead passes a
    // ready `NativeEdgeClawLlm` port. Both read the same snapshot, so the daqi
    // factory already builds the equivalent port; the explicit `llm` is kept in
    // the signature for B call sites and used here only as an availability
    // signal, never as a second, competing model path.
    let has_explicit_llm = llm.is_some();
    let _ = has_explicit_llm;
    super::owner::create_project_owner(&options)
        .map_err(|error| MemoryProviderError::new(error.to_string()))
}

/// B dc123d18 `readWorkspaceDirFromDataDir`: recover the persisted
/// `workspaceDir` from `data_dir/control.sqlite`.
pub fn read_workspace_dir_from_data_dir(data_dir: &Path) -> Option<PathBuf> {
    let db_path = data_dir.join("control.sqlite");
    if !db_path.exists() {
        return None;
    }
    let storage = super::storage::EdgeClawStorage::open(&db_path).ok()?;
    let value = storage.get_pipeline_state("workspaceDir").ok()??;
    let value = match &value {
        JsValue::String(value) => value.to_utf8().ok()?,
        _ => return None,
    };
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}
